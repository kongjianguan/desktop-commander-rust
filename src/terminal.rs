use anyhow::{Result, Context, ensure};
use serde_json::{Value, json};
use std::{collections::{HashMap, VecDeque}, sync::Arc, time::{Duration, Instant}};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWriteExt}, sync::{Mutex, Notify}};
use tokio_util::sync::CancellationToken;

const CAPACITY: usize = 1024 * 1024;

pub struct Session {
    pid: u32,
    command: String,
    started: Instant,
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

async fn pump(mut input: impl AsyncRead + Unpin, session: Arc<Session>) -> Result<()> {
    let mut chunk = [0u8; 8192];
    loop {
        let size = input.read(&mut chunk).await?;
        if size == 0 { break; }
        let mut output = session.output.lock().await;
        output.extend(&chunk[..size]);
        while output.len() > CAPACITY {
            output.pop_front();
            session.discarded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        drop(output);
        session.changed.notify_waiters();
    }
    Ok(())
}

impl Terminal {
    pub async fn start(&self, command: &str, shell: &str, timeout: u64) -> Result<Value> {
        let mut sessions = self.sessions.lock().await;
        let mut expired = Vec::new();
        for (&pid, session) in sessions.iter() {
            if session.exit.lock().await.is_some() && session.started.elapsed() > Duration::from_secs(300) { expired.push(pid); }
        }
        for pid in expired { sessions.remove(&pid); }
        ensure!(sessions.len() < 16, "Terminal session limit reached (16)");
        let mut builder = tokio::process::Command::new(shell);
        if cfg!(windows) { builder.args(["-NoLogo", "-NoProfile", "-Command", command]); }
        else { builder.args(["-c", command]); }
        builder.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()).kill_on_drop(true);
        #[cfg(unix)] { builder.process_group(0); }
        #[cfg(windows)] { builder.creation_flags(0x08000000); }
        let mut child = builder.spawn().context("Unable to start shell")?;
        let pid = child.id().context("Process has no PID")?;
        let session = Arc::new(Session { pid, command: command.into(), started: Instant::now(),
            output: Mutex::new(VecDeque::new()), stdin: Mutex::new(child.stdin.take()), exit: Mutex::new(None),
            changed: Notify::new(), cancel: CancellationToken::new(), cursor: Mutex::new(0),
            discarded: std::sync::atomic::AtomicUsize::new(0) });
        let stdout = child.stdout.take().context("stdout missing")?;
        let stderr = child.stderr.take().context("stderr missing")?;
        let active = session.clone();
        tokio::spawn(async move {
            let out = tokio::spawn(pump(stdout, active.clone()));
            let err = tokio::spawn(pump(stderr, active.clone()));
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
        self.wait(&session, timeout.min(30_000)).await;
        self.read(pid, 0, None, 1000).await
    }

    async fn get(&self, pid: u32) -> Result<Arc<Session>> {
        self.sessions.lock().await.get(&pid).cloned().context("Unknown terminal session")
    }

    async fn wait(&self, session: &Session, timeout: u64) {
        let deadline = Instant::now() + Duration::from_millis(timeout);
        while Instant::now() < deadline && session.exit.lock().await.is_none() {
            let delay = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(100));
            tokio::select! { _ = session.changed.notified() => {}, _ = tokio::time::sleep(delay) => {} }
        }
    }

    pub async fn read(&self, pid: u32, timeout: u64, offset: Option<i64>, length: usize) -> Result<Value> {
        let session = self.get(pid).await?;
        if timeout > 0 { self.wait(&session, timeout.min(30_000)).await; }
        let output = session.output.lock().await;
        let bytes: Vec<_> = output.iter().copied().collect();
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<_> = text.lines().collect();
        let mut cursor = session.cursor.lock().await;
        let start = match offset {
            Some(n) if n < 0 => lines.len().saturating_sub(n.unsigned_abs() as usize),
            Some(n) if n > 0 => n as usize,
            _ => (*cursor).min(lines.len()),
        };
        let end = start.saturating_add(length.min(10_000)).min(lines.len());
        let content = if start >= lines.len() { String::new() } else { lines[start..end].join("\n") };
        *cursor = end;
        let exit = *session.exit.lock().await;
        Ok(json!({"pid": pid, "output": content, "isBlocked": exit.is_none(), "exitCode": exit,
            "runtimeMs": session.started.elapsed().as_millis(), "totalLines": lines.len(),
            "discardedBytes": session.discarded.load(std::sync::atomic::Ordering::Relaxed)}))
    }

    pub async fn interact(&self, pid: u32, input: &str, timeout: u64) -> Result<Value> {
        let session = self.get(pid).await?;
        let mut stdin = session.stdin.lock().await;
        let writer = stdin.as_mut().context("Process stdin is closed")?;
        writer.write_all(input.as_bytes()).await?; writer.write_all(b"\n").await?; writer.flush().await?;
        drop(stdin);
        self.read(pid, timeout, None, 1000).await
    }

    pub async fn terminate(&self, pid: u32) -> Result<Value> {
        let session = self.get(pid).await?;
        session.cancel.cancel(); self.wait(&session, 5000).await;
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
