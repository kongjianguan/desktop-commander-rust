use anyhow::{Result, Context, ensure};
use serde_json::{Value, json};
use std::{collections::{HashMap, VecDeque}, sync::Arc, time::{Duration, Instant}};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWriteExt}, sync::{Mutex, Notify}};
use tokio_util::sync::CancellationToken;

const CAPACITY: usize = 256 * 1024;

pub struct Session {
    pid: u32,
    command: String,
    started: Instant,
    started_at:i64,
    events:Mutex<VecDeque<Value>>,
    sequence:std::sync::atomic::AtomicU64,
    first_output:std::sync::atomic::AtomicI64,
    last_output:std::sync::atomic::AtomicI64,
    output: Mutex<VecDeque<u8>>,
    stdin: Mutex<Option<tokio::process::ChildStdin>>,
    exit: Mutex<Option<i32>>,
    changed: Notify,
    cancel: CancellationToken,
    cursor: Mutex<usize>,
    discarded: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
pub struct Terminal { sessions: Mutex<HashMap<u32, Arc<Session>>> }

async fn pump(mut input: impl AsyncRead + Unpin, session: Arc<Session>,source:&'static str) -> Result<()> {
    let mut chunk = [0u8; 8192];
    loop {
        let size = input.read(&mut chunk).await?;
        if size == 0 { break; }
        let mut output = session.output.lock().await;
        let evict=(output.len()+size).saturating_sub(CAPACITY);
        if evict>0 {
            output.drain(..evict);
            session.discarded.fetch_add(evict,std::sync::atomic::Ordering::Relaxed);
        }
        output.extend(&chunk[..size]);
        drop(output);
        let now=session.started_at+session.started.elapsed().as_millis() as i64;
        let _=session.first_output.compare_exchange(-1,now,std::sync::atomic::Ordering::Relaxed,std::sync::atomic::Ordering::Relaxed);
        session.last_output.store(now,std::sync::atomic::Ordering::Relaxed);
        let mut events=session.events.lock().await;
        let previous=events.back().and_then(|event|event["timestamp"].as_i64()).unwrap_or(now);
        events.push_back(json!({"timestamp":now,"deltaMs":now-previous,"source":source,"length":size,
            "snippet":String::from_utf8_lossy(&chunk[..size]).chars().take(80).collect::<String>()}));
        while events.len()>64 {events.pop_front();}
        drop(events);
        session.sequence.fetch_add(1,std::sync::atomic::Ordering::Release);
        session.changed.notify_waiters();
    }
    Ok(())
}

impl Terminal {
    pub async fn start(&self, command: &str, shell: &str, timeout: u64,verbose:bool) -> Result<Value> {
        let mut sessions = self.sessions.lock().await;
        let mut expired = Vec::new();
        for (&pid, session) in sessions.iter() {
            if session.exit.lock().await.is_some() && session.started.elapsed() > Duration::from_secs(300) { expired.push(pid); }
        }
        for pid in expired { sessions.remove(&pid); }
        if sessions.len()>=16 {
            let mut oldest=None;
            for (&pid,session) in sessions.iter() {
                if session.exit.lock().await.is_some() && oldest.is_none_or(|(_,started)|session.started<started) {oldest=Some((pid,session.started));}
            }
            if let Some((pid,_))=oldest {sessions.remove(&pid);}
        }
        ensure!(sessions.len() < 16, "Terminal session limit reached (16)");
        let mut builder = tokio::process::Command::new(shell);
        let name = std::path::Path::new(shell).file_name().context("Shell executable is missing")?.to_string_lossy().to_lowercase();
        match name.as_str() {
            "cmd.exe"|"cmd" => {builder.args(["/D","/S","/C",command]);}
            "powershell.exe"|"powershell"|"pwsh.exe"|"pwsh" => {builder.args(["-NoLogo","-NoProfile","-Command",command]);}
            _ => {builder.args(["-c",command]);}
        }
        builder.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()).kill_on_drop(true);
        #[cfg(unix)] { builder.process_group(0); }
        #[cfg(windows)] { builder.creation_flags(0x08000000); }
        let mut child = builder.spawn().context("Unable to start shell")?;
        let pid = child.id().context("Process has no PID")?;
        let session = Arc::new(Session { pid, command: command.into(), started: Instant::now(),
            started_at:chrono::Utc::now().timestamp_millis(),events:Mutex::new(VecDeque::new()),sequence:std::sync::atomic::AtomicU64::new(0),
            first_output:std::sync::atomic::AtomicI64::new(-1),last_output:std::sync::atomic::AtomicI64::new(-1),
            output: Mutex::new(VecDeque::new()), stdin: Mutex::new(child.stdin.take()), exit: Mutex::new(None),
            changed: Notify::new(), cancel: CancellationToken::new(), cursor: Mutex::new(0),
            discarded: std::sync::atomic::AtomicUsize::new(0) });
        let stdout = child.stdout.take().context("stdout missing")?;
        let stderr = child.stderr.take().context("stderr missing")?;
        let active = session.clone();
        tokio::spawn(async move {
            let out = tokio::spawn(pump(stdout, active.clone(),"stdout"));
            let err = tokio::spawn(pump(stderr, active.clone(),"stderr"));
            let status = tokio::select! {
                result = child.wait() => result,
                _ = active.cancel.cancelled() => {
                    if let Err(error) = crate::platform::kill_tree(pid) { tracing::error!("{error:#}"); }
                    child.wait().await
                }
            };
            for task in [out, err] {
                match task.await { Ok(Ok(())) => {}, other => tracing::error!("Process output failed: {other:?}") }
            }
            *active.exit.lock().await = Some(match status { Ok(status) => status.code().unwrap_or(-1), Err(error) => {tracing::error!("{error}"); -1} });
            active.stdin.lock().await.take(); active.changed.notify_waiters();
        });
        sessions.insert(pid, session.clone());
        drop(sessions);
        self.wait(&session, timeout.min(30_000),Some(0)).await;
        self.read(pid, 0, None, 1000,verbose).await
    }

