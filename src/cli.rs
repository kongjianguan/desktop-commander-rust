use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Method, Url};
use serde_json::{Value, json};
use std::{fs::{File, OpenOptions}, path::Path, time::Duration};

use crate::{
    Command, SettingsAction,
    app::{Settings, atomic_json},
};

struct ControlClient {
    http: Client,
    endpoint: Url,
    token: String,
}

impl ControlClient {
    fn open(dir: &Path) -> Result<Option<Self>> {
        let descriptor = dir.join("control.json");
        if !descriptor.exists() {
            return Ok(None);
        }
        let value: Value = serde_json::from_reader(
            File::open(&descriptor).context("Cannot open local control descriptor")?,
        )
        .context("Invalid control descriptor")?;
        let endpoint = Url::parse(
            value["endpoint"]
                .as_str()
                .context("Control endpoint missing")?,
        )?;
        ensure!(
            endpoint.scheme() == "http"
                && endpoint.host_str() == Some("127.0.0.1")
                && endpoint.port().is_some()
                && endpoint.path() == "/"
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "Refusing to contact non-local control endpoint"
        );
        let token = value["token"]
            .as_str()
            .context("Control authentication token missing")?
            .to_owned();
        ensure!(!token.is_empty(), "Control authentication token is empty");
        let http = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Some(Self {
            http,
            endpoint,
            token,
        }))
    }

    async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = self.endpoint.join(path)?;
        let mut request = self.http.request(method, url).bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .context("Cannot reach the local agent; is it running?")?;
        let status = response.status();
        let bytes = response.bytes().await?;
        ensure!(
            status.is_success(),
            "Control API returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn status(&self) -> Result<Value> {
        self.request(Method::GET, "status", None).await
    }
}

fn offline_settings(dir: &Path) -> Result<Settings> {
    let path = dir.join("settings.json");
    if !path.exists() {
        return Ok(Settings::default());
    }
    let settings: Settings = serde_json::from_reader(File::open(path)?)?;
    settings.validate()?;
    Ok(settings)
}

fn instance_lock(dir: &Path) -> Result<Option<File>> {
    crate::app::private_dir(dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("instance.lock"))?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(Some(lock)),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn local_status(
    client: &ControlClient,
    dir: &Path,
) -> Result<(Option<Value>, Option<File>)> {
    match client.status().await {
        Ok(status) => Ok((Some(status), None)),
        Err(api_error) => match instance_lock(dir)? {
            Some(lock) => Ok((None, Some(lock))),
            None => Err(api_error),
        },
    }
}

fn stopped_status(json_output: bool) -> Result<()> {
    let status = json!({"running": false, "state": "stopped"});
    if json_output {
        println!("{}", pretty(&status)?);
    } else {
        println!("Desktop Commander is not running");
    }
    Ok(())
}

fn lookup<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    let mut current = value;
    for name in key.split('.') {
        current = current
            .get(name)
            .with_context(|| format!("Unknown setting: {key}"))?;
    }
    Ok(current)
}

fn assign(value: &mut Value, key: &str, raw: &str) -> Result<()> {
    let names: Vec<_> = key.split('.').collect();
    ensure!(
        !names.is_empty() && names.iter().all(|name| !name.is_empty()),
        "Invalid setting key"
    );
    let current = lookup(value, key)?;
    let typed = match current {
        Value::Bool(_) => match raw.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Value::Bool(true),
            "false" | "off" | "no" | "0" => Value::Bool(false),
            _ => bail!("Expected true/false for {key}"),
        },
        Value::String(_) => Value::String(raw.to_owned()),
        Value::Number(_) => json!(raw.parse::<u64>().context("Expected positive integer")?),
        Value::Array(_) => {
            let parsed: Value = serde_json::from_str(raw).context("Expected JSON array")?;
            ensure!(parsed.is_array(), "Expected JSON array for {key}");
            parsed
        }
        _ => bail!("Setting {key} is not editable"),
    };
    let mut cursor = value;
    for name in &names[..names.len() - 1] {
        cursor = cursor.get_mut(*name).context("Unknown setting parent")?;
    }
    let target = cursor
        .get_mut(names[names.len() - 1])
        .context("Unknown setting")?;
    *target = typed;
    Ok(())
}

