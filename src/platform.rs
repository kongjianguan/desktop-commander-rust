use anyhow::{Result, Context, ensure};
use std::{path::{Path,PathBuf}, process::{Child,Command}};

fn mac_contents() -> Result<Option<PathBuf>> {
    let executable = std::env::current_exe()?;
    let parent = executable.parent().context("Executable directory missing")?;
    if cfg!(target_os="macos") && parent.file_name().is_some_and(|name|name == "MacOS") {
        return Ok(Some(parent.parent().context("Application Contents missing")?.to_owned()));
    }
    Ok(None)
}
pub fn resources() -> Result<PathBuf> {
    let dir = if let Some(contents) = mac_contents()? {contents.join("Resources")}
        else {std::env::current_exe()?.parent().context("Executable directory missing")?.join("resources")};
    ensure!(dir.is_dir(),"Application resources missing: {}",dir.display()); Ok(dir)
}
pub fn bundled(name:&str) -> Result<PathBuf> {
    let name = if cfg!(windows) {format!("{name}.exe")} else {name.into()};
    let path = if let Some(contents) = mac_contents()? {contents.join("Helpers").join(name)} else {resources()?.join("bin").join(name)};
    ensure!(path.is_file(),"Bundled executable missing: {}",path.display()); Ok(path)
}
pub fn gui_executable() -> Result<PathBuf> {
    if let Some(contents) = mac_contents()? {
        let path = contents.join("Helpers/Desktop Commander Control.app/Contents/MacOS/desktop_commander");
        ensure!(path.is_file(),"Flutter control panel is missing: {}",path.display()); return Ok(path);
    }
    let dir = std::env::current_exe()?.parent().context("Executable directory missing")?.join("desktop");
    let path = if cfg!(target_os="macos") {dir.join("Desktop Commander.app/Contents/MacOS/desktop_commander")} else if cfg!(windows) {dir.join("desktop_commander.exe")} else {dir.join("desktop_commander")};
    ensure!(path.is_file(),"Flutter control panel is missing: {}",path.display()); Ok(path)
}
pub fn pdfium_library() -> Result<PathBuf> {
    if let Some(contents) = mac_contents()? {return Ok(contents.join("Frameworks/libpdfium.dylib"));}
    Ok(resources()?.join(if cfg!(windows){"pdfium.dll"}else if cfg!(target_os="macos"){"libpdfium.dylib"}else{"libpdfium.so"}))
}
pub async fn wait_for_termination() -> Result<()> {
    #[cfg(unix)] {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result?, _=term.recv()=>{}}
    }
    #[cfg(windows)] {tokio::signal::ctrl_c().await?;}
    Ok(())
}
pub fn kill_tree(pid:u32) -> Result<()> {
    ensure!(pid > 1 && pid != std::process::id(),"Refusing to terminate this process or PID 1");
    #[cfg(windows)] {
        let status = Command::new("taskkill").args(["/PID",&pid.to_string(),"/T","/F"]).output()?;
        ensure!(status.status.success(),"taskkill failed: {}",String::from_utf8_lossy(&status.stderr));
    }
    #[cfg(unix)] {
        let system = sysinfo::System::new_all();
        fn descend(system:&sysinfo::System,pid:sysinfo::Pid) {
            for (&id,child) in system.processes() { if child.parent() == Some(pid) { descend(system,id); } }
            if let Some(process) = system.process(pid) { process.kill(); }
        }
        descend(&system,sysinfo::Pid::from_u32(pid));
    }
    Ok(())
}

