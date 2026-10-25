use anyhow::{Result,ensure};
use ksni::{TrayMethods,menu::{MenuItem,StandardItem}};
use std::sync::Arc;

struct Tray {
    app:Arc<crate::app::App>,
    runtime:tokio::runtime::Handle,
    label:String,
}

impl Tray {
    fn open(&self) {
        let app=self.app.clone();
        self.runtime.spawn(async move {if let Err(error)=app.open_gui().await {app.state("error",format!("{error:#}")).await;}});
    }
    fn connect(&self) {
        let app=self.app.clone();self.runtime.spawn(async move {app.start().await;});
    }
    fn disconnect(&self) {
        let app=self.app.clone();
        self.runtime.spawn(async move {if let Err(error)=app.stop().await {app.state("error",format!("{error:#}")).await;}});
    }
}

impl ksni::Tray for Tray {
    fn id(&self)->String {"desktop-commander-rust".into()}
    fn title(&self)->String {format!("Desktop Commander — {}",self.label)}
    fn activate(&mut self,_x:i32,_y:i32) {self.open();}
    fn icon_pixmap(&self)->Vec<ksni::Icon> {
        static ICON:std::sync::LazyLock<ksni::Icon>=std::sync::LazyLock::new(|| {
            let mut data=Vec::with_capacity(32*32*4);
            for y in 0..32 {for x in 0..32 {
                let active=(7..25).contains(&x)&&(7..25).contains(&y)&&(x<11||x>20||y<11||y>20);
                data.extend_from_slice(if active {&[255,99,102,241]}else{&[0,0,0,0]});
            }}
            ksni::Icon{width:32,height:32,data}
        });
        vec![ICON.clone()]
    }
    fn tool_tip(&self)->ksni::ToolTip {
        ksni::ToolTip{title:"Desktop Commander".into(),description:self.label.clone(),..Default::default()}
    }
    fn menu(&self)->Vec<MenuItem<Self>> {
        vec![
            StandardItem{label:self.label.clone(),enabled:false,..Default::default()}.into(),
            MenuItem::Separator,
            StandardItem{label:"打开管理界面".into(),activate:Box::new(|tray:&mut Self|tray.open()),..Default::default()}.into(),
            StandardItem{label:"连接远程服务".into(),activate:Box::new(|tray:&mut Self|tray.connect()),..Default::default()}.into(),
            StandardItem{label:"停止远程连接".into(),activate:Box::new(|tray:&mut Self|tray.disconnect()),..Default::default()}.into(),
            MenuItem::Separator,
            StandardItem{label:"退出程序".into(),activate:Box::new(|tray:&mut Self|tray.app.shutdown.cancel()),..Default::default()}.into(),
        ]
    }
}

pub async fn run(app:Arc<crate::app::App>,runtime:tokio::runtime::Handle)->Result<()> {
    let handle=Tray{app:app.clone(),runtime,label:"正在启动".into()}.spawn().await?;
    loop {
        tokio::select! {
            _=app.shutdown.cancelled()=>break,
            _=app.tray_changed.notified()=>{
                let status=app.status.read().await;
                let label=match status.state.as_str(){"online"=>"远程连接正常","connecting"=>"正在连接","authorizing"=>"等待设备授权","reconnecting"=>"正在重新连接","error"=>"连接发生错误",_=>"远程连接已停止"}.to_owned();
                drop(status);
                ensure!(handle.update(|tray|tray.label=label).await.is_some(),"Status notifier service closed");
            }
        }
    }
    handle.shutdown().await;
    Ok(())
}
