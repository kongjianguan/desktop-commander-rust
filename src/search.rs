use anyhow::{Result, Context, ensure};
use serde_json::{Value, json};
use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}, time::{Duration, Instant}};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, sync::Mutex};
use tokio_util::sync::CancellationToken;

struct Search {
    path: PathBuf,
    count: AtomicUsize,
    complete: AtomicBool,
    error: Mutex<Option<String>>,
    cancel: CancellationToken,
    started: Instant,
}
pub struct Searches { dir: PathBuf, sessions: Mutex<HashMap<String, Arc<Search>>> }

impl Searches {
    pub fn new(dir: &Path) -> Result<Self> {
        let dir = dir.join("searches"); crate::app::private_dir(&dir)?;
        for entry in std::fs::read_dir(&dir)? { let path = entry?.path(); if path.is_file() { std::fs::remove_file(path)?; } }
        Ok(Self { dir, sessions: Mutex::new(HashMap::new()) })
    }
    pub async fn start(&self, args: &Value, path: &Path) -> Result<Value> {
        let mut sessions = self.sessions.lock().await;
        let expired: Vec<_> = sessions.iter().filter(|(_, s)| s.complete.load(Ordering::Acquire) && s.started.elapsed() > Duration::from_secs(300)).map(|(id, _)| id.clone()).collect();
        for id in expired { let old = sessions.remove(&id).unwrap(); tokio::fs::remove_file(&old.path).await?; }
        ensure!(sessions.len() < 16, "Search session limit reached (16)");
        let id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(Search { path: self.dir.join(format!("{id}.jsonl")), count: AtomicUsize::new(0),
            complete: AtomicBool::new(false), error: Mutex::new(None), cancel: CancellationToken::new(), started: Instant::now() });
        let files = args["searchType"].as_str().unwrap_or("files") == "files";
        let pattern = args["pattern"].as_str().context("pattern missing")?;
        let mut cmd = tokio::process::Command::new(crate::platform::bundled("rg")?);
        if files {
            cmd.arg("--files");
            let glob = if pattern.contains(['*', '?', '[']) { pattern.to_owned() } else { format!("*{pattern}*") };
            cmd.args([if args["ignoreCase"].as_bool().unwrap_or(true) {"--iglob"} else {"--glob"}, &glob]);
        } else {
            cmd.args(["--json", "--max-columns", "65536"]);
            if args["ignoreCase"].as_bool().unwrap_or(true) { cmd.arg("-i"); }
            if args["literalSearch"].as_bool().unwrap_or(false) { cmd.arg("-F"); }
            cmd.args(["--context", &args["contextLines"].as_u64().unwrap_or(5).min(100).to_string()]);
            cmd.arg("-e").arg(pattern);
        }
        if args["includeHidden"].as_bool().unwrap_or(false) { cmd.arg("--hidden"); }
        if let Some(glob) = args["filePattern"].as_str() { cmd.arg("--glob").arg(glob); }
        cmd.arg("--").arg(path).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().context("Unable to start bundled ripgrep")?;
        let stdout = child.stdout.take().context("Search stdout missing")?;
        let stderr = child.stderr.take().context("Search stderr missing")?;
        let active = session.clone();
        let maximum = args["maxResults"].as_u64().unwrap_or(10_000).min(100_000) as usize;
        let mut file = tokio::fs::File::create(&session.path).await?;
        tokio::spawn(async move {
            let result: Result<()> = async {
                let mut lines = BufReader::new(stdout).lines();
                let stderr_task = tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut bytes = Vec::new(); stderr.take(65536).read_to_end(&mut bytes).await.map(|_| bytes)
                });
                let mut capped = false;
                loop {
                    let line = tokio::select! { _ = active.cancel.cancelled() => { capped = true; break; }, line = lines.next_line() => line? };
                    let Some(line) = line else { break; };
                    let value = if files { json!({"type": "file", "file": line}) } else {
                        let record: Value = serde_json::from_str(&line)?;
                        if record["type"] != "match" && record["type"] != "context" { continue; }
                        json!({"type": record["type"], "file": record["data"]["path"]["text"],
                            "line": record["data"]["line_number"], "text": record["data"]["lines"]["text"]})
                    };
                    file.write_all(serde_json::to_string(&value)?.as_bytes()).await?; file.write_all(b"\n").await?;
                    file.flush().await?;
                    if active.count.fetch_add(1, Ordering::Release) + 1 >= maximum { capped = true; break; }
                }
                if capped { child.kill().await?; }
                let status = child.wait().await?;
                let stderr = stderr_task.await??;
                ensure!(capped || status.success() || status.code() == Some(1), "ripgrep failed: {}", String::from_utf8_lossy(&stderr));
                Ok(())
            }.await;
            if let Err(error) = result { *active.error.lock().await = Some(format!("{error:#}")); }
            active.complete.store(true, Ordering::Release);
        });
        sessions.insert(id.clone(), session); drop(sessions);
        tokio::time::sleep(Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(100).min(5000))).await;
        self.more(&id, 0, 100).await
    }
    pub async fn more(&self, id: &str, offset: usize, length: usize) -> Result<Value> {
        let session = self.sessions.lock().await.get(id).cloned().context("Unknown search session")?;
        if let Some(error) = session.error.lock().await.as_ref() { anyhow::bail!("{error}"); }
        let mut lines = BufReader::new(tokio::fs::File::open(&session.path).await?).lines();
        let mut result = Vec::new(); let mut index = 0;
        while let Some(line) = lines.next_line().await? {
            if index >= offset { result.push(serde_json::from_str::<Value>(&line)?); }
            index += 1;
            if result.len() >= length.min(1000) { break; }
        }
        Ok(json!({"sessionId": id, "results": result, "offset": offset, "totalResults": session.count.load(Ordering::Acquire),
            "isComplete": session.complete.load(Ordering::Acquire)}))
    }
    pub async fn stop(&self, id: &str) -> Result<Value> {
        let session = self.sessions.lock().await.get(id).cloned().context("Unknown search session")?;
        session.cancel.cancel(); Ok(json!({"sessionId": id, "stopped": true}))
    }
    pub async fn list(&self) -> Value {
        json!(self.sessions.lock().await.iter().map(|(id, s)| json!({"sessionId":id,
            "isComplete":s.complete.load(Ordering::Acquire), "totalResults":s.count.load(Ordering::Acquire)})).collect::<Vec<_>>())
    }
}
