use anyhow::{Result, Context, bail, ensure};
use serde_json::{Value, json};
use std::{collections::HashMap, path::{Path, PathBuf}, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use futures_util::StreamExt;

pub struct Tools {
    pub terminal: crate::terminal::Terminal,
    pub searches: crate::search::Searches,
    pub definitions: Vec<Value>,
    validators: HashMap<String, jsonschema::Validator>,
    pub documents: tokio::sync::Semaphore,
}

pub fn text(value: impl AsRef<str>) -> Value { json!({"content":[{"type":"text","text":value.as_ref()}]}) }
pub fn result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).expect("JSON serialization failed");
    let structured = if value.is_object() {value} else {json!({"data":value})};
    json!({"content":[{"type":"text","text":text}], "structuredContent": structured})
}
pub fn string<'a>(args: &'a Value, field: &str) -> Result<&'a str> { args[field].as_str().with_context(|| format!("Missing string: {field}")) }
pub fn number(args: &Value, field: &str, default: u64) -> u64 { args[field].as_u64().unwrap_or(default) }

impl Tools {
    pub fn new(dir: &Path) -> Result<Self> {
        let mut definitions: Vec<Value> = serde_json::from_str(include_str!("../reference/tools.json"))?;
        for tool in &mut definitions {
            if tool["name"] == "write_pdf" {
                tool["description"] = json!("Generate PDF from CommonMark Markdown with native Typst typography, or insert/delete PDF pages. HTML/CSS and browser launch options are unsupported.");
                tool["inputSchema"]["properties"]["options"] = json!({"type":"object", "additionalProperties":false,
                    "properties":{"pageWidth":{"type":"string"},"pageHeight":{"type":"string"},
                    "margin":{"type":"string"},"font":{"type":"string"}}});
            }
        }
        let validators = definitions.iter().map(|tool| {
            let name = tool["name"].as_str().context("Tool name missing")?.to_owned();
            Ok((name, jsonschema::validator_for(&tool["inputSchema"])?))
        }).collect::<Result<_>>()?;
        Ok(Self { terminal: Default::default(), searches: crate::search::Searches::new(dir)?, definitions, validators,
            documents: tokio::sync::Semaphore::new(1) })
    }

