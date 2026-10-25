use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest_websocket::{Message, Upgrade};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::{Arc, atomic::{AtomicI64, Ordering}}, time::{Duration, Instant}};
use tokio_util::sync::CancellationToken;
use crate::app::{App, Session, Settings};

pub fn http_client(settings: &Settings) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(2).user_agent(concat!("desktop-commander-rust/",env!("CARGO_PKG_VERSION")));
    if !settings.proxy.is_empty() {
        builder = builder.proxy(reqwest::Proxy::all(&settings.proxy)?.no_proxy(reqwest::NoProxy::from_string(&settings.no_proxy)));
    }
    Ok(builder.build()?)
}

struct Cloud {
    app: Arc<App>, client: reqwest::Client, url: String, key: String, user: String,
    offset: AtomicI64,
}

impl Cloud {
    fn observe(&self, response: &reqwest::Response) {
        if let Some(date) = response.headers().get("date").and_then(|v|v.to_str().ok()).and_then(|v|httpdate::parse_http_date(v).ok()) {
            let server = chrono::DateTime::<Utc>::from(date).timestamp_millis();
            self.offset.store(server - Utc::now().timestamp_millis(), Ordering::Relaxed);
        }
    }
    fn now(&self) -> String {
        (Utc::now() + chrono::Duration::milliseconds(self.offset.load(Ordering::Relaxed))).to_rfc3339()
    }
    async fn request(&self, method: reqwest::Method, route: &str, body: Option<&Value>) -> Result<Value> {
        let token = self.app.session.read().await.as_ref().context("Not authorized")?.access_token.clone();
        let mut request = self.client.request(method,format!("{}{route}",self.url)).bearer_auth(token)
            .header("apikey",&self.key).header("Prefer","return=representation").timeout(Duration::from_secs(15));
        if let Some(body) = body { request = request.json(body); }
        let response = request.send().await?; self.observe(&response);
        let status = response.status();
        let bytes = response.bytes().await?;
        ensure!(status.is_success(),"Cloud request failed ({status}): {}",String::from_utf8_lossy(&bytes));
        if bytes.is_empty() { Ok(Value::Null) } else { Ok(serde_json::from_slice(&bytes)?) }
    }
    async fn refresh(&self) -> Result<()> {
        let session = self.app.session.read().await.clone().context("Session missing")?;
        let response = self.client.post(format!("{}/auth/v1/token?grant_type=refresh_token",self.url))
            .header("apikey",&self.key).json(&json!({"refresh_token":session.refresh_token})).timeout(Duration::from_secs(15)).send().await?;
        self.observe(&response);
        let status = response.status(); let body: Value = response.json().await?;
        ensure!(status.is_success(),"Session refresh failed ({status}): {}",body["msg"]);
        self.app.save_session(Session { access_token: crate::tools::string(&body,"access_token")?.into(),
            refresh_token:crate::tools::string(&body,"refresh_token")?.into(),device_id:session.device_id }).await
    }
    async fn device(&self, online: bool) -> Result<()> {
        let session = self.app.session.read().await.clone().context("Session missing")?;
        let settings = self.app.settings.read().await;
        let caps = json!({"tools":self.app.tools.definitions,"transport_broadcast_v1":online,
            "platform":std::env::consts::OS,"version":env!("CARGO_PKG_VERSION")});
        self.request(reqwest::Method::PATCH,&format!("/rest/v1/mcp_devices?id=eq.{}",session.device_id),
            Some(&json!({"status":if online {"online"}else{"offline"},"last_seen":self.now(),"capabilities":caps,"device_name":settings.device_name}))).await?;
        Ok(())
    }
    async fn pending(&self) -> Result<Vec<String>> {
        let id = self.app.session.read().await.as_ref().context("Session missing")?.device_id.clone();
        let mut url = reqwest::Url::parse(&format!("{}/rest/v1/mcp_remote_calls",self.url))?;
        url.query_pairs_mut().append_pair("device_id",&format!("eq.{id}"))
            .append_pair("status","eq.pending").append_pair("timeout_at",&format!("gt.{}",self.now()))
            .append_pair("select","id").append_pair("order","created_at.asc").append_pair("limit","100");
        let route = format!("{}?{}",url.path(),url.query().unwrap_or_default());
        let rows = self.request(reqwest::Method::GET,&route,None).await?;
        Ok(rows.as_array().context("Invalid pending response")?.iter().filter_map(|row|row["id"].as_str().map(str::to_owned)).collect())
    }
    async fn execute(&self, id: &str) -> Result<()> {
        let session = self.app.session.read().await.clone().context("Session missing")?;
        let mut url = reqwest::Url::parse(&format!("{}/rest/v1/mcp_remote_calls",self.url))?;
        url.query_pairs_mut().append_pair("id",&format!("eq.{id}"))
            .append_pair("device_id",&format!("eq.{}",session.device_id)).append_pair("status","eq.pending")
            .append_pair("timeout_at",&format!("gt.{}",self.now()));
        let rows = self.request(reqwest::Method::PATCH,&format!("{}?{}",url.path(),url.query().unwrap()),Some(&json!({"status":"executing"}))).await?;
        let Some(row) = rows.as_array().and_then(|rows|rows.first()) else { return Ok(()); };
        let name = crate::tools::string(row,"tool_name")?;
        let args = row.get("tool_args").cloned().unwrap_or_else(||json!({}));
        let response = match name {
            "ping" => crate::tools::text(format!("pong {}",self.now())),
            "shutdown" => { self.app.shutdown.cancel(); crate::tools::text("Shutdown requested") },
            _ => crate::tools::Tools::call(self.app.clone(),name,args).await,
        };
        let body = json!({"status":"completed","completed_at":self.now(),"result":strip_nulls(response)});
        let outbox = self.app.dir.join("outbox"); crate::app::private_dir(&outbox)?;
        let file = outbox.join(format!("{}.json",uuid::Uuid::parse_str(id)?));
        crate::app::atomic_json(&file,&body)?;
        self.request(reqwest::Method::PATCH,&format!("/rest/v1/mcp_remote_calls?id=eq.{id}&status=eq.executing"),Some(&body)).await?;
        tokio::fs::remove_file(file).await?;
        Ok(())
    }
    async fn flush_results(&self) -> Result<()> {
        let dir = self.app.dir.join("outbox"); if !dir.exists() { return Ok(()); }
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.path().extension().is_none_or(|ext|ext != "json") { continue; }
            let id = uuid::Uuid::parse_str(entry.path().file_stem().context("Outbox filename missing")?.to_str().context("Invalid filename")?)?;
            let body: Value = serde_json::from_slice(&tokio::fs::read(entry.path()).await?)?;
            self.request(reqwest::Method::PATCH,&format!("/rest/v1/mcp_remote_calls?id=eq.{id}&status=eq.executing"),Some(&body)).await?;
            tokio::fs::remove_file(entry.path()).await?;
        }
        Ok(())
    }
}