    async fn get(&self, pid: u32) -> Result<Arc<Session>> {
        self.sessions.lock().await.get(&pid).cloned().context("Unknown terminal session")
    }

    async fn wait(&self, session: &Session, timeout: u64,prompt_after:Option<u64>) {
        let deadline = Instant::now() + Duration::from_millis(timeout);
        while Instant::now() < deadline && session.exit.lock().await.is_none() {
            if prompt_after.is_some_and(|sequence|session.sequence.load(std::sync::atomic::Ordering::Acquire)>sequence) && has_prompt(&*session.output.lock().await) {break;}
            let delay = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(100));
            tokio::select! { _ = session.changed.notified() => {}, _ = tokio::time::sleep(delay) => {} }
        }
    }

    pub async fn read(&self, pid: u32, timeout: u64, offset: Option<i64>, length: usize,verbose:bool) -> Result<Value> {
        let session = self.get(pid).await?;
        if timeout > 0 {
            let after={
                let output=session.output.lock().await;
                let cursor=session.cursor.lock().await;
                if session.discarded.load(std::sync::atomic::Ordering::Relaxed)+output.len()>*cursor {0}
                    else {session.sequence.load(std::sync::atomic::Ordering::Acquire)}
            };
            self.wait(&session, timeout.min(30_000),Some(after)).await;
        }
        let output = session.output.lock().await;
        let bytes: Vec<_> = output.iter().copied().collect();
        let mut cursor = session.cursor.lock().await;
        let discarded = session.discarded.load(std::sync::atomic::Ordering::Relaxed);
        let implicit = offset.is_none() || offset == Some(0);
        let begin = if implicit {cursor.saturating_sub(discarded).min(bytes.len())} else {0};
        let remaining=&bytes[begin..];
        let exit = *session.exit.lock().await;
        let valid=match std::str::from_utf8(remaining) {Err(error) if exit.is_none() && error.error_len().is_none()=>error.valid_up_to(),_=>remaining.len()};
        let text = String::from_utf8_lossy(&remaining[..valid]);
        let lines: Vec<_> = text.split_inclusive('\n').collect();
        let start = match offset {
            Some(n) if n < 0 => lines.len().saturating_sub(n.unsigned_abs() as usize),
            Some(n) if n > 0 => n as usize,
            _ => 0,
        };
        let end = start.saturating_add(length.min(10_000)).min(lines.len());
        let content = if start >= lines.len() { String::new() } else { lines[start..end].concat() };
        let consumed = if end == lines.len() {valid} else {lines[..end].iter().map(|line|line.len()).sum()};
        *cursor = discarded+begin+consumed;
        let blocked=exit.is_none()&&has_prompt(&output);
        let mut data=json!({"pid": pid, "output": content, "isBlocked":blocked,"isRunning":exit.is_none(), "exitCode": exit,
            "runtimeMs": session.started.elapsed().as_millis(), "totalLines": bytes.iter().filter(|&&c|c == b'\n').count()+usize::from(bytes.last().is_some_and(|&c|c != b'\n')),
            "discardedBytes": discarded});
        if verbose {
            let events=session.events.lock().await;
            let elapsed=session.started.elapsed().as_millis() as i64;
            let first=session.first_output.load(std::sync::atomic::Ordering::Relaxed);
            let first=if first<0 {None}else{Some(first)};
            let last=session.last_output.load(std::sync::atomic::Ordering::Relaxed);
            let last=if last<0 {None}else{Some(last)};
            data["timingInfo"]=json!({"startTime":session.started_at,"endTime":session.started_at+elapsed,"totalDurationMs":elapsed,
                "exitReason":if exit.is_some(){"process_exit"}else if blocked{"early_exit_quick_pattern"}else{"timeout"},
                "firstOutputTime":first,"lastOutputTime":last,"timeToFirstOutputMs":first.map(|time|time-session.started_at),
                "outputEvents":*events,"outputEventsTruncated":session.sequence.load(std::sync::atomic::Ordering::Acquire)>64});
        }
        Ok(data)
    }

    pub async fn interact(&self, pid: u32, input: &str, timeout: u64,wait:bool,verbose:bool) -> Result<Value> {
        let session = self.get(pid).await?;
        let mut stdin = session.stdin.lock().await;
        let writer = stdin.as_mut().context("Process stdin is closed")?;
        writer.write_all(input.as_bytes()).await?; writer.write_all(b"\n").await?; writer.flush().await?;
        drop(stdin);
        self.read(pid, if wait{timeout}else{0}, None, 1000,verbose).await
    }

    pub async fn terminate(&self, pid: u32) -> Result<Value> {
        let session = self.get(pid).await?;
        session.cancel.cancel(); self.wait(&session, 5000,None).await;
        ensure!(session.exit.lock().await.is_some(), "Process did not terminate");
        Ok(json!({"pid": pid, "terminated": true}))
    }

    pub async fn list(&self) -> Value {
        let sessions = self.sessions.lock().await;
        let mut result = Vec::new();
        for session in sessions.values() {
            result.push(json!({"pid": session.pid, "command": session.command,
                "runtime": session.started.elapsed().as_millis(), "exitCode": *session.exit.lock().await}));
        }
        json!(result)
    }

    pub async fn shutdown(&self) -> Result<()> {
        let pids: Vec<_> = self.sessions.lock().await.keys().copied().collect();
        for pid in pids { self.terminate(pid).await?; }
        Ok(())
    }
}

fn has_prompt(output:&VecDeque<u8>)->bool {
    [b">>> ".as_slice(),b"... ",b"> ",b"$ ",b"# ",b"% "].iter().any(|marker|
        output.len()>=marker.len()&&output.iter().rev().take(marker.len()).copied().eq(marker.iter().rev().copied()))
}
