use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::VecDeque, fs::{self, File, OpenOptions}, path::{Path, PathBuf}, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub server_url: String,
    pub proxy: String,
    pub no_proxy: String,
    pub device_name: String,
    pub auto_connect: bool,
    pub prevent_sleep: bool,
    pub start_at_login: bool,
    pub tools: Value,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: "https://mcp.desktopcommander.app".into(), proxy: String::new(),
            no_proxy: "localhost,127.0.0.1,::1".into(),
            device_name: sysinfo::System::host_name().unwrap_or_else(|| "Desktop Commander".into()),
            auto_connect: true, prevent_sleep: false, start_at_login: false,
            tools: json!({"blockedCommands": [], "defaultShell": if cfg!(windows) {"powershell.exe"} else {"/bin/sh"},
                "allowedDirectories": [], "fileReadLineLimit": 1000, "fileWriteLineLimit": 1000,
                "telemetryEnabled": false, "defaultEditorName": "", "defaultEditorPath": ""}),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub device_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: String,
    pub detail: String,
    pub device_id: String,
    pub verification_uri: String,
    pub user_code: String,
    pub updated_at: String,
}

pub struct App {
    pub dir: PathBuf,
    pub settings: RwLock<Settings>,
    pub session: RwLock<Option<Session>>,
    pub status: RwLock<Status>,
    pub remote: Mutex<Option<(CancellationToken, tokio::task::JoinHandle<Result<()>>)>>,
    pub events: Mutex<VecDeque<Value>>,
    pub history_lock: Mutex<()>,
    pub shutdown: CancellationToken,
    pub tools: crate::tools::Tools,
    pub gui: Mutex<Option<tokio::process::Child>>,
    pub token: String,
    pub endpoint: RwLock<String>,
    pub slots: tokio::sync::Semaphore,
    _lock: File,
}

pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)] {
        let user = std::env::var("USERNAME")?;
        let status = std::process::Command::new("icacls").arg(path)
            .args(["/inheritance:r", "/grant:r", &format!("{user}:(OI)(CI)F")]).output()?;
        anyhow::ensure!(status.status.success(), "Unable to restrict configuration directory ACL");
    }
    Ok(())
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let bytes = serde_json::to_vec(value)?;
    atomicwrites::AtomicFile::new(path,atomicwrites::OverwriteBehavior::AllowOverwrite).write(|file| {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()
    })?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(path,fs::Permissions::from_mode(0o600))?; }
    Ok(())
}

impl App {
    pub fn load(dir: PathBuf) -> Result<Arc<Self>> {
        private_dir(&dir)?;
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("instance.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).context("Another instance owns this configuration directory")?;
        let read = |name: &str| -> Result<Option<Value>> {
            let path = dir.join(name);
            if !path.exists() { return Ok(None); }
            Ok(Some(serde_json::from_reader(File::open(path)?)?))
        };
        let settings = read("settings.json")?.map(serde_json::from_value).transpose()?.unwrap_or_default();
        let session = read("session.json")?.map(serde_json::from_value).transpose()?;
        let app = Arc::new(Self {
            tools: crate::tools::Tools::new(&dir)?, dir, settings: RwLock::new(settings), session: RwLock::new(session),
            status: RwLock::new(Status { state: "stopped".into(), detail: String::new(), device_id: String::new(),
                verification_uri: String::new(), user_code: String::new(), updated_at: Utc::now().to_rfc3339() }),
            remote: Mutex::new(None), events: Mutex::new(VecDeque::new()), history_lock: Mutex::new(()),
            shutdown: CancellationToken::new(), gui: Mutex::new(None), token: uuid::Uuid::new_v4().to_string(),
            endpoint: RwLock::new(String::new()), slots: tokio::sync::Semaphore::new(4), _lock: lock,
        });
        Ok(app)
    }

    pub async fn state(&self, state: &str, detail: impl Into<String>) {
        let mut status = self.status.write().await;
        status.state = state.into(); status.detail = detail.into(); status.updated_at = Utc::now().to_rfc3339();
        tracing::info!(state, detail = %status.detail);
    }

    pub async fn event(&self, event: Value) {
        let mut events = self.events.lock().await;
        events.push_back(event);
        while events.len() > 100 { events.pop_front(); }
    }

    pub async fn save_session(&self, session: Session) -> Result<()> {
        let mut guard = self.session.write().await;
        atomic_json(&self.dir.join("session.json"), &session)?;
        *guard = Some(session);
        Ok(())
    }

    pub async fn start(self: &Arc<Self>) {
        let mut remote = self.remote.lock().await;
        if remote.as_ref().is_some_and(|(_, handle)| !handle.is_finished()) { return; }
        let cancel = self.shutdown.child_token();
        let app = self.clone();
        let task_cancel = cancel.clone();
        *remote = Some((cancel, tokio::spawn(async move {
            let result = crate::remote::run(app.clone(), task_cancel).await;
            if let Err(error) = &result {
                app.state("error", format!("{error:#}")).await;
            }
            result
        })));
    }

    pub async fn stop(&self) -> Result<()> {
        if let Some((cancel, task)) = self.remote.lock().await.take() {
            cancel.cancel(); task.await??;
        }
        self.state("stopped", "远程连接已停止").await;
        Ok(())
    }

    pub async fn open_gui(&self) -> Result<()> {
        let mut slot = self.gui.lock().await;
        if let Some(child) = slot.as_mut() { if child.try_wait()?.is_none() { return Ok(()); } }
        let executable = crate::platform::gui_executable()?;
        *slot = Some(tokio::process::Command::new(executable)
            .env("DC_DESCRIPTOR", self.dir.join("control.json"))
            .kill_on_drop(true).spawn()?);
        Ok(())
    }

    pub async fn set_settings(&self, settings: Settings) -> Result<()> {
        let url = reqwest::Url::parse(&settings.server_url)?;
        if url.scheme() != "https" { bail!("Server URL must use HTTPS"); }
        if !settings.proxy.is_empty() { reqwest::Proxy::all(&settings.proxy)?; }
        if settings.start_at_login != self.settings.read().await.start_at_login {
            crate::platform::autostart(settings.start_at_login, &self.dir)?;
        }
        atomic_json(&self.dir.join("settings.json"), &settings)?;
        *self.settings.write().await = settings;
        Ok(())
    }
}
