use anyhow::{Result, Context, ensure, bail};
use base64::Engine;
use pdfium_render::prelude::*;
use quick_xml::{Reader, Writer, events::Event};
use serde_json::{Value, json};
use std::{fs::File, io::{Read, Write, Cursor}, path::{Path, PathBuf}, sync::Arc, time::Duration};

pub async fn invoke(app: &Arc<crate::app::App>, name: &str, args: &Value) -> Result<Value> {
    use tokio::io::AsyncWriteExt;
    let _permit = app.tools.documents.acquire().await?;
    let work = app.dir.join("work"); crate::app::private_dir(&work)?;
    let temp = tempfile::tempdir_in(work)?;
    let tools = app.settings.read().await.tools.clone();
    let mut args = args.clone();
    let file = args["path"].as_str().or(args["file_path"].as_str()).context("Document path missing")?;
    if Path::new(file).extension().is_some_and(|ext|ext.eq_ignore_ascii_case("pdf")) {
        let base = Path::new(file).parent().context("Document parent missing")?.to_owned();
        if let Some(markdown) = args["content"].as_str() {
            args["content"] = json!(prepare_markdown(app,markdown,&base,temp.path()).await?);
        } else if let Some(operations) = args["content"].as_array_mut() {
            for operation in operations {
                if let Some(markdown) = operation["markdown"].as_str() {
                    operation["markdown"] = json!(prepare_markdown(app,markdown,&base,temp.path()).await?);
                }
            }
        }
    }
    let mut child = tokio::process::Command::new(std::env::current_exe()?).arg("document-worker")
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true).spawn()?;
    let mut stdin = child.stdin.take().context("Document worker stdin missing")?;
    stdin.write_all(serde_json::to_string(&json!({"name":name,"args":args,"tools":tools,"work":temp.path()}))?.as_bytes()).await?;
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(120),child.wait_with_output()).await.context("Document processing exceeded 120 seconds")??;
    ensure!(output.status.success(),"Document worker failed: {}",String::from_utf8_lossy(&output.stderr));
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn prepare_markdown(app:&Arc<crate::app::App>,markdown:&str,base:&Path,work:&Path) -> Result<String> {
    use futures_util::StreamExt;
    use pulldown_cmark::{Parser,Options,Event as MarkdownEvent,Tag};
    let mut events:Vec<_> = Parser::new_ext(markdown,Options::all()).map(MarkdownEvent::into_static).collect();
    let settings = app.settings.read().await.clone();
    let client = crate::remote::http_client(&settings)?;
    for event in &mut events {
        if let MarkdownEvent::Start(Tag::Image {dest_url,..}) = event {
            let source = dest_url.to_string();
            let bytes = if source.starts_with("https://") || source.starts_with("http://") {
                let response = client.get(&source).timeout(Duration::from_secs(15)).send().await?.error_for_status()?;
                let mut stream = response.bytes_stream(); let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {let chunk=chunk?;
                    ensure!(bytes.len()+chunk.len() <= 8*1024*1024,"Markdown image exceeds 8 MiB"); bytes.extend_from_slice(&chunk);}
                bytes
            } else {
                let path = if source.starts_with("file://") {reqwest::Url::parse(&source)?.to_file_path().map_err(|_|anyhow::anyhow!("Invalid file URL"))?}
                    else {base.join(&source)};
                let path = crate::tools::checked_path(path.to_str().context("Invalid image path")?,&settings.tools)?;
                ensure!(tokio::fs::metadata(&path).await?.len() <= 8*1024*1024,"Markdown image exceeds 8 MiB");
                tokio::fs::read(path).await?
            };
            let name = format!("asset-{}",uuid::Uuid::new_v4());
            tokio::fs::write(work.join(&name),bytes).await?;
            *dest_url = name.into();
        }
    }
    let mut normalized = String::new();
    pulldown_cmark_to_cmark::cmark(events.iter(),&mut normalized)?;
    Ok(normalized)
}