fn strip_nulls(value: Value) -> Value {
    match value { Value::String(s) => json!(s.replace('\0',"")), Value::Array(a) => Value::Array(a.into_iter().map(strip_nulls).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k,v)|(k.replace('\0',""),strip_nulls(v))).collect()), other => other }
}

async fn authorize(app: &Arc<App>, client: &reqwest::Client, cancel: &CancellationToken) -> Result<()> {
    use rand::RngCore;
    let mut random = [0u8;32]; rand::rng().fill_bytes(&mut random);
    let verifier = URL_SAFE_NO_PAD.encode(random);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let settings = app.settings.read().await.clone();
    let response: Value = client.post(format!("{}/device/start",settings.server_url))
        .json(&json!({"client_id":"mcp-device","scope":"mcp:tools","device_name":settings.device_name,
            "device_type":"mcp","code_challenge":challenge,"code_challenge_method":"S256"}))
        .timeout(Duration::from_secs(15)).send().await?.error_for_status()?.json().await?;
    let uri = crate::tools::string(&response,"verification_uri_complete")?;
    let code = crate::tools::string(&response,"user_code")?;
    { let mut status = app.status.write().await; status.verification_uri = uri.into(); status.user_code = code.into(); }
    app.state("authorizing",format!("请在浏览器确认验证码 {code}")).await;
    eprintln!("Authorize device: {uri}\nCode: {code}");
    if let Err(error) = open::that(uri) { tracing::warn!("Unable to open browser: {error}"); }
    let mut interval = crate::tools::number(&response,"interval",5).max(1);
    let deadline = Instant::now() + Duration::from_secs(crate::tools::number(&response,"expires_in",600));
    while Instant::now() < deadline {
        tokio::select! { _ = cancel.cancelled() => bail!("Authorization cancelled"), _ = tokio::time::sleep(Duration::from_secs(interval)) => {} }
        let response = client.post(format!("{}/device/poll",settings.server_url))
            .json(&json!({"client_id":"mcp-device","device_code":response["device_code"],"code_verifier":verifier}))
            .timeout(Duration::from_secs(15)).send().await?;
        let status = response.status(); let body: Value = response.json().await?;
        if status.is_success() && body["access_token"].is_string() {
            app.save_session(Session { access_token:crate::tools::string(&body,"access_token")?.into(),
                refresh_token:crate::tools::string(&body,"refresh_token")?.into(),
                device_id:crate::tools::string(&body,"device_id")?.into() }).await?;
            let mut status = app.status.write().await; status.verification_uri.clear(); status.user_code.clear(); return Ok(());
        }
        match body["error"].as_str() {
            Some("authorization_pending") => {}, Some("slow_down") => interval += 5,
            _ => bail!("Authorization failed ({status}): {}",body["error_description"]),
        }
    }
    bail!("Authorization expired")
}

