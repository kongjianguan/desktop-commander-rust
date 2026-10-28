#![cfg_attr(all(windows,feature="tray"),windows_subsystem="windows")]

mod app;
mod cli;
mod control;
mod documents;
mod mcp;
mod platform;
mod remote;
mod tools;
mod terminal;
mod search;
#[cfg(all(feature="tray",target_os="linux"))]
mod linux_tray;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, global = true, env = "DC_CONFIG_DIR")]
    config_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Remote { #[arg(long)] headless: bool },
    Stdio,
    Gui,
    /// Launch the headless agent in the background
    Start,
    /// Show running connection details (works without a running agent)
    Status { #[arg(long)] json: bool },
    /// Connect the running agent to the remote service
    Connect,
    /// Disconnect the running agent from the remote service
    Disconnect,
    /// View or edit settings, including when the agent is stopped
    Settings { #[command(subcommand)] action: SettingsAction },
    /// Show recent tool calls recorded by the running agent
    Events { #[arg(long, default_value_t=20)] limit: usize, #[arg(long)] json: bool },
    /// Export diagnostic data from the running agent
    Diagnostics,
    /// Stop the local headless agent
    Quit,
    Logout,
    DocumentWorker,
}

#[derive(Subcommand)]
enum SettingsAction {
    /// View saved settings
    Show { #[arg(long)] json: bool },
    /// Read a setting such as proxy, autoConnect, tools.defaultShell
    Get { key: String },
    /// Set a setting (booleans accept on/off; arrays accept JSON)
    Set { key: String, value: String },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::DocumentWorker)) { return documents::worker(); }
    let dir = cli.config_dir.unwrap_or_else(|| {
        #[cfg(target_os="android")]
        {
            // Termux owns this home directory; never default to a system-wide or root-owned path.
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(||"/data/local/tmp".into()))
                .join(".config/desktop-commander-rust")
        }
        #[cfg(not(target_os="android"))]
        {
            directories::ProjectDirs::from("app", "DesktopCommander", "DesktopCommanderRust")
                .expect("User configuration directory unavailable").config_dir().to_owned()
        }
    });
    if matches!(cli.command, Some(Command::Gui)) { return control::open_existing(&dir); }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    if let Some(command) = cli.command.as_ref() {
        if matches!(command,
            Command::Start | Command::Status {..} | Command::Connect | Command::Disconnect |
            Command::Settings {..} | Command::Events {..} | Command::Diagnostics |
            Command::Quit | Command::Logout) {
            return runtime.block_on(cli::run(command, &dir));
        }
    }
    let app = match app::App::load(dir.clone()) {
        Ok(app) => app,
        Err(error) if !matches!(cli.command,Some(Command::Stdio|Command::Logout|Command::Remote{headless:true})) &&
            error.downcast_ref::<std::io::Error>().is_some_and(|e|e.raw_os_error() == fs2::lock_contended_error().raw_os_error()) => {
                return control::open_existing(&dir);
            }
        Err(error) => return Err(error),
    };
    let signals = app.clone();
    runtime.spawn(async move {
        tokio::select! {
            result=platform::wait_for_termination()=> {
                if let Err(error)=result {tracing::error!("Signal handler failed: {error:#}");}
                signals.shutdown.cancel();
            }
            _=signals.shutdown.cancelled()=>{}
        }
    });
    if matches!(cli.command, Some(Command::Stdio)) { return runtime.block_on(mcp::serve(app)); }
    let headless = !cfg!(feature="tray") || matches!(cli.command, Some(Command::Remote { headless: true }));
    let running = app.clone();
    let background = runtime.spawn(async move {
        let result = control::serve(running.clone()).await;
        if let Err(error) = result { tracing::error!("{error:#}"); running.shutdown.cancel(); }
    });
    #[cfg(feature = "tray")]
    if !headless { platform::tray(app.clone(), runtime.handle().clone())?; }
    runtime.block_on(async {
        if headless { app.shutdown.cancelled().await; }
        app.shutdown.cancel();
        app.stop().await?;
        app.tools.terminal.shutdown().await?;
        if let Some(gui) = app.gui.lock().await.take() {gui.cancel.cancel(); gui.task.await??;}
        background.await?;
        let descriptor = app.dir.join("control.json"); if descriptor.exists() { std::fs::remove_file(descriptor)?; }
        Ok(())
    })
}