pub fn worker() -> Result<()> {
    let input: Value = serde_json::from_reader(std::io::stdin().lock())?;
    let name = crate::tools::string(&input,"name")?;
    let args = &input["args"];
    let path = args["file_path"].as_str().or(args["path"].as_str()).context("Document path missing")?;
    let path = crate::tools::checked_path(path,&input["tools"])?;
    if path.exists() { ensure!(path.metadata()?.len() <= 128 * 1024 * 1024,"Document exceeds 128 MiB"); }
    let extension = path.extension().and_then(|e|e.to_str()).unwrap_or("").to_lowercase();
    let result = match extension.as_str() {
        "docx" => docx(name,args,&path),
        "xlsx" => excel(name,args,&path),
        "pdf" => pdf(name,args,&path,Path::new(crate::tools::string(&input,"work")?),&input["tools"]),
        "png"|"jpg"|"jpeg"|"gif"|"webp"|"bmp"|"tiff" => image(name,args,&path),
        _ => bail!("Unsupported document format"),
    }?;
    serde_json::to_writer(std::io::stdout().lock(),&result)?;
    Ok(())
}

fn image(name:&str,args:&Value,path:&Path) -> Result<Value> {
    ensure!(name == "read_file","Unsupported image operation");
    ensure!(path.metadata()?.len() <= 16*1024*1024,"Image exceeds 16 MiB");
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default(); limits.max_alloc = Some(128*1024*1024); reader.limits(limits);
    let image = reader.decode()?.thumbnail(2048,2048);
    let mut buffer = Cursor::new(Vec::new()); image.write_to(&mut buffer,image::ImageFormat::Png)?;
    let data = base64::engine::general_purpose::STANDARD.encode(buffer.into_inner());
    let content = if args["origin"] == "ui" {json!([{"type":"text","text":data}])}
        else {json!([{"type":"image","mimeType":"image/png","data":data}])};
    Ok(json!({"content":content,"structuredContent":{"fileName":path.file_name().unwrap_or_default(),"filePath":path,"fileType":"image","mimeType":"image/png"}}))
}

fn pretty_xml(source: &str) -> Result<String> {
    let mut reader = Reader::from_str(source);
    let mut writer = Writer::new_with_indent(Vec::new(), b' ',2);
    loop { let event = reader.read_event()?; if matches!(event,Event::Eof) { break; } writer.write_event(event)?; }
    Ok(String::from_utf8(writer.into_inner())?)
}

fn docx(name: &str,args: &Value,path: &Path) -> Result<Value> {
    if name == "write_file" {
        ensure!(args["mode"] != "append","DOCX append is unsupported; use edit_block");
        let mut document = docx_rs::Docx::new();
        for line in crate::tools::string(args,"content")?.lines() {
            let mut paragraph = docx_rs::Paragraph::new();
            if let Some(pulldown_cmark::Event::Start(pulldown_cmark::Tag::Heading{level,..})) = pulldown_cmark::Parser::new(line).next() {
                let text:String = pulldown_cmark::Parser::new(line).filter_map(|event|match event {pulldown_cmark::Event::Text(text)|pulldown_cmark::Event::Code(text)=>Some(text.to_string()),_=>None}).collect();
                paragraph = paragraph.style(&format!("Heading{}",level as u8)).add_run(docx_rs::Run::new().add_text(text));
            } else {paragraph=paragraph.add_run(docx_rs::Run::new().add_text(line));}
            document = document.add_paragraph(paragraph);
        }
        document.build().pack(File::create(path)?)?;
        return Ok(crate::tools::text("DOCX written"));
    }
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    let mut xml = String::new(); archive.by_name("word/document.xml")?.take(32*1024*1024).read_to_string(&mut xml)?;
    let pretty = pretty_xml(&xml)?;
    if name == "read_file" {
        let lines: Vec<_> = pretty.lines().collect();
        let offset = args["offset"].as_i64().unwrap_or(0);
        let start = if offset < 0 {lines.len().saturating_sub(offset.unsigned_abs() as usize)} else {offset as usize};
        let content = lines.into_iter().skip(start).take(crate::tools::number(args,"length",1000).min(10_000) as usize).collect::<Vec<_>>().join("\n");
        return Ok(json!({"content":[{"type":"text","text":content}],"structuredContent":{
            "fileName":path.file_name().unwrap_or_default(),"filePath":path,"fileType":"unsupported"}}));
    }
    if name == "get_file_info" { return Ok(crate::tools::result(json!({"size":path.metadata()?.len(),"isFile":true,"isDirectory":false,"metadata":{"isDocx":true,"xmlParts":archive.len()}}))); }
    ensure!(name == "edit_block","Unsupported DOCX operation");
    let old = crate::tools::string(args,"old_string")?; ensure!(!old.is_empty(),"old_string is empty");
    let new = crate::tools::string(args,"new_string")?;
    let expected = crate::tools::number(args,"expected_replacements",1) as usize;
    let mut edits = std::collections::HashMap::new(); let mut matches = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.name() == "word/document.xml" || ((entry.name().starts_with("word/header") || entry.name().starts_with("word/footer")) && entry.name().ends_with(".xml")) {
            let mut source = String::new(); entry.read_to_string(&mut source)?;
            let source = pretty_xml(&source)?; let count = source.matches(old).count();
            if count > 0 { let updated = source.replace(old,new); pretty_xml(&updated)?;
                edits.insert(entry.name().to_owned(),updated.into_bytes()); matches += count; }
        }
    }
    ensure!(matches == expected,"Expected {expected} occurrences, found {matches}");
    let pending = path.with_extension(format!("{}.pending",uuid::Uuid::new_v4()));
    let mut output = zip::ZipWriter::new(File::create(&pending)?);
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        if let Some(bytes) = edits.get(entry.name()) {
            output.start_file(entry.name(),zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))?;
            output.write_all(bytes)?;
        } else { output.raw_copy_file(entry)?; }
    }
    output.finish()?.sync_all()?; drop(archive);
    atomicwrites::replace_atomic(&pending,path)?;
    Ok(crate::tools::result(json!({"success":true,"editsApplied":matches})))
}

