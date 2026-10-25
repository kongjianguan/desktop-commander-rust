#![cfg_attr(all(windows,feature="tray"),windows_subsystem="windows")]

mod app;
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
    Logout,
    DocumentWorker,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::DocumentWorker)) { return documents::worker(); }
    let dir = cli.config_dir.unwrap_or_else(|| directories::ProjectDirs::from("app", "DesktopCommander", "DesktopCommanderRust")
        .expect("User configuration directory unavailable").config_dir().to_owned());
    if matches!(cli.command, Some(Command::Gui)) { return control::open_existing(&dir); }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let app = match app::App::load(dir.clone()) {
        Ok(app) => app,
        Err(error) if !matches!(cli.command,Some(Command::Stdio|Command::Logout|Command::Remote{headless:true})) &&
            error.downcast_ref::<std::io::Error>().is_some_and(|e|e.raw_os_error() == fs2::lock_contended_error().raw_os_error()) => {
                return control::open_existing(&dir);
            }
        Err(error) => return Err(error),
    };
    if matches!(cli.command, Some(Command::Logout)) {
        let path = app.dir.join("session.json"); if path.exists() { std::fs::remove_file(path)?; } return Ok(());
    }
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