pub async fn run(app: Arc<App>, cancel: CancellationToken) -> Result<()> {
    let settings = app.settings.read().await.clone();
    let client = http_client(&settings)?;
    app.state("connecting","正在读取服务配置").await;
    let mut attempt = 0u32;
    let info: Value = loop {
        let response = client.get(format!("{}/api/mcp-info",settings.server_url)).timeout(Duration::from_secs(10)).send().await;
        match response {
            Ok(response) if response.status().is_success() => break response.json().await?,
            Ok(response) if response.status().as_u16() != 429 && !response.status().is_server_error() => { response.error_for_status()?; bail!("Invalid service configuration"); }
            error => { app.state("reconnecting",format!("配置请求失败：{error:?}")).await; }
        }
        attempt += 1;
        tokio::select! { _ = cancel.cancelled() => return Ok(()), _ = tokio::time::sleep(Duration::from_secs((1u64 << attempt.min(5)).min(30))) => {} }
    };
    if app.session.read().await.is_none() { authorize(&app,&client,&cancel).await?; }
    let session = app.session.read().await.clone().context("Session missing")?;
    let cloud = Arc::new(Cloud {app:app.clone(),client,url:crate::tools::string(&info,"supabaseUrl")?.trim_end_matches('/').into(),
        key:crate::tools::string(&info,"supabasePublishableKey")?.into(),user:String::new(),offset:AtomicI64::new(0)});
    // 获取用户使用服务端验证，避免依赖设备本地时间判断 JWT 是否过期。
    let user = match cloud.request(reqwest::Method::GET,"/auth/v1/user",None).await {
        Ok(user) => user,
        Err(_) => { cloud.refresh().await?; cloud.request(reqwest::Method::GET,"/auth/v1/user",None).await? }
    };
    let user_id = crate::tools::string(&user,"id")?.to_owned();
    let mut cloud = Arc::try_unwrap(cloud).map_err(|_|anyhow::anyhow!("Unexpected cloud reference"))?;
    cloud.user = user_id; let cloud = Arc::new(cloud);
    let rows = cloud.request(reqwest::Method::GET,&format!("/rest/v1/mcp_devices?id=eq.{}&select=id",session.device_id),None).await?;
    ensure!(rows.as_array().is_some_and(|rows|!rows.is_empty()),"Authorized device no longer exists; authorize again");
    app.status.write().await.device_id = session.device_id;
    cloud.device(false).await?;
    let sleep_guard = crate::platform::SleepGuard::new(settings.prevent_sleep)?;
    attempt = 0;
    while !cancel.is_cancelled() {
        let result = connected(cloud.clone(),&cancel).await;
        if cancel.is_cancelled() { break; }
        cloud.device(false).await?;
        attempt += 1;
        let detail = result.err().map(|e|format!("{e:#}")).unwrap_or_else(||"Connection closed".into());
        app.state("reconnecting",detail).await;
        let delay = (1u64 << attempt.min(5)).min(30) * 500 + rand::random_range(0..1000);
        tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(delay)) => {} }
    }
    drop(sleep_guard);
    cloud.device(false).await?;
    Ok(())
}