fn write_cells(sheet: &mut umya_spreadsheet::Worksheet, content: &Value, col: u32,row: u32) -> Result<()> {
    for (r,values) in content.as_array().context("Expected 2D array")?.iter().enumerate() {
        for (c,value) in values.as_array().context("Expected array row")?.iter().enumerate() {
            let cell = sheet.get_cell_mut((col+c as u32,row+r as u32));
            match value {
                Value::Null => { cell.set_value_string(""); },
                Value::Bool(b) => { cell.set_value_bool(*b); },
                Value::Number(n) => { cell.set_value_number(n.as_f64().context("Invalid number")?); },
                Value::String(s) if s.starts_with('=') => { cell.set_formula(&s[1..]); },
                Value::String(s) => { cell.set_value_string(s); },
                _ => bail!("Unsupported cell value"),
            }
        }
    }
    Ok(())
}

fn excel(name: &str,args: &Value,path: &Path) -> Result<Value> {
    let mut book = if name == "write_file" && args["mode"] != "append" { umya_spreadsheet::new_file() } else { umya_spreadsheet::reader::xlsx::read(path)? };
    if name == "get_file_info" {
        return Ok(crate::tools::result(json!({"size":path.metadata()?.len(),"isFile":true,"isDirectory":false,
            "metadata":{"isExcel":true,"sheets":book.get_sheet_collection().iter().map(|s|json!({"name":s.get_name(),"rows":s.get_highest_row(),"columns":s.get_highest_column()})).collect::<Vec<_>>()}})));
    }
    if name == "write_file" {
        let content: Value = serde_json::from_str(crate::tools::string(args,"content")?)?;
        let sheets = if content.is_array() { vec![("Sheet1".to_owned(),content)] } else {
            content.as_object().context("Expected 2D array or sheet map")?.iter().map(|(k,v)|(k.clone(),v.clone())).collect()
        };
        for (name,data) in sheets {
            if book.get_sheet_by_name(&name).is_none() { book.new_sheet(&name).map_err(anyhow::Error::msg)?; }
            let sheet = book.get_sheet_by_name_mut(&name).context("Sheet missing")?;
            let row = if args["mode"] == "append" {sheet.get_highest_row()+1} else {1};
            write_cells(sheet,&data,1,row)?;
        }
    } else {
        let range = args["range"].as_str().unwrap_or("");
        let (embedded, cells) = range.rsplit_once('!').map(|(s,r)|(Some(s.trim_matches('\'').replace("''","'")),r)).unwrap_or((None,range));
        let sheet_name = args["sheet"].as_str().map(str::to_owned).or(embedded)
            .unwrap_or_else(||book.get_sheet(&0).expect("Workbook is empty").get_name().to_owned());
        if name == "edit_block" && book.get_sheet_by_name(&sheet_name).is_none() { book.new_sheet(&sheet_name).map_err(anyhow::Error::msg)?; }
        let sheet = book.get_sheet_by_name_mut(&sheet_name).context("Unknown worksheet")?;
        let mut start_col = 1; let mut start_row = 1; let mut end_col = sheet.get_highest_column().max(1); let mut end_row = sheet.get_highest_row().max(1);
        if !cells.is_empty() {
            let mut range = umya_spreadsheet::structs::Range::default(); range.set_range(cells.to_uppercase());
            start_col = *range.get_coordinate_start_col().context("Invalid range column")?.get_num();
            start_row = *range.get_coordinate_start_row().context("Invalid range row")?.get_num();
            end_col = range.get_coordinate_end_col().map(|c|*c.get_num()).unwrap_or(start_col);
            end_row = range.get_coordinate_end_row().map(|r|*r.get_num()).unwrap_or(start_row);
            ensure!(start_col > 0 && start_row > 0 && end_col >= start_col && end_row >= start_row,"Invalid cell range");
        }
        if name == "read_file" {
            let total = end_row-start_row+1; let offset = args["offset"].as_i64().unwrap_or(0);
            start_row = if offset < 0 {end_row.saturating_sub(offset.unsigned_abs() as u32).saturating_add(1).max(start_row)} else {start_row.saturating_add(offset as u32)};
            end_row = end_row.min(start_row.saturating_add(crate::tools::number(args,"length",1000).min(10_000) as u32).saturating_sub(1));
            ensure!((end_col-start_col+1) as u64 * end_row.saturating_sub(start_row).saturating_add(1) as u64 <= 100_000,"Requested spreadsheet range exceeds 100000 cells");
            let mut data = Vec::new();
            for row in start_row..=end_row {
                let mut values = Vec::new();
                for col in start_col..=end_col {
                    let value = match sheet.get_cell((col,row)) {
                        None => Value::Null,
                        Some(cell) if cell.get_data_type() == "b" => json!(cell.get_value() == "1" || cell.get_value() == "TRUE"),
                        Some(cell) => cell.get_value_number().map(|n|json!(n)).unwrap_or_else(||json!(cell.get_value())),
                    }; values.push(value);
                } data.push(values);
            }
            return Ok(crate::tools::result(json!({"content":data,"metadata":{"isExcel":true,"sheetName":sheet_name,"totalRows":total,"returnedRows":data.len()}})));
        }
        ensure!(name == "edit_block","Unsupported spreadsheet operation");
        write_cells(sheet,&args["content"],start_col,start_row)?;
    }
    let pending = path.with_extension(format!("{}.xlsx",uuid::Uuid::new_v4()));
    umya_spreadsheet::writer::xlsx::write(&book,&pending)?; atomicwrites::replace_atomic(&pending,path)?;
    Ok(crate::tools::text("Spreadsheet saved"))
}

