use anyhow::{Result, Context};
use axum::{Router, Json, extract::{State, Request}, middleware::{self, Next}, response::{Response, IntoResponse}, http::StatusCode, routing::{get, post, put}};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use crate::app::{App, Settings};

type ApiResult = Result<Json<Value>,(StatusCode,String)>;
fn failure(error: impl std::fmt::Display) -> (StatusCode,String) { (StatusCode::BAD_REQUEST,error.to_string()) }

async fn authenticate(State(app):State<Arc<App>>,request:Request,next:Next) -> Response {
    let expected = format!("Bearer {}",app.token);
    if request.headers().get("authorization").is_none_or(|value|value.as_bytes() != expected.as_bytes()) || request.headers().contains_key("origin") {
        return (StatusCode::UNAUTHORIZED,"Unauthorized").into_response();
    }
    next.run(request).await
}

async fn status(State(app):State<Arc<App>>) -> Json<Value> {
    Json(json!({"status":*app.status.read().await,"settings":*app.settings.read().await,
        "events":*app.events.lock().await,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),
        "guiGeneration":app.gui_generation.load(std::sync::atomic::Ordering::Relaxed)}))
}
async fn start(State(app):State<Arc<App>>) -> ApiResult { app.start().await; Ok(Json(json!({"ok":true}))) }
async fn stop(State(app):State<Arc<App>>) -> ApiResult { app.stop().await.map_err(failure)?; Ok(Json(json!({"ok":true}))) }
async fn settings(State(app):State<Arc<App>>,Json(settings):Json<Settings>) -> ApiResult {
    let reconnect = app.remote.lock().await.as_ref().is_some_and(|(_,task)|!task.is_finished());
    app.stop().await.map_err(failure)?;
    app.set_settings(settings).await.map_err(failure)?;
    if reconnect { app.start().await; }
    Ok(Json(json!({"ok":true})))
}
async fn logout(State(app):State<Arc<App>>) -> ApiResult {
    app.stop().await.map_err(failure)?;
    let mut session = app.session.write().await;
    let path = app.dir.join("session.json"); if path.exists() { tokio::fs::remove_file(path).await.map_err(failure)?; }
    *session = None; Ok(Json(json!({"ok":true})))
}
async fn export(State(app):State<Arc<App>>) -> ApiResult {
    let path = app.dir.join("diagnostics.json");
    let data = json!({"version":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "status":*app.status.read().await,"history":crate::tools::recent(&app).await.map_err(failure)?});
    crate::app::atomic_json(&path,&data).map_err(failure)?;
    Ok(Json(json!({"path":path})))
}
async fn gui(State(app):State<Arc<App>>) -> ApiResult { app.open_gui().await.map_err(failure)?; Ok(Json(json!({"ok":true}))) }
async fn quit(State(app):State<Arc<App>>) -> ApiResult { app.shutdown.cancel(); Ok(Json(json!({"ok":true}))) }

pub async fn serve(app:Arc<App>) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,0)).await?;
    let endpoint = format!("http://{}",listener.local_addr()?);
    *app.endpoint.write().await = endpoint.clone();
    crate::app::atomic_json(&app.dir.join("control.json"),&json!({"endpoint":endpoint,"token":app.token,"pid":std::process::id()}))?;
    let router = Router::new().route("/status",get(status)).route("/connect",post(start)).route("/disconnect",post(stop))
        .route("/settings",put(settings)).route("/logout",post(logout)).route("/export",post(export))
        .route("/gui",post(gui)).route("/quit",post(quit))
        .layer(middleware::from_fn_with_state(app.clone(),authenticate)).with_state(app.clone());
    if app.settings.read().await.auto_connect { app.start().await; }
    let cancel = app.shutdown.clone();
    axum::serve(listener,router).with_graceful_shutdown(cancel.cancelled_owned()).await?;
    Ok(())
}

pub fn open_existing(dir:&Path) -> Result<()> {
    let descriptor: Value = serde_json::from_reader(std::fs::File::open(dir.join("control.json")).context("Start Desktop Commander before opening the control panel")?)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        reqwest::Client::builder().no_proxy().build()?.post(format!("{}/gui",crate::tools::string(&descriptor,"endpoint")?))
            .bearer_auth(crate::tools::string(&descriptor,"token")?).send().await?.error_for_status()?;
        Ok(())
    })
}