fn pretty(value: &Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

fn print_status(value: &Value) {
    let state = &value["status"];
    println!(
        "Desktop Commander v{} — {}",
        value["version"].as_str().unwrap_or("?"),
        state["state"].as_str().unwrap_or("unknown")
    );
    println!("PID: {}", value["pid"]);
    if let Some(detail) = state["detail"].as_str().filter(|s| !s.is_empty()) {
        println!("Detail: {detail}");
    }
    if let Some(name) = value["settings"]["deviceName"].as_str() {
        println!("Device: {name}");
    }
    if let Some(id) = state["deviceId"].as_str().filter(|s| !s.is_empty()) {
        println!("Device ID: {id}");
    }
    if let Some(uri) = state["verificationUri"].as_str().filter(|s| !s.is_empty()) {
        println!("Authorize: {uri}");
        if let Some(code) = state["userCode"].as_str() {
            println!("Code: {code}");
        }
    }
}

fn print_events(value: &Value, limit: usize) {
    let events = value["events"].as_array();
    let Some(events) = events else {
        println!("No tool calls recorded");
        return;
    };
    if events.is_empty() {
        println!("No tool calls recorded");
        return;
    }
    for event in events.iter().rev().take(limit) {
        println!(
            "{}  {:<7} {:<30} {}ms",
            event["timestamp"].as_str().unwrap_or("?"),
            if event["success"].as_bool().unwrap_or(false) {
                "ok"
            } else {
                "failed"
            },
            event["toolName"].as_str().unwrap_or("?"),
            event["durationMs"].as_u64().unwrap_or_default()
        );
    }
}

async fn online(dir: &Path) -> Result<ControlClient> {
    let client = ControlClient::open(dir)?
        .context("Agent is not running. Start: desktop-commander remote --headless")?;
    // Reject an obsolete control.json descriptor from an exited agent.
    client.status().await?;
    Ok(client)
}

pub async fn run(command: &Command, dir: &Path) -> Result<()> {
    match command {
        Command::Start => {
            if let Some(client) = ControlClient::open(dir)? {
                match client.status().await {
                    Ok(status) => {
                        println!("Agent already running (PID {})", status["pid"]);
                        return Ok(());
                    }
                    Err(api_error) => {
                        if instance_lock(dir)?.is_none() {
                            return Err(api_error);
                        }
                    }
                }
            }
            crate::app::private_dir(dir)?;
            let log_path = dir.join("agent.log");
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600))?;
            }
            let mut launch = std::process::Command::new(std::env::current_exe()?);
            launch
                .arg("--config-dir")
                .arg(dir)
                .arg("remote")
                .arg("--headless")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(log);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                launch.process_group(0);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x0000_0008;
                const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
                launch.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            }
            let mut child = launch
                .spawn()
                .context("Unable to launch background agent")?;
            let readiness = tokio::time::timeout(Duration::from_secs(6), async {
                loop {
                    if let Some(status) = child.try_wait()? {
                        bail!(
                            "Agent exited before becoming ready ({status}); see {}",
                            log_path.display()
                        );
                    }
                    if let Some(client) = ControlClient::open(dir)? {
                        if let Ok(snapshot) = client.status().await {
                            return Ok(snapshot);
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await;
            let snapshot = match readiness {
                Ok(result) => result?,
                Err(_) => {
                    match child.try_wait()? {
                        Some(status) => bail!(
                            "Agent exited before becoming ready ({status}); see {}",
                            log_path.display()
                        ),
                        None => {
                            if let Err(kill_error) = child.kill() {
                                if child.try_wait()?.is_none() {
                                    return Err(kill_error.into());
                                }
                            }
                            child.wait().context("Unable to reap unresponsive agent")?;
                            bail!(
                                "Agent did not become ready within 6 seconds; inspect {}",
                                log_path.display()
                            );
                        }
                    }
                }
            };
            println!(
                "Agent started (PID {}). Log: {}",
                snapshot["pid"],
                log_path.display()
            );
        }
        Command::Status { json } => match ControlClient::open(dir)? {
            None => stopped_status(*json)?,
            Some(client) => match local_status(&client, dir).await? {
                (None, Some(_lock)) => stopped_status(*json)?,
                (Some(mut status), None) => {
                    if *json {
                        status["running"] = json!(true);
                        println!("{}", pretty(&status)?);
                    } else {
                        print_status(&status);
                    }
                }
                _ => unreachable!("local status result invariant"),
            },
        },
        Command::Connect | Command::Disconnect | Command::Quit | Command::Diagnostics => {
            let client = online(dir).await?;
            let (route, name) = match command {
                Command::Connect => ("connect", "Connection requested"),
                Command::Disconnect => ("disconnect", "Remote connection stopped"),
                Command::Quit => ("quit", "Agent shutdown requested"),
                _ => ("export", "Diagnostics exported"),
            };
            let response = client.request(Method::POST, route, None).await?;
            if let Some(path) = response["path"].as_str() {
                println!("{name}: {path}");
            } else {
                println!("{name}");
            }
        }
        Command::Events { limit, json } => {
            let snapshot = online(dir).await?.status().await?;
            let events = snapshot["events"].as_array().context("Invalid events")?;
            let selected: Vec<_> = events.iter().rev().take(*limit).cloned().collect();
            if *json {
                println!("{}", pretty(&json!(selected))?);
            } else {
                print_events(&snapshot, *limit);
            }
        }
        Command::Settings { action } => {
            let client = ControlClient::open(dir)?;
            let (mut data, connected, _offline_lock) = if let Some(client) = &client {
                match local_status(client, dir).await? {
                    (None, Some(lock)) => (serde_json::to_value(offline_settings(dir)?)?, false, Some(lock)),
                    (Some(snapshot), None) => (snapshot["settings"].clone(), true, None),
                    _ => unreachable!("local status result invariant"),
                }
            } else {
                let lock = instance_lock(dir)?.context("Agent is starting; retry the command")?;
                (serde_json::to_value(offline_settings(dir)?)?, false, Some(lock))
            };
            match action {
                SettingsAction::Show { json } => {
                    if *json {
                        println!("{}", pretty(&data)?);
                    } else {
                        for (name, val) in data.as_object().context("Invalid settings")? {
                            println!(
                                "{name} = {}",
                                if let Some(s) = val.as_str() {
                                    s.to_owned()
                                } else {
                                    val.to_string()
                                }
                            );
                        }
                    }
                }
                SettingsAction::Get { key } => {
                    let val = lookup(&data, key)?;
                    println!(
                        "{}",
                        if let Some(s) = val.as_str() {
                            s.to_owned()
                        } else {
                            val.to_string()
                        }
                    );
                }
                SettingsAction::Set { key, value } => {
                    assign(&mut data, key, value)?;
                    let updated: Settings = serde_json::from_value(data.clone())?;
                    updated.validate()?;
                    if connected {
                        client
                            .as_ref()
                            .expect("verified client")
                            .request(Method::PUT, "settings", Some(&data))
                            .await?;
                    } else {
                        crate::app::private_dir(dir)?;
                        atomic_json(&dir.join("settings.json"), &updated)?;
                    }
                    println!("Updated {key}");
                }
            }
        }
        Command::Logout => {
            if let Some(client) = ControlClient::open(dir)? {
                let (_, stopped_lock) = local_status(&client, dir).await?;
                if let Some(_lock) = stopped_lock {
                    let session = dir.join("session.json");
                    if session.exists() {
                        std::fs::remove_file(session)?;
                    }
                } else {
                    client.request(Method::POST, "logout", None).await?;
                }
            } else {
                // No agent owns the instance; delete only the credential file.
                let app = crate::app::App::load(dir.to_owned())?;
                let session = app.dir.join("session.json");
                if session.exists() {
                    std::fs::remove_file(session)?;
                }
            }
            println!("Local device session removed (server-side revocation not performed)");
        }
        _ => bail!("Not a management command"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_parsing_is_type_safe() {
        let mut data = serde_json::to_value(Settings::default()).unwrap();
        assign(&mut data, "deviceName", "Test Android").unwrap();
        assign(&mut data, "autoConnect", "off").unwrap();
        assign(&mut data, "tools.allowedDirectories", "[\"/tmp\"]").unwrap();
        assert_eq!(data["deviceName"], "Test Android");
        assert_eq!(data["autoConnect"], false);
        assert_eq!(data["tools"]["allowedDirectories"], json!(["/tmp"]));
        assert!(assign(&mut data, "autoConnect", "tomorrow").is_err());
        assert!(assign(&mut data, "password", "secret").is_err());
        assert!(assign(&mut data, "tools", "overwrite").is_err());
        serde_json::from_value::<Settings>(data)
            .unwrap()
            .validate()
            .unwrap();
    }
}