fn create_pdf(markdown: &str,options: &Value,output: &Path,work: &Path) -> Result<()> {
    let resources = crate::platform::resources()?;
    let source = work.join(format!("{}.md",uuid::Uuid::new_v4())); std::fs::write(&source,markdown)?;
    let mut command = std::process::Command::new(crate::platform::bundled("typst")?);
    command.arg("compile").arg("--root").arg(work.parent().context("Work parent missing")?)
        .arg("--font-path").arg(resources.join("fonts"));
    command.arg("--input").arg(format!("source={}",source.display()));
    if let Some(options) = options.as_object() {
        for (key,value) in options {
            ensure!(["pageWidth","pageHeight","margin","font"].contains(&key.as_str()),"Unsupported PDF option: {key}");
            let value = value.as_str().context("PDF options must be strings")?;
            if key != "font" { let mm: f64 = value.strip_suffix("mm").context("Dimensions must use mm")?.parse()?;
                ensure!(mm > 0.0 && mm <= 2000.0,"PDF dimension out of range"); }
            command.arg("--input").arg(format!("{key}={value}"));
        }
    }
    // 模板和插件位于应用资源目录，Markdown 保留为输入数据。
    let template = work.join("pdf.typ"); std::fs::copy(resources.join("pdf.typ"),&template)?;
    copy_directory(&resources.join("cmarker"),&work.join("cmarker"))?;
    command.arg(&template).arg(output);
    let result = command.output()?;
    ensure!(result.status.success(),"Typst failed: {}",String::from_utf8_lossy(&result.stderr));
    Ok(())
}

fn copy_directory(from: &Path,to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? { let entry = entry?; let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {copy_directory(&entry.path(),&target)?;} else {std::fs::copy(entry.path(),target)?;} }
    Ok(())
}