    pub async fn call(app: Arc<crate::app::App>, name: &str, args: Value) -> Value {
        let started = std::time::Instant::now();
        let response: Result<Value> = async {
            let validator = app.tools.validators.get(name).context("Unknown tool")?;
            validator.validate(&args).map_err(|error| anyhow::anyhow!("Invalid arguments: {error}"))?;
            let _permit = app.slots.acquire().await?;
            Self::dispatch(&app, name, &args).await
        }.await;
        let response = match response { Ok(value) => value, Err(error) => json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true}) };
        let record = json!({"timestamp":chrono::Utc::now().to_rfc3339(),"toolName":name,
            "success":response["isError"] != true,"durationMs":started.elapsed().as_millis()});
        if let Err(error) = history(&app, &record).await { tracing::error!("History write failed: {error:#}"); }
        app.event(record).await;
        response
    }

    async fn dispatch(app: &Arc<crate::app::App>, name: &str, args: &Value) -> Result<Value> {
        let prefs = app.settings.read().await.clone();
        let path = |field: &str| checked_path(string(args, field)?, &prefs.tools);
        let lines = number(&prefs.tools, "fileReadLineLimit", 1000).min(10_000) as usize;
        match name {
            "get_config" => {
                let mut config = prefs.tools.clone();
                config["version"] = json!(env!("CARGO_PKG_VERSION"));
                config["systemInfo"] = json!({"platform": std::env::consts::OS,"arch":std::env::consts::ARCH});
                let entries:Vec<_> = config.as_object().context("Invalid tool configuration")?.iter().filter(|(key,_)|key.as_str() != "systemInfo" && key.as_str() != "version")
                    .map(|(key,value)|json!({"key":key,"value":value,"valueType":if value.is_array(){"array"}else if value.is_boolean(){"boolean"}else if value.is_number(){"number"}else{"string"},
                        "editable":key != "telemetryEnabled"})).collect();
                Ok(json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&config)?}],
                    "structuredContent":{"config":config,"entries":entries,"uiHints":{"availableShells":[prefs.tools["defaultShell"]]}}}))
            }
            "set_config_value" => {
                let key = string(args, "key")?;
                ensure!(key != "telemetryEnabled","Telemetry settings are read-only");
                ensure!(prefs.tools.get(key).is_some(), "Unknown configuration key: {key}");
                let mut settings = prefs;
                let old = &settings.tools[key]; let value = &args["value"];
                ensure!((old.is_array() && value.is_array()) || (old.is_boolean() && value.is_boolean()) ||
                    (old.is_number() && value.as_u64().is_some_and(|v| v > 0 && v <= 10_000)) ||
                    (old.is_string() && value.is_string()), "Invalid configuration type or range");
                settings.tools[key] = value.clone(); app.set_settings(settings).await?; Ok(text("Configuration saved"))
            }
            "read_file" => {
                if args["isUrl"].as_bool().unwrap_or(false) {
                    let client = crate::remote::http_client(&prefs)?;
                    let response = client.get(string(args, "path")?).send().await?.error_for_status()?;
                    ensure!(response.content_length().unwrap_or(0) <= 8 * 1024 * 1024, "URL response exceeds 8 MiB");
                    use futures_util::StreamExt;
                    let mut stream = response.bytes_stream(); let mut bytes = Vec::new();
                    while let Some(chunk) = stream.next().await { let chunk = chunk?;
                        ensure!(bytes.len() + chunk.len() <= 8 * 1024 * 1024, "URL response exceeds 8 MiB"); bytes.extend_from_slice(&chunk); }
                    return Ok(text(String::from_utf8(bytes)?));
                }
                let file = path("path")?;
                if file.is_dir() { return directory_result(&file,2); }
                if document(&file) || image(&file) { return crate::documents::invoke(app, "read_file", args).await; }
                let offset = args["offset"].as_i64().unwrap_or(0);
                let length = number(args, "length", lines as u64).min(10_000) as usize;
                let mut reader = tokio_util::codec::FramedRead::new(tokio::fs::File::open(&file).await?,tokio_util::codec::LinesCodec::new_with_max_length(1024*1024));
                let mut output = std::collections::VecDeque::new(); let mut index = 0usize; let mut bytes = 0usize;
                while let Some(line) = reader.next().await {
                    let line = line?;
                    ensure!(line.len() <= 1024 * 1024, "Text line exceeds 1 MiB");
                    if offset < 0 { bytes += line.len(); output.push_back(line);
                        while output.len() > offset.unsigned_abs().min(10_000) as usize { bytes -= output.pop_front().unwrap().len(); }
                    } else if index >= offset as usize && output.len() < length { bytes += line.len(); output.push_back(line); }
                    ensure!(bytes <= 8 * 1024 * 1024, "Selected text exceeds 8 MiB");
                    index += 1;
                    if offset >= 0 && output.len() == length { break; }
                }
                let content = output.into_iter().take(length).collect::<Vec<_>>().join("\n");
                let mime = if file.extension().is_some_and(|ext| ext == "md") {"text/markdown"} else {"text/plain"};
                Ok(json!({"content":[{"type":"text","text":content}],"structuredContent":{
                    "filePath":file,"fileName":file.file_name().unwrap_or_default(),"fileType":if mime == "text/markdown" {"markdown"} else if file.extension().is_some_and(|ext|ext == "html") {"html"} else {"text"},
                    "mimeType":mime,"sourceTool":"read_file"}}))
            }
            "read_multiple_files" => {
                let mut results = Vec::new();
                for file in args["paths"].as_array().context("paths must be array")? {
                    let call = json!({"path":file});
                    results.push(Box::pin(Self::dispatch(app, "read_file", &call)).await?);
                }
                Ok(result(json!({"files":results})))
            }
            "write_file" => {
                let file = path("path")?;
                if document(&file) { return crate::documents::invoke(app, name, args).await; }
                let content = string(args, "content")?;
                let max = number(&prefs.tools, "fileWriteLineLimit", 1000) as usize;
                ensure!(content.lines().count() <= max, "Write exceeds fileWriteLineLimit ({max})");
                let append = args["mode"] == "append";
                let mut output = tokio::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&file).await?;
                output.write_all(content.as_bytes()).await?; output.sync_all().await?;
                Ok(result(json!({"path":file,"bytesWritten":content.len()})))
            }
            "write_pdf" => { path("path")?; if args["outputPath"].is_string() { path("outputPath")?; } crate::documents::invoke(app, name, args).await }
            "edit_block" => {
                let file = path("file_path")?;
                if document(&file) { return crate::documents::invoke(app, name, args).await; }
                ensure!(tokio::fs::metadata(&file).await?.len() <= 32 * 1024 * 1024, "Text edit exceeds 32 MiB");
                let source = tokio::fs::read_to_string(&file).await?;
                let old = string(args, "old_string")?; ensure!(!old.is_empty(), "old_string is empty");
                let count = source.matches(old).count();
                let expected = number(args,"expected_replacements",1) as usize;
                ensure!(count == expected, "Expected {expected} occurrences, found {count}");
                let updated = source.replace(old, string(args,"new_string")?);
                let pending = file.with_extension(format!("{}.pending",uuid::Uuid::new_v4()));
                tokio::fs::write(&pending,updated).await?; tokio::fs::set_permissions(&pending,tokio::fs::metadata(&file).await?.permissions()).await?;
                atomicwrites::replace_atomic(&pending,&file)?;
                Ok(result(json!({"path":file,"replacements":count})))
            }
            "create_directory" => { tokio::fs::create_dir_all(path("path")?).await?; Ok(text("Directory created")) }
            "move_file" => { tokio::fs::rename(path("source")?,path("destination")?).await?; Ok(text("File moved")) }
            "list_directory" => {
                let base = path("path")?; let depth = number(args,"depth",2).min(16) as usize;
                directory_result(&base,depth)
            }
            "get_file_info" => {
                let file = path("path")?;
                if document(&file) { return crate::documents::invoke(app,name,args).await; }
                let meta = tokio::fs::metadata(&file).await?;
                Ok(result(json!({"path":file,"size":meta.len(),"isDirectory":meta.is_dir(),"isFile":meta.is_file(),
                    "modified":meta.modified().ok().map(chrono::DateTime::<chrono::Utc>::from)})))
            }
            "start_process" => {
                let command = string(args,"command")?;
                if let Some(blocked) = prefs.tools["blockedCommands"].as_array() {
                    for item in blocked { if let Some(item) = item.as_str() { ensure!(!command.split_whitespace().any(|word|word == item),"Command is blocked: {item}"); } }
                }
                let shell = args["shell"].as_str().or(prefs.tools["defaultShell"].as_str()).context("defaultShell missing")?;
                Ok(result(app.tools.terminal.start(command,shell,number(args,"timeout_ms",1000)).await?))
            }
            "read_process_output" => Ok(result(app.tools.terminal.read(number(args,"pid",0) as u32,number(args,"timeout_ms",0),args["offset"].as_i64(),number(args,"length",lines as u64) as usize).await?)),
            "interact_with_process" => Ok(result(app.tools.terminal.interact(number(args,"pid",0) as u32,string(args,"input")?,number(args,"timeout_ms",1000)).await?)),
            "force_terminate" => Ok(result(app.tools.terminal.terminate(number(args,"pid",0) as u32).await?)),
            "list_sessions" => Ok(result(app.tools.terminal.list().await)),
            "list_processes" => {
                let system = sysinfo::System::new_all();
                Ok(result(json!(system.processes().iter().map(|(pid,p)|json!({"pid":pid.as_u32(),"command":p.name(),"cpu":p.cpu_usage(),"memory":p.memory()})).collect::<Vec<_>>())))
            }
            "kill_process" => { crate::platform::kill_tree(number(args,"pid",0) as u32)?; Ok(text("Process terminated")) }
            "start_search" => Ok(result(app.tools.searches.start(args,&path("path")?).await?)),
            "get_more_search_results" => Ok(result(app.tools.searches.more(string(args,"sessionId")?,number(args,"offset",0) as usize,number(args,"length",100) as usize).await?)),
            "stop_search" => Ok(result(app.tools.searches.stop(string(args,"sessionId")?).await?)),
            "list_searches" => Ok(result(app.tools.searches.list().await)),
            "get_recent_tool_calls" | "get_usage_stats" => {
                let mut records = recent(app).await?;
                if name == "get_usage_stats" { return Ok(result(json!({"recordedCalls":records.len(),"version":env!("CARGO_PKG_VERSION")}))); }
                records.retain(|r| args["toolName"].as_str().is_none_or(|n| r["toolName"] == n) &&
                    args["since"].as_str().is_none_or(|since| r["timestamp"].as_str().is_some_and(|t|t >= since)));
                let count = number(args,"maxResults",50).min(1000) as usize;
                records.reverse(); records.truncate(count); Ok(result(json!({"calls":records})))
            }
            "get_prompts" => {
                let prompts: Value = serde_json::from_str(include_str!("../reference/onboarding-prompts.json"))?;
                let id = string(args,"promptId")?;
                let prompt = prompts["prompts"].as_array().and_then(|items| items.iter().find(|p| p["id"] == id)).context("Unknown promptId")?;
                Ok(result(prompt.clone()))
            }
            "give_feedback_to_desktop_commander" => { open::that("https://desktopcommander.app/feedback")?; Ok(text("Feedback page opened")) }
            "track_ui_event" => Ok(text("UI event received")),
            _ => bail!("Unknown tool: {name}"),
        }
    }
}