pub struct SleepGuard {
    child:Option<Child>,
    #[cfg(windows)]
    stop:Option<std::sync::mpsc::Sender<()>>,
    #[cfg(windows)]
    thread:Option<std::thread::JoinHandle<()>>,
}
impl SleepGuard {
    pub fn new(enabled:bool) -> Result<Self> {
        if !enabled { return Ok(Self {child:None,#[cfg(windows)]stop:None,#[cfg(windows)]thread:None}); }
        #[cfg(windows)] {
            use windows_sys::Win32::System::Power::{SetThreadExecutionState,ES_CONTINUOUS,ES_SYSTEM_REQUIRED};
            let (ready_tx,ready_rx) = std::sync::mpsc::sync_channel(1);
            let (stop,receiver) = std::sync::mpsc::channel();
            let thread = std::thread::Builder::new().name("sleep-inhibitor".into()).spawn(move || {
                let state = unsafe {SetThreadExecutionState(ES_CONTINUOUS|ES_SYSTEM_REQUIRED)};
                let result = if state == 0 {Err(std::io::Error::last_os_error())} else {Ok(())};
                ready_tx.send(result).expect("Sleep inhibitor controller disappeared");
                if state == 0 {return;}
                let _ = receiver.recv();
                let cleared = unsafe {SetThreadExecutionState(ES_CONTINUOUS)};
                if cleared == 0 {tracing::error!("Unable to clear sleep inhibition: {}",std::io::Error::last_os_error());}
            })?;
            ready_rx.recv()??;
            return Ok(Self {child:None,stop:Some(stop),thread:Some(thread)});
        }
        #[cfg(target_os="macos")]
        let child = Command::new("/usr/bin/caffeinate").args(["-i","-w",&std::process::id().to_string()]).spawn()?;
        #[cfg(target_os="linux")]
        let child = Command::new("systemd-inhibit").args(["--what=sleep","--why=Desktop Commander remote connection","--mode=block","sleep","infinity"]).spawn()?;
        #[cfg(not(windows))]
        Ok(Self {child:Some(child)})
    }
}
impl Drop for SleepGuard { fn drop(&mut self) {
    #[cfg(windows)] {
        if let Some(stop)=self.stop.take() {let _=stop.send(());}
        if let Some(thread)=self.thread.take() {if thread.join().is_err() {tracing::error!("Sleep inhibitor thread panicked");}}
    }
    if let Some(child) = self.child.as_mut() { let _ = child.kill(); let _ = child.wait(); }
} }

pub fn autostart(enabled:bool,dir:&Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    #[cfg(target_os="macos")] {
        let home = directories::BaseDirs::new().context("Home unavailable")?;
        let folder = home.home_dir().join("Library/LaunchAgents"); std::fs::create_dir_all(&folder)?;
        let path = folder.join("app.desktopcommander.rust.plist");
        if enabled {
            // plist 库处理路径转义及文件格式。
            let value = serde_json::json!({"Label":"app.desktopcommander.rust","ProgramArguments":[exe.to_string_lossy(),"--config-dir",dir.to_string_lossy(),"remote"],"RunAtLoad":true});
            let data = plist::to_value(&value)?; data.to_file_xml(path)?;
        } else if path.exists() {std::fs::remove_file(path)?;}
    }
    #[cfg(target_os="linux")] {
        let base = directories::BaseDirs::new().context("Home unavailable")?;
        let folder = base.config_dir().join("autostart"); std::fs::create_dir_all(&folder)?;
        let path = folder.join("desktop-commander-rust.desktop");
        if enabled {
            let quote = |p:&Path|p.to_string_lossy().replace('\\',"\\\\").replace('"',"\\\"").replace('`',"\\`").replace('$',"\\$").replace('%',"%%");
            std::fs::write(path,format!("[Desktop Entry]\nType=Application\nName=Desktop Commander\nExec=\"{}\" --config-dir \"{}\" remote\nTerminal=false\n",quote(&exe),quote(dir)))?;
        } else if path.exists() {std::fs::remove_file(path)?;}
    }
    #[cfg(windows)] {
        let command = format!("\"{}\" --config-dir \"{}\" remote",exe.display(),dir.display());
        let mut cmd = Command::new("reg");
        cmd.args([if enabled {"add"} else {"delete"},"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run","/v","DesktopCommanderRust","/f"]);
        if enabled {cmd.args(["/t","REG_SZ","/d",&command]);}
        let result = cmd.output()?;
        ensure!(result.status.success(),"Unable to update startup entry: {}",String::from_utf8_lossy(&result.stderr));
    }
    Ok(())
}

#[cfg(feature="tray")]
pub fn tray(app:std::sync::Arc<crate::app::App>,runtime:tokio::runtime::Handle) -> Result<()> {
    use tao::{event::{Event,StartCause},event_loop::{ControlFlow,EventLoopBuilder}};
    use tao::platform::run_return::EventLoopExtRunReturn;
    use tray_icon::{TrayIconBuilder,Icon,menu::{Menu,MenuItem,PredefinedMenuItem,MenuEvent}};
    let mut event_loop = EventLoopBuilder::new().build();
    let menu = Menu::new();
    let status = MenuItem::new("正在启动",false,None);
    let open = MenuItem::new("打开管理界面",true,None);
    let connect = MenuItem::new("连接远程服务",true,None);
    let disconnect = MenuItem::new("停止远程连接",true,None);
    let quit = MenuItem::new("退出程序",true,None);
    menu.append_items(&[&status,&PredefinedMenuItem::separator(),&open,&connect,&disconnect,&PredefinedMenuItem::separator(),&quit])?;
    let mut pixels = Vec::with_capacity(32*32*4);
    for y in 0..32 {for x in 0..32 {let active = (7..25).contains(&x) && (7..25).contains(&y) && (x < 11 || x > 20 || y < 11 || y > 20);
        pixels.extend_from_slice(if active {&[99,102,241,255]} else {&[0,0,0,0]});}}
    let icon = Icon::from_rgba(pixels,32,32)?; let mut tray = None;
    let mut previous = String::new();
    event_loop.run_return(move |event,_,control| {
        *control = ControlFlow::WaitUntil(std::time::Instant::now()+std::time::Duration::from_millis(500));
        if matches!(event,Event::NewEvents(StartCause::Init)) {
            tray = Some(TrayIconBuilder::new().with_icon(icon.clone()).with_menu(Box::new(menu.clone()))
                .with_tooltip("Desktop Commander").build().expect("Unable to create status icon"));
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let app = app.clone();
            if event.id == open.id() { runtime.spawn(async move {if let Err(e) = app.open_gui().await {app.state("error",format!("{e:#}")).await;}}); }
            else if event.id == connect.id() { runtime.spawn(async move {app.start().await;}); }
            else if event.id == disconnect.id() { runtime.spawn(async move {if let Err(e) = app.stop().await {app.state("error",format!("{e:#}")).await;}}); }
            else if event.id == quit.id() {app.shutdown.cancel();}
        }
        if let Ok(snapshot) = app.status.try_read() {
            if snapshot.state == previous {if app.shutdown.is_cancelled() {tray.take(); *control=ControlFlow::Exit;} return;}
            previous = snapshot.state.clone();
            let label = match snapshot.state.as_str() {"online"=>"远程连接正常","connecting"=>"正在连接","authorizing"=>"等待设备授权","reconnecting"=>"正在重新连接","error"=>"连接发生错误",_=>"远程连接已停止"};
            status.set_text(label);
            if let Some(tray) = tray.as_ref() {let _ = tray.set_tooltip(Some(format!("Desktop Commander — {label}")));}
        }
        if app.shutdown.is_cancelled() {tray.take(); *control=ControlFlow::Exit;}
    });
    Ok(())
}