fn pdf(name: &str,args: &Value,path: &Path,work: &Path,config: &Value) -> Result<Value> {
    let resources = crate::platform::resources()?;
    let pdfium = Pdfium::new(Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&resources))?);
    if name == "write_file" || (name == "write_pdf" && args["content"].is_string()) {
        create_pdf(crate::tools::string(args,"content")?,&args["options"],path,work)?;
        return Ok(crate::tools::text("PDF generated"));
    }
    let mut document = pdfium.load_pdf_from_file(path,None)?;
    if name == "get_file_info" {
        return Ok(crate::tools::result(json!({"size":path.metadata()?.len(),"isFile":true,"isDirectory":false,"metadata":{"isPdf":true,"totalPages":document.pages().len()}})));
    }
    if name == "read_file" {
        let count = document.pages().len() as usize; let offset = args["offset"].as_i64().unwrap_or(0);
        let start = if offset < 0 {count.saturating_sub(offset.unsigned_abs() as usize)} else {offset as usize};
        let length = crate::tools::number(args,"length",10).min(100) as usize; let mut pages = Vec::new();
        let mut bytes = 0;
        for index in start..start.saturating_add(length).min(count) {
            let page = document.pages().get(index as u16)?; let text = page.text()?.all(); let mut images = Vec::new();
            for (object_id,object) in page.objects().iter().enumerate() {
                if let Some(image) = object.as_image_object() {
                    let image = image.get_raw_image()?.thumbnail(2048,2048);
                    let (width,height) = (image.width(),image.height()); let mut buffer = Cursor::new(Vec::new());
                    image.write_to(&mut buffer,image::ImageFormat::Png)?;
                    let data = base64::engine::general_purpose::STANDARD.encode(buffer.into_inner()); bytes += data.len();
                    ensure!(bytes <= 24*1024*1024,"PDF image response exceeds 24 MiB; request fewer pages");
                    images.push(json!({"objId":object_id,"width":width,"height":height,"mimeType":"image/png","data":data}));
                }
            }
            pages.push(json!({"text":text,"images":images,"pageNumber":index+1}));
        }
        let mut content = vec![json!({"type":"text","text":format!("PDF file: {} ({count} pages)",path.display())})];
        for mut page in pages {
            let Value::Array(images) = page["images"].take() else { bail!("PDF images missing"); };
            for mut image in images {
                content.push(json!({"type":"image","mimeType":image["mimeType"].take(),"data":image["data"].take()}));
            }
            content.push(json!({"type":"text","text":format!("<!-- Page: {} -->\n{}",page["pageNumber"],page["text"].as_str().unwrap_or_default())}));
        }
        return Ok(json!({"content":content,"structuredContent":{"fileName":path.file_name().unwrap_or_default(),
            "filePath":path,"fileType":"unsupported"}}));
    }
    ensure!(name == "write_pdf" || name == "edit_block","Unsupported PDF operation");
    for operation in args["content"].as_array().context("PDF operations must be an array")? {
        let count = document.pages().len() as i64;
        match crate::tools::string(operation,"type")? {
            "delete" => {
                let mut indices = operation["pageIndexes"].as_array().context("pageIndexes missing")?.iter().map(|n| {
                    let index = n.as_i64().context("Invalid page index")?; let index = if index < 0 {count+index} else {index};
                    ensure!(index >= 0 && index < count,"Page index out of range"); Ok(index as u16)
                }).collect::<Result<Vec<_>>>()?;
                indices.sort_unstable(); indices.dedup(); indices.reverse();
                for index in indices { document.pages().get(index)?.delete()?; }
            }
            "insert" => {
                let index = operation["pageIndex"].as_i64().context("pageIndex missing")?; let index = if index < 0 {count+index} else {index};
                ensure!(index >= 0 && index <= count,"Page index out of range");
                let source = if let Some(markdown) = operation["markdown"].as_str() {
                    let source = work.join(format!("{}.pdf",uuid::Uuid::new_v4()));
                    create_pdf(markdown,&operation["pdfOptions"],&source,work)?; source
                } else { crate::tools::checked_path(crate::tools::string(operation,"sourcePdfPath")?,config)? };
                let source = pdfium.load_pdf_from_file(&source,None)?;
                document.pages_mut().copy_pages_from_document(&source,&format!("1-{}",source.pages().len()),index as u16)?;
            }
            kind => bail!("Unknown PDF operation: {kind}"),
        }
    }
    let destination: PathBuf = args["outputPath"].as_str().map(|p|crate::tools::checked_path(p,config)).transpose()?.unwrap_or_else(||path.to_owned());
    let pending = destination.with_extension(format!("{}.pdf",uuid::Uuid::new_v4())); document.save_to_file(&pending)?;
    drop(document); atomicwrites::replace_atomic(&pending,&destination)?;
    Ok(crate::tools::text("PDF saved"))
}