pub fn checked_path(input: &str, config: &Value) -> Result<PathBuf> {
    let path = if input == "~" || input.starts_with("~/") {
        directories::BaseDirs::new().context("Home directory unavailable")?.home_dir().join(input.trim_start_matches('~').trim_start_matches('/'))
    } else { PathBuf::from(input) };
    ensure!(path.is_absolute(), "An absolute path is required");
    let path = path_clean::clean(path);
    if let Some(allowed) = config["allowedDirectories"].as_array().filter(|dirs| !dirs.is_empty()) {
        let mut ancestor = path.as_path();
        while !ancestor.exists() { ancestor = ancestor.parent().context("Invalid destination")?; }
        let canonical = ancestor.canonicalize()?;
        let mut permitted = false;
        for base in allowed { if canonical.starts_with(Path::new(base.as_str().context("Invalid allowed directory")?).canonicalize()?) { permitted = true; break; } }
        ensure!(permitted,"Path is outside allowedDirectories");
    }
    Ok(path)
}
fn document(path: &Path) -> bool { matches!(path.extension().and_then(|s|s.to_str()).unwrap_or("").to_lowercase().as_str(),"pdf"|"docx"|"xlsx") }
fn image(path: &Path) -> bool { matches!(path.extension().and_then(|s|s.to_str()).unwrap_or("").to_lowercase().as_str(),"png"|"jpg"|"jpeg"|"webp"|"gif"|"bmp"|"tiff") }