async fn connected(cloud: Arc<Cloud>, cancel: &CancellationToken) -> Result<()> {
    let session = cloud.app.session.read().await.clone().context("Session missing")?;
    let mut url = reqwest::Url::parse(&format!("{}/realtime/v1/websocket",cloud.url))?;
    url.set_scheme("wss").map_err(|_|anyhow::anyhow!("Invalid WebSocket URL"))?;
    url.query_pairs_mut().append_pair("apikey",&cloud.key).append_pair("vsn","1.0.0");
    let response = cloud.client.get(url).upgrade().send().await?;
    let mut socket = response.into_websocket().await?;
    let topic = format!("realtime:user:{}",cloud.user);
    let frame = |event:&str,payload:Value,reference:u64| json!({"topic":topic,"event":event,"payload":payload,"ref":reference.to_string(),"join_ref":"1"});
    socket.send(Message::Text(frame("phx_join",json!({"config":{"broadcast":{"ack":true,"self":false},
        "presence":{"key":session.device_id,"enabled":true},"private":true},"access_token":session.access_token}),1).to_string())).await?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(25)); heartbeat.tick().await;
    let mut database = tokio::time::interval(Duration::from_secs(300)); database.tick().await;
    let mut recovery = tokio::time::interval(Duration::from_secs(30)); recovery.tick().await;
    let mut refresh = tokio::time::interval(Duration::from_secs(45*60)); refresh.tick().await;
    let mut watchdog = tokio::time::interval(Duration::from_secs(5));
    let mut last_ack = Instant::now(); let joined_at = Instant::now();
    let mut ready = false; let mut reference = 3u64;
    let mut tasks = tokio::task::JoinSet::new();
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                message = socket.next() => {
                    let message = message.context("WebSocket closed")??;
                    let Message::Text(text) = message else { continue; };
                    let message: Value = serde_json::from_str(&text)?;
                    if message["event"] == "phx_error" || message["event"] == "phx_close" { bail!("Realtime channel closed"); }
                    if message["event"] == "phx_reply" {
                        ensure!(message["payload"]["status"] == "ok","Realtime request rejected: {}",message["payload"]);
                        last_ack = Instant::now();
                        if message["ref"] == "1" {
                            socket.send(Message::Text(frame("presence",json!({"type":"presence","event":"track","payload":{
                                "device_id":session.device_id,"device_name":cloud.app.settings.read().await.device_name,
                                "app_version":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS}}),2).to_string())).await?;
                        }
                        if message["ref"] == "2" {
                            cloud.device(true).await?; ready = true;
                            cloud.app.state("online","远程连接正常").await;
                            cloud.flush_results().await?;
                            for id in cloud.pending().await?.into_iter().take(4) {
                                let active = cloud.clone(); tasks.spawn(async move { active.execute(&id).await });
                            }
                        }
                    }
                    if ready && message["event"] == "broadcast" && message["payload"]["event"] == "new_call" {
                        let payload = &message["payload"]["payload"];
                        if payload["device_id"] != session.device_id || tasks.len() >= 4 { continue; }
                        let id = crate::tools::string(payload,"call_id")?.to_owned(); uuid::Uuid::parse_str(&id)?;
                        let active = cloud.clone(); tasks.spawn(async move { active.execute(&id).await });
                    }
                }
                _ = heartbeat.tick() => {
                    reference += 1;
                    socket.send(Message::Text(json!({"topic":"phoenix","event":"heartbeat","payload":{},"ref":reference.to_string()}).to_string())).await?;
                }
                _ = database.tick(), if ready => { cloud.device(true).await?; }
                _ = recovery.tick(), if ready => {
                    cloud.flush_results().await?;
                    for id in cloud.pending().await?.into_iter().take(4usize.saturating_sub(tasks.len())) {
                        let active = cloud.clone(); tasks.spawn(async move { active.execute(&id).await });
                    }
                }
                _ = refresh.tick() => {
                    cloud.refresh().await?;
                    reference += 1;
                    let token = cloud.app.session.read().await.as_ref().context("Session missing")?.access_token.clone();
                    socket.send(Message::Text(frame("access_token",json!({"access_token":token}),reference).to_string())).await?;
                }
                _ = watchdog.tick() => {
                    ensure!(last_ack.elapsed() < Duration::from_secs(75),"Realtime heartbeat expired");
                    ensure!(ready || joined_at.elapsed() < Duration::from_secs(30),"Realtime join timed out");
                }
                task = tasks.join_next(), if !tasks.is_empty() => { match task.context("Missing call task")? { Ok(Ok(())) => {}, Ok(Err(e)) => tracing::error!("Remote call failed: {e:#}"), Err(e) => return Err(e.into()) } }
            }
        }
        Ok(())
    }.await;
    while let Some(task) = tasks.join_next().await { if let Err(error) = task? { tracing::error!("Remote call failed: {error:#}"); } }
    let _ = socket.close().await;
    result
}
