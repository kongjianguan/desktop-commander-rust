use std::sync::Arc;
use rmcp::{ServerHandler, ServiceExt, RoleServer, ErrorData, model::*, service::RequestContext};
use serde_json::{Value, json};

#[derive(Clone)]
struct Server(Arc<crate::app::App>);
fn protocol(error: impl std::fmt::Display) -> ErrorData { ErrorData::internal_error(error.to_string(),None) }

impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo { capabilities: serde_json::from_value(json!({"tools":{},"resources":{},"prompts":{}})).expect("Invalid capabilities"),
            instructions:Some("Desktop Commander provides local filesystem, terminal, search, and native document tools.".into()),
            ..Default::default() }
    }
    async fn list_tools(&self,_:Option<PaginatedRequestParam>,_:RequestContext<RoleServer>) -> Result<ListToolsResult,ErrorData> {
        serde_json::from_value(json!({"tools":self.0.tools.definitions})).map_err(protocol)
    }
    async fn call_tool(&self,request:CallToolRequestParam,_:RequestContext<RoleServer>) -> Result<CallToolResult,ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        serde_json::from_value(crate::tools::Tools::call(self.0.clone(),&request.name,args).await).map_err(protocol)
    }
    async fn list_resources(&self,_:Option<PaginatedRequestParam>,_:RequestContext<RoleServer>) -> Result<ListResourcesResult,ErrorData> {
        serde_json::from_value(json!({"resources":[
            {"uri":"ui://desktop-commander/file-preview","name":"Desktop Commander File Preview","mimeType":"text/html;profile=mcp-app"},
            {"uri":"ui://desktop-commander/config-editor","name":"Desktop Commander Config Editor","mimeType":"text/html;profile=mcp-app"}
        ]})).map_err(protocol)
    }
    async fn read_resource(&self,request:ReadResourceRequestParam,_:RequestContext<RoleServer>) -> Result<ReadResourceResult,ErrorData> {
        let source = match request.uri.as_str() {
            "ui://desktop-commander/file-preview" => include_str!("../reference/file-preview.json"),
            "ui://desktop-commander/config-editor" => include_str!("../reference/config-editor.json"),
            _ => return Err(ErrorData::invalid_params("Unknown resource",None)),
        };
        serde_json::from_str(source).map_err(protocol)
    }
}

pub async fn serve(app:Arc<crate::app::App>) -> anyhow::Result<()> {
    let service = Server(app.clone()).serve(rmcp::transport::stdio()).await?;
    let cancel = service.cancellation_token();
    tokio::select! {
        result=service.waiting()=>{result?;}
        _=app.shutdown.cancelled()=>{cancel.cancel();}
    }
    app.tools.terminal.shutdown().await?;
    Ok(())
}