fn directory_result(base:&Path,depth:usize) -> Result<Value> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(base).max_depth(depth).sort_by_file_name().follow_links(false).into_iter().skip(1) {
        let entry = entry?;
        ensure!(entries.len() < 10_000,"Directory listing exceeds 10000 entries; reduce depth");
        entries.push(format!("[{}] {}",if entry.file_type().is_dir(){"DIR"}else{"FILE"},entry.path().strip_prefix(base)?.display()));
    }
    Ok(json!({"content":[{"type":"text","text":entries.join("\n")}],"structuredContent":{
        "filePath":base,"fileName":base.file_name().unwrap_or_default(),"fileType":"directory"}}))
}

async fn history(app: &crate::app::App, record: &Value) -> Result<()> {
    let _guard = app.history_lock.lock().await;
    let path = app.dir.join("history.jsonl");
    if path.exists() && tokio::fs::metadata(&path).await?.len() >= 1024 * 1024 {
        let old = app.dir.join("history.previous.jsonl"); if old.exists() { tokio::fs::remove_file(&old).await?; }
        tokio::fs::rename(&path,old).await?;
    }
    let mut file = tokio::fs::OpenOptions::new().append(true).create(true).open(path).await?;
    file.write_all(serde_json::to_string(record)?.as_bytes()).await?; file.write_all(b"\n").await?;
    Ok(())
}
pub async fn recent(app: &crate::app::App) -> Result<Vec<Value>> {
    let _guard = app.history_lock.lock().await;
    let mut records = std::collections::VecDeque::new();
    for name in ["history.previous.jsonl","history.jsonl"] {
        let path = app.dir.join(name); if !path.exists() { continue; }
        let mut lines = BufReader::new(tokio::fs::File::open(path).await?).lines();
        while let Some(line) = lines.next_line().await? { records.push_back(serde_json::from_str(&line)?); while records.len() > 1000 { records.pop_front(); } }
    }
    Ok(records.into_iter().collect())
}
