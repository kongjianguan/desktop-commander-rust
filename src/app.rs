use anyhow::{Context, Result};
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
            tools: json!({"blockedCommands": [], "defaultShell": if cfg!(windows) {"powershell.exe".to_string()} else if cfg!(target_os="android") {
                std::env::var("PREFIX").ok().map(|prefix|format!("{prefix}/bin/sh")).filter(|shell|Path::new(shell).is_file())
                    .unwrap_or_else(||"/system/bin/sh".to_string())
            } else {"/bin/sh".to_string()},
                "allowedDirectories": [], "fileReadLineLimit": 1000, "fileWriteLineLimit": 1000,
                "telemetryEnabled": false, "defaultEditorName": "", "defaultEditorPath": ""}),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.server_url)?;
        anyhow::ensure!(url.scheme() == "https","Server URL must use HTTPS");
        if !self.proxy.is_empty() {reqwest::Proxy::all(&self.proxy)?;}
        anyhow::ensure!(!self.device_name.trim().is_empty(),"Device name is empty");
        let defaults = Self::default().tools;
        let tools = self.tools.as_object().context("Tool settings must be an object")?;
        for (key,value) in tools {
            let default = defaults.get(key).context("Unknown tool setting")?;
            anyhow::ensure!((default.is_array() && value.as_array().is_some_and(|array|array.iter().all(Value::is_string))) ||
                (default.is_boolean() && value.is_boolean()) || (default.is_string() && value.is_string()) ||
                (default.is_number() && value.as_u64().is_some_and(|n|n > 0 && n <= 10_000)),"Invalid setting: {key}");
        }
        for key in defaults.as_object().expect("Invalid default settings").keys() {anyhow::ensure!(tools.contains_key(key),"Missing setting: {key}");}
        anyhow::ensure!(self.tools["telemetryEnabled"] == false,"Telemetry is unavailable");
        #[cfg(target_os="android")]
        {
            anyhow::ensure!(!self.start_at_login,
                "Android autostart is managed by Termux:Boot or the root manager");
            anyhow::ensure!(!self.prevent_sleep,
                "Android wake locks are managed by Termux:API outside Desktop Commander");
        }
        Ok(())
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
    pub gui: Mutex<Option<GuiProcess>>,
    pub gui_generation: std::sync::atomic::AtomicU64,
    pub tray_changed: tokio::sync::Notify,
    pub token: String,
    pub endpoint: RwLock<String>,
    pub slots: tokio::sync::Semaphore,
    _lock: File,
}

pub struct GuiProcess {
    pub cancel:CancellationToken,
    pub task:tokio::task::JoinHandle<Result<std::process::ExitStatus>>,
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
        let settings:Settings = read("settings.json")?.map(serde_json::from_value).transpose()?.unwrap_or_default();
        settings.validate()?;
        let session = read("session.json")?.map(serde_json::from_value).transpose()?;
        let app = Arc::new(Self {
            tools: crate::tools::Tools::new(&dir)?, dir, settings: RwLock::new(settings), session: RwLock::new(session),
            status: RwLock::new(Status { state: "stopped".into(), detail: String::new(), device_id: String::new(),
                verification_uri: String::new(), user_code: String::new(), updated_at: Utc::now().to_rfc3339() }),
            remote: Mutex::new(None), events: Mutex::new(VecDeque::new()), history_lock: Mutex::new(()),
            shutdown: CancellationToken::new(), gui: Mutex::new(None), gui_generation:std::sync::atomic::AtomicU64::new(0),
            tray_changed:tokio::sync::Notify::new(),token: uuid::Uuid::new_v4().to_string(),
            endpoint: RwLock::new(String::new()), slots: tokio::sync::Semaphore::new(4), _lock: lock,
        });
        Ok(app)
    }

    pub async fn state(&self, state: &str, detail: impl Into<String>) {
        let mut status = self.status.write().await;
        status.state = state.into(); status.detail = detail.into(); status.updated_at = Utc::now().to_rfc3339();
        tracing::info!(state, detail = %status.detail);
        drop(status);
        self.tray_changed.notify_one();
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
            cancel.cancel();
            if let Err(error) = task.await? {
                self.state("stopped",format!("远程连接已停止，最后错误：{error:#}")).await;
                return Ok(());
            }
        }
        self.state("stopped", "远程连接已停止").await;
        Ok(())
    }

    pub async fn open_gui(&self) -> Result<()> {
        let mut slot = self.gui.lock().await;
        if let Some(gui) = slot.as_ref() { if !gui.task.is_finished() {
            self.gui_generation.fetch_add(1,std::sync::atomic::Ordering::Relaxed); return Ok(());
        } }
        if let Some(gui)=slot.take() {gui.task.await??;}
        let executable = crate::platform::gui_executable()?;
        let mut command = tokio::process::Command::new(&executable);
        #[cfg(windows)]
        command.current_dir(executable.parent().context("GUI executable directory missing")?);
        let mut child = command
            .env("DC_DESCRIPTOR", self.dir.join("control.json"))
            .kill_on_drop(true).spawn()?;
        let cancel = self.shutdown.child_token();
        let stopped = cancel.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                result=child.wait()=>Ok(result?),
                _=stopped.cancelled()=>{
                    if child.try_wait()?.is_none() {child.kill().await?;}
                    Ok(child.wait().await?)
                }
            }
        });
        *slot = Some(GuiProcess {cancel,task});
        Ok(())
    }

    pub async fn set_settings(&self, settings: Settings) -> Result<()> {
        settings.validate()?;
        if settings.start_at_login != self.settings.read().await.start_at_login {
            crate::platform::autostart(settings.start_at_login, &self.dir)?;
        }
        atomic_json(&self.dir.join("settings.json"), &settings)?;
        *self.settings.write().await = settings;
        Ok(())
    }
}
