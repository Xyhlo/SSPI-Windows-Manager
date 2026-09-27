use super::*;
use std::collections::HashSet;

const MAX_PNG: usize = 2 * 1024 * 1024;
const MAX_META: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LibraryTitle {
    title_id: String,
    name: String,
    version: Option<String>,
    base_version: Option<String>,
    update_version: Option<String>,
    icon: Option<String>,
    custom_icon: bool,
    required_firmware: Option<String>,
    content_id: Option<String>,
    platform: String,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConsoleLibrarySnapshot {
    target: String,
    entries: Vec<LibraryTitle>,
    complete: bool,
    truncated: bool,
    errors: Vec<String>,
    metadata_warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IconWriteResult {
    title_id: String,
    written: u32,
    backed_up: bool,
    refresh: String,
    message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConsoleStorage { label: String, path: String, total_bytes: u64, free_bytes: u64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConsoleMemory { total_bytes: u64, free_bytes: u64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ConsoleNetwork { ip: Option<String>, mac: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct InstalledTheme { content_id: String, title: String }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConsoleThemes { themes: Vec<InstalledTheme>, active_content_id: Option<String>, truncated: bool }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConsoleSystemInfo {
    target: String,
    receiver_version: String,
    firmware: Option<String>,
    sdk_version: Option<String>,
    model: Option<String>,
    console_name: Option<String>,
    uptime_seconds: Option<u64>,
    cpu_temp_c: Option<f64>,
    soc_temp_c: Option<f64>,
    storage: Vec<ConsoleStorage>,
    memory: Option<ConsoleMemory>,
    network: Option<ConsoleNetwork>,
    running_title_id: Option<String>,
    capabilities: Vec<String>,
    extras: HashMap<String, String>,
}

fn label(target: &str) -> Result<&'static str, String> {
    match target { "ps4" => Ok("PS4"), "ps5" => Ok("PS5"), _ => Err("Choose PS4 or PS5.".into()) }
}
fn clean(text: &str) -> String {
    let text = redact_delivery_error(text, "ps4");
    let private = Regex::new(r#"(?i)(?:https?://|[a-z]:[\\/]|\\\\|/(?:user|data|system_data|mnt|home|tmp)/)[^\s\"']+|\b(?:\d{1,3}\.){3}\d{1,3}\b"#).unwrap();
    private.replace_all(&text, "[redacted]").chars().filter(|c| !c.is_control()).take(400).collect()
}
fn id_request(id: &str) -> Result<Vec<u8>, String> {
    let b=id.as_bytes();
    if b.len()!=9 || !b[..4].iter().all(u8::is_ascii_uppercase) || !b[4..].iter().all(u8::is_ascii_digit) {
        return Err("Use a title ID with four uppercase letters and five digits.".into());
    }
    let mut result=b.to_vec(); result.push(0); Ok(result)
}
async fn exchange(socket: &mut TcpStream, command: u8, body: &[u8], max: usize) -> Result<Vec<u8>, String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut header=[0u8;5]; header[0]=command; header[1..].copy_from_slice(&(body.len() as u32).to_le_bytes());
        socket.write_all(&header).await.map_err(|_| "The receiver connection closed. Reload the receiver and retry.".to_string())?;
        socket.write_all(body).await.map_err(|_| "The receiver connection closed. Reload the receiver and retry.".to_string())?;
        socket.read_exact(&mut header).await.map_err(|_| "The receiver connection closed. Reload the receiver and retry.".to_string())?;
        let size=u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
        if size>max.max(4096) { return Err("The receiver response exceeds its size limit.".into()); }
        let mut bytes=vec![0u8;size];
        socket.read_exact(&mut bytes).await.map_err(|_| "The receiver response was interrupted. Retry the operation.".to_string())?;
        if header[0]==2 {
            let text=String::from_utf8_lossy(&bytes);
            let value=serde_json::from_slice::<Value>(&bytes).ok();
            return Err(clean(value.as_ref().and_then(|v| v["error"].as_str()).unwrap_or(&text)));
        }
        let ok_reply=matches!(command,0x63|0x67|0x68);
        if (ok_reply && header[0]!=1) || (!ok_reply && header[0]!=3) { return Err("The receiver returned an unexpected response code.".into()); }
        if size>max { return Err("The receiver response exceeds its size limit.".into()); }
        Ok(bytes)
    }).await.map_err(|_| "The receiver did not respond in time. Retry the operation.".to_string())?
}
async fn checked(target: &str, host: &str, port: u16, capability: &str, action: &str) -> Result<(TcpStream, Value), String> {
    let console=label(target)?;
    validate_receiver_candidate(host.trim(),port)?;
    let mut socket=tokio::time::timeout(Duration::from_secs(5),TcpStream::connect((host.trim(),port))).await
        .map_err(|_| format!("The {console} receiver did not respond. Load the receiver and retry."))?
        .map_err(|_| format!("The {console} receiver is unavailable. Load the receiver and retry."))?;
    let bytes=exchange(&mut socket,0x53,&[],64*1024).await?;
    let config: Value=serde_json::from_slice(&bytes).map_err(|_| "The receiver configuration is invalid.".to_string())?;
    if config["platform"].as_str().is_some_and(|p| p!=target) { return Err(format!("This endpoint is not a {console} receiver.")); }
    let version=config["version"].as_str().filter(|s| !s.is_empty() && s.len()<=32).ok_or("The receiver configuration has no valid version.")?;
    let caps=config["capabilities"].as_array().ok_or("The receiver configuration has no capability list.")?;
    if caps.len()>64 || caps.iter().any(|v| !v.is_string()) { return Err("The receiver capability list is invalid.".into()); }
    if !caps.iter().any(|v| v==capability) {
        if capability=="shell-refresh-v1" && caps.iter().any(|v| v=="title-icons-v1") {
            return Err(format!("Restart your {console} to see the new icons."));
        }
        return Err(format!("Reload the {console} receiver to {action}. The loaded receiver is {}.",clean(version)));
    }
    Ok((socket,config))
}

fn validate_png(bytes: &[u8]) -> Result<(), String> {
    let valid = bytes.len()<=MAX_PNG && pkg_meta::is_valid_png(bytes) && bytes.len()>=33 && {
        let width=u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height=u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        width==height && (256..=1024).contains(&width)
    };
    if !valid { return Err("Use a square PNG from 256 to 1024 pixels, at most 2 MiB.".into()); }
    let mut reader=image::ImageReader::with_format(std::io::Cursor::new(bytes),image::ImageFormat::Png);
    let mut limits=image::Limits::default(); limits.max_image_width=Some(1024); limits.max_image_height=Some(1024); limits.max_alloc=Some(16*1024*1024); reader.limits(limits);
    reader.decode().map_err(|_| "The PNG image could not be decoded.".to_string())?;
    Ok(())
}
fn icon_result(bytes: &[u8], id: &str) -> Result<IconWriteResult,String> {
    let mut result: IconWriteResult=serde_json::from_slice(bytes).map_err(|_| "The receiver returned an invalid icon-write result.".to_string())?;
    if result.title_id!=id || result.written==0 || result.written>16 || !matches!(result.refresh.as_str(),"live"|"restart-required") || result.message.trim().is_empty() {
        return Err("The receiver returned an invalid icon-write result.".into());
    }
    result.message=clean(&result.message); Ok(result)
}
#[tauri::command]
pub(super) async fn get_title_icon(target: String, host: String, port: u16, title_id: String, original: bool) -> Result<String,String> {
    let mut body=id_request(&title_id)?; body.push(u8::from(original));
    let (mut socket,_)=checked(&target,&host,port,"title-icons-v1","read icons").await?;
    let png=exchange(&mut socket,0x60,&body,MAX_PNG).await?;
    tokio::task::spawn_blocking(move || { validate_png(&png)?; Ok(format!("data:image/png;base64,{}",BASE64.encode(&png))) }).await.map_err(|_| "Could not decode the title icon.".to_string())?
}
#[tauri::command]
pub(super) async fn set_title_icon(target: String, host: String, port: u16, title_id: String, png: String) -> Result<IconWriteResult,String> {
    let mut body=id_request(&title_id)?;
    if png.len()>MAX_PNG.div_ceil(3)*4 { return Err("The icon exceeds the 2 MiB limit.".into()); }
    let bytes=BASE64.decode(png).map_err(|_| "The icon must contain base64 PNG bytes without a data URL prefix.".to_string())?;
    let bytes=tokio::task::spawn_blocking(move || { validate_png(&bytes)?; Ok::<_,String>(bytes) }).await.map_err(|_| "Could not validate the icon.".to_string())??;
    body.extend_from_slice(&bytes);
    let (mut socket,_)=checked(&target,&host,port,"title-icons-v1","change icons").await?;
    icon_result(&exchange(&mut socket,0x61,&body,4096).await?,&title_id)
}
#[tauri::command]
pub(super) async fn restore_title_icon(target: String, host: String, port: u16, title_id: String) -> Result<IconWriteResult,String> {
    let body=id_request(&title_id)?;
    let (mut socket,_)=checked(&target,&host,port,"title-icons-v1","restore icons").await?;
    icon_result(&exchange(&mut socket,0x62,&body,4096).await?,&title_id)
}
fn theme_request(content_id: &str) -> Result<Vec<u8>,String> {
    let valid = content_id.len()==36 && content_id.is_char_boundary(36) && content_id.as_bytes()[6]==b'-' && content_id.as_bytes()[16]==b'_' && content_id.as_bytes()[19]==b'-'
        && content_id.bytes().enumerate().all(|(i,b)| matches!(i,6|16|19) || b.is_ascii_uppercase() || b.is_ascii_digit() || b==b'_');
    if !valid { return Err("That is not a PS4 theme content ID.".into()); }
    let mut body=content_id.as_bytes().to_vec(); body.push(0); Ok(body)
}
#[tauri::command]
pub(super) async fn list_console_themes(host: String, port: u16) -> Result<ConsoleThemes,String> {
    let (mut socket,_)=checked("ps4",&host,port,"themes-v1","list installed themes").await?;
    let value: Value=serde_json::from_slice(&exchange(&mut socket,0x66,&[],256*1024).await?).map_err(|_| "The receiver returned an invalid theme list.".to_string())?;
    let items=value["themes"].as_array().ok_or("The receiver returned an invalid theme list.")?;
    let mut themes=Vec::new();
    for item in items.iter().take(128) {
        let Some(id)=item["contentId"].as_str().filter(|id| theme_request(id).is_ok()) else { continue };
        let title: String=item["title"].as_str().unwrap_or("").chars().filter(|c| !c.is_control()).take(127).collect();
        themes.push(InstalledTheme { content_id: id.into(), title: if title.trim().is_empty() { id[20..].into() } else { title } });
    }
    // The console stores the selected theme as its 16-character label.
    let active=value["active"].as_str().unwrap_or("");
    let active_content_id=(!active.is_empty()).then(|| themes.iter().find(|t| t.content_id[20..]==*active).map(|t| t.content_id.clone())).flatten();
    Ok(ConsoleThemes { themes, active_content_id, truncated: value["truncated"].as_bool().unwrap_or(false) })
}
#[tauri::command]
pub(super) async fn apply_console_theme(host: String, port: u16, content_id: String) -> Result<String,String> {
    let body=theme_request(&content_id)?;
    let (mut socket,_)=checked("ps4",&host,port,"themes-v1","apply themes").await?;
    exchange(&mut socket,0x67,&body,4096).await?;
    Ok("Theme selected on the PS4.".into())
}
#[tauri::command]
pub(super) async fn remove_console_theme(host: String, port: u16, content_id: String) -> Result<String,String> {
    let body=theme_request(&content_id)?;
    let (mut socket,_)=checked("ps4",&host,port,"themes-v1","remove themes").await?;
    exchange(&mut socket,0x68,&body,4096).await?;
    Ok("Theme removed from the PS4.".into())
}
#[tauri::command]
pub(super) async fn refresh_console_shell(target: String, host: String, port: u16) -> Result<String,String> {
    let (mut socket,_)=checked(&target,&host,port,"shell-refresh-v1","refresh the home screen").await?;
    let bytes=exchange(&mut socket,0x63,&[],4096).await?;
    let text=std::str::from_utf8(&bytes).map_err(|_| "The receiver returned an invalid refresh response.".to_string())?;
    if text.trim().is_empty() { return Err("The receiver returned an empty refresh response.".into()); }
    Ok(clean(text))
}
#[tauri::command]
pub(super) async fn console_system_info(target: String, host: String, port: u16) -> Result<ConsoleSystemInfo,String> {
    let (mut socket,config)=checked(&target,&host,port,"system-info-v1","read system information").await?;
    let mut info: ConsoleSystemInfo=serde_json::from_slice(&exchange(&mut socket,0x64,&[],64*1024).await?).map_err(|_| "The receiver returned invalid system information.".to_string())?;
    if info.target!=target || config["version"]!=info.receiver_version || info.storage.len()>16 || info.capabilities.len()>64 || info.extras.len()>64 ||
        info.storage.iter().any(|s| s.free_bytes>s.total_bytes || !matches!(s.path.as_str(),"/user"|"/data"|"/mnt/ext0"|"/mnt/ext1")) ||
        info.memory.as_ref().is_some_and(|m| m.free_bytes>m.total_bytes) ||
        info.running_title_id.as_ref().is_some_and(|id| id_request(id).is_err()) ||
        [info.cpu_temp_c,info.soc_temp_c].into_iter().flatten().any(|t| !t.is_finite()) {
        return Err("The receiver returned inconsistent system information.".into());
    }
    for s in &mut info.storage { s.label=clean(&s.label); }
    for value in [&mut info.firmware,&mut info.sdk_version,&mut info.model,&mut info.console_name].into_iter().flatten() { *value=clean(value); }
    info.extras=info.extras.into_iter().map(|(k,v)| (clean(&k),clean(&v))).collect();
    Ok(info)
}

fn metadata_field<'a>(bytes: &'a [u8], at: &mut usize, cap: usize) -> Result<&'a [u8],String> {
    let header=bytes.get(*at..at.saturating_add(4)).ok_or("Installed metadata is truncated.")?;
    let size=u32::from_le_bytes(header.try_into().unwrap()) as usize; *at+=4;
    if size>cap { return Err("Installed metadata exceeds its size limit.".into()); }
    let value=bytes.get(*at..at.saturating_add(size)).ok_or("Installed metadata is truncated.")?; *at+=size; Ok(value)
}
#[derive(Default)]
struct Metadata { name: Option<String>, version: Option<String>, firmware: Option<String>, content: Option<String> }
fn firmware_version(value: u64) -> Option<String> {
    let value=if value>u32::MAX as u64 { value>>32 } else { value };
    if value==0 { None } else { Some(format!("{:x}.{:02x}",(value>>24)&255,(value>>16)&255)) }
}
fn sfo_value<'a>(data: &'a [u8], wanted: &str) -> Option<(u16,&'a [u8])> {
    if pkg_meta::parse_sfo(data).is_none() { return None; }
    let u32at=|at:usize| u32::from_le_bytes(data[at..at+4].try_into().unwrap()) as usize;
    let keys=u32at(8); let values=u32at(12);
    for e in data[20..20+u32at(16)*16].chunks_exact(16) {
        let start=keys+u16::from_le_bytes(e[..2].try_into().unwrap()) as usize;
        let key=&data[start..values]; let end=key.iter().position(|b| *b==0)?;
        if &key[..end]==wanted.as_bytes() {
            let at=values+u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
            let n=u32::from_le_bytes(e[4..8].try_into().unwrap()) as usize;
            return Some((u16::from_le_bytes(e[2..4].try_into().unwrap()),&data[at..at+n]));
        }
    }
    None
}
fn decode_metadata(id: &str, bytes: &[u8]) -> Result<Metadata,String> {
    if bytes.is_empty() { return Ok(Metadata::default()); }
    if id.starts_with("CUSA") {
        let (_,name,version,actual)=pkg_meta::parse_sfo(bytes).ok_or("param.sfo is malformed.")?;
        if actual.as_deref().is_some_and(|a| a!=id) { return Err("param.sfo belongs to another title.".into()); }
        let firmware=sfo_value(bytes,"SYSTEM_VER").and_then(|(kind,raw)| if kind==0x0404 && raw.len()==4 { firmware_version(u32::from_le_bytes(raw.try_into().unwrap()) as u64) } else { None });
        let content=sfo_value(bytes,"CONTENT_ID").and_then(|(kind,raw)| if kind==0x0204 { std::str::from_utf8(raw).ok().map(|v| v.trim_end_matches('\0').to_string()).filter(|s| !s.is_empty()) } else { None });
        return Ok(Metadata { name,version,firmware,content });
    }
    let value: Value=serde_json::from_slice(bytes).map_err(|_| "param.json is malformed.")?;
    if !value.is_object() || value["titleId"].as_str()!=Some(id) { return Err("param.json has no matching title ID.".into()); }
    let string=|key: &str| value[key].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let locale=&value["localizedParameters"];
    let language=locale["defaultLanguage"].as_str().unwrap_or("en-US");
    let name=locale[language]["titleName"].as_str().or_else(|| locale["en-US"]["titleName"].as_str()).or_else(|| value["titleName"].as_str()).map(str::to_string);
    let required=&value["requiredSystemSoftwareVersion"];
    let firmware=if let Some(n)=required.as_u64() { firmware_version(n) } else { required.as_str().and_then(|s| {
        if let Some(hex)=s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) { u64::from_str_radix(hex,16).ok().and_then(firmware_version) }
        else if s.contains('.') && s.bytes().all(|b| b.is_ascii_digit() || b==b'.') { Some(s.into()) }
        else { s.parse::<u64>().ok().and_then(firmware_version) }
    }) };
    Ok(Metadata { name,version:string("contentVersion").or_else(|| string("masterVersion")),firmware,content:string("contentId") })
}
#[tauri::command]
pub(super) async fn list_console_library(target: String, host: String, port: u16) -> Result<ConsoleLibrarySnapshot,String> {
    let (mut socket,_)=checked(&target,&host,port,"installed-library-v1","read the library").await?;
    let response: Value=serde_json::from_slice(&exchange(&mut socket,0x5e,&[],128*1024).await?).map_err(|_| "The receiver returned an invalid library snapshot.".to_string())?;
    let ids=response["titles"].as_array().ok_or("The library snapshot has no title list.")?;
    let truncated=response["truncated"].as_bool().ok_or("The library snapshot has no truncation status.")?;
    let mut complete=response["complete"].as_bool().ok_or("The library snapshot has no completion status.")? && !truncated;
    let mut errors: Vec<String>=response["errors"].as_array().ok_or("The library snapshot has no error list.")?.iter()
        .map(|v| v.as_str().map(clean).ok_or("The library error list is malformed.")).collect::<Result<_,_>>()?;
    if response["errorsTruncated"].as_bool().unwrap_or(false) { errors.push("Some inventory errors were omitted.".into()); }
    if !errors.is_empty() { complete=false; }
    let mut custom=HashSet::new();
    if let Some(values)=response.get("customIcons") {
        for value in values.as_array().ok_or("The custom-icon list is malformed.")? {
            let id=value.as_str().filter(|id| id_request(id).is_ok()).ok_or("The custom-icon list contains an invalid title ID.")?;
            custom.insert(id);
        }
    }
    if ids.len()>2048 { complete=false; errors.push("The library exceeded the 2,048-title limit.".into()); }
    let mut snapshot=ConsoleLibrarySnapshot { target:target.clone(),entries:Vec::new(),complete,truncated:truncated||ids.len()>2048,errors,metadata_warnings:Vec::new() };
    let mut seen=HashSet::new(); let mut connection_ok=true;
    for value in ids.iter().take(2048) {
        let Some(id)=value.as_str().filter(|id| title_id(id) && (target=="ps5" || id.starts_with("CUSA"))) else {
            snapshot.complete=false; snapshot.errors.push("The library contained an invalid title ID.".into()); continue;
        };
        if !seen.insert(id) { snapshot.complete=false; snapshot.errors.push("The library contained a duplicate title ID.".into()); continue; }
        let mut entry=LibraryTitle { title_id:id.into(),name:id.into(),version:None,base_version:None,update_version:None,icon:None,custom_icon:custom.contains(id),required_firmware:None,content_id:None,platform:if id.starts_with("CUSA") {"ps4"} else {"ps5"}.into() };
        if connection_ok {
            let bytes=exchange(&mut socket,0x5f,&id_request(id)?,12+2*MAX_META+MAX_PNG).await;
            match bytes {
                Ok(bytes) => {
                    let parts=(|| { let mut at=0; let base=metadata_field(&bytes,&mut at,MAX_META)?; let patch=metadata_field(&bytes,&mut at,MAX_META)?; let icon=metadata_field(&bytes,&mut at,MAX_PNG)?;
                        if at!=bytes.len() { return Err("Installed metadata contains trailing bytes.".to_string()); } Ok((base,patch,icon)) })();
                    match parts {
                        Ok((base,patch,icon)) => {
                            let mut parsed=Vec::new();
                            for data in [base,patch] { match decode_metadata(id,data) { Ok(meta)=>parsed.push(meta), Err(error)=> { snapshot.metadata_warnings.push(format!("{id}: {error}")); parsed.push(Metadata::default()); } } }
                            let update=parsed.pop().unwrap(); let base=parsed.pop().unwrap();
                            if let Some(name)=update.name.or(base.name) { entry.name=name.chars().filter(|c| !c.is_control()).take(256).collect(); }
                            if entry.name==id { snapshot.metadata_warnings.push(format!("{id}: Title metadata is unavailable; showing the title ID.")); }
                            entry.base_version=base.version; entry.update_version=update.version; entry.version=entry.update_version.clone().or_else(|| entry.base_version.clone());
                            entry.required_firmware=update.firmware.or(base.firmware); entry.content_id=update.content.or(base.content);
                            if !icon.is_empty() {
                                let icon=icon.to_vec(); let thumb=tokio::task::spawn_blocking(move || ps4_receiver::compact_installed_icon(&icon)).await.ok().flatten();
                                if let Some(bytes)=thumb { entry.icon=Some(format!("data:image/jpeg;base64,{}",BASE64.encode(bytes))); }
                                else { snapshot.metadata_warnings.push(format!("{id}: The cover artwork could not be decoded.")); }
                            }
                        }
                        Err(error)=>snapshot.metadata_warnings.push(format!("{id}: {error}")),
                    }
                }
                Err(error)=> { snapshot.metadata_warnings.push(format!("{id}: {}",clean(&error))); connection_ok=false; snapshot.complete=false; }
            }
        }
        snapshot.entries.push(entry);
    }
    snapshot.errors.truncate(64); snapshot.metadata_warnings.truncate(256); Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_requests_need_a_ps4_content_id() {
        assert_eq!(theme_request("UP9000-CUSA00000_00-SSPIBUBBLES00002").unwrap().last(), Some(&0));
        for bad in ["", "UP9000-CUSA00000_00-SSPIBUBBLES0000", "UP9000-CUSA00000_00-sspibubbles00002", "UP9000/CUSA00000_00-SSPIBUBBLES00002", "UP9000-CUSA00000_00-SSPIBUBBLES0000\u{e9}"] {
            assert!(theme_request(bad).is_err(), "{bad}");
        }
    }
    use tokio::net::TcpListener;
    const ID: &str = "CUSA12345";
    const HOST: &str = "127.0.0.1";
    const CAPS: &[&str] = &["installed-library-v1", "title-icons-v1", "system-info-v1", "shell-refresh-v1"];
    type Step = (u8, Vec<u8>, u8, Vec<u8>);
    struct Fake { port: u16, task: tokio::task::JoinHandle<Vec<u8>> }
    impl Fake {
        async fn start(target: &str, caps: &[&str], steps: Vec<Step>) -> Self {
            Self::config(json!({"platform":target,"version":"1.0.5","capabilities":caps}),steps).await
        }
        async fn config(config: Value, steps: Vec<Step>) -> Self {
            let listener=TcpListener::bind((HOST,0)).await.unwrap(); let port=listener.local_addr().unwrap().port();
            let task=tokio::spawn(async move {
                let (mut socket,_)=listener.accept().await.unwrap();
                let mut commands=Vec::new();
                for (cmd,body,code,reply) in std::iter::once((0x53,vec![],3,serde_json::to_vec(&config).unwrap())).chain(steps) {
                    let mut h=[0;5]; socket.read_exact(&mut h).await.unwrap();
                    let n=u32::from_le_bytes(h[1..].try_into().unwrap()) as usize; assert!(n<=MAX_PNG+10);
                    let mut request=vec![0;n]; socket.read_exact(&mut request).await.unwrap();
                    assert_eq!(h[0],cmd); assert_eq!(request,body); commands.push(cmd);
                    h[0]=code; h[1..].copy_from_slice(&(reply.len() as u32).to_le_bytes());
                    socket.write_all(&h).await.unwrap(); socket.write_all(&reply).await.unwrap();
                }
                let mut extra=[0;5]; let n=socket.read(&mut extra).await.unwrap_or(0); assert_eq!(n,0,"unexpected command after capability refusal or final result");
                commands
            }); Self {port,task}
        }
        async fn done(self) -> Vec<u8> { tokio::time::timeout(Duration::from_secs(3),self.task).await.unwrap().unwrap() }
    }
    fn png() -> Vec<u8> {
        let image=image::RgbaImage::from_pixel(256,256,image::Rgba([40,80,120,200]));
        let mut output=std::io::Cursor::new(Vec::new()); image.write_to(&mut output,image::ImageFormat::Png).unwrap(); output.into_inner()
    }
    fn write_result(id: &str) -> Vec<u8> { serde_json::to_vec(&json!({"titleId":id,"written":2,"backedUp":true,"refresh":"restart-required","message":"Icon PNG copies saved. Restart your console."})).unwrap() }
    fn metadata(fields: &[&[u8]]) -> Vec<u8> {
        let mut bytes=Vec::new(); for field in fields { bytes.extend_from_slice(&(field.len() as u32).to_le_bytes()); bytes.extend_from_slice(field); } bytes
    }
    fn sfo() -> Vec<u8> {
        let values: Vec<(&str,u16,Vec<u8>)>=vec![
            ("TITLE",0x0204,b"Host title\0".to_vec()),("TITLE_ID",0x0204,b"CUSA12345\0".to_vec()),
            ("APP_VER",0x0204,b"01.20\0".to_vec()),("CONTENT_ID",0x0204,b"UP0000-CUSA12345_00-ABCDEFGHIJKLMNOP\0".to_vec()),
            ("SYSTEM_VER",0x0404,0x09000000u32.to_le_bytes().to_vec())];
        let keys: Vec<u8>=values.iter().flat_map(|(name,_,_)| name.bytes().chain(std::iter::once(0))).collect();
        let key_offset=20+16*values.len(); let value_offset=key_offset+keys.len();
        let mut out=vec![0;value_offset]; out[..4].copy_from_slice(b"\0PSF"); out[4..8].copy_from_slice(&0x101u32.to_le_bytes());
        for (at,n) in [(8,key_offset),(12,value_offset),(16,values.len())] { out[at..at+4].copy_from_slice(&(n as u32).to_le_bytes()); }
        out[key_offset..].copy_from_slice(&keys); let mut key=0; let mut offset=0;
        for (i,(name,kind,bytes)) in values.iter().enumerate() {
            let at=20+i*16; out[at..at+2].copy_from_slice(&(key as u16).to_le_bytes()); out[at+2..at+4].copy_from_slice(&kind.to_le_bytes());
            for (field,n) in [(4,bytes.len()),(8,bytes.len()),(12,offset)] { out[at+field..at+field+4].copy_from_slice(&(n as u32).to_le_bytes()); }
            out.extend_from_slice(bytes); key+=name.len()+1; offset+=bytes.len();
        }
        out
    }
    fn system(target: &str) -> Value { json!({"target":target,"receiverVersion":"1.0.5","firmware":null,"sdkVersion":null,
        "model":null,"consoleName":null,"uptimeSeconds":null,"cpuTempC":null,"socTempC":null,"storage":[{"label":"Internal","path":"/user","totalBytes":1000,"freeBytes":500}],
        "memory":null,"network":null,"runningTitleId":null,"capabilities":CAPS,"extras":{}}) }

    #[tokio::test]
    async fn get_icons_preserves_full_png_and_original_flag_on_both_consoles() {
        for target in ["ps4","ps5"] { for original in [false,true] {
            let mut request=id_request(ID).unwrap(); request.push(original as u8); let bytes=png();
            let fake=Fake::start(target,CAPS,vec![(0x60,request,3,bytes.clone())]).await;
            let result=get_title_icon(target.into(),HOST.into(),fake.port,ID.into(),original).await.unwrap();
            assert_eq!(result,format!("data:image/png;base64,{}",BASE64.encode(bytes))); assert_eq!(fake.done().await,vec![0x53,0x60]);
        } }
    }
    #[tokio::test]
    async fn set_and_restore_use_identical_protocol_on_both_consoles() {
        for target in ["ps4","ps5"] {
            let bytes=png(); let mut request=id_request(ID).unwrap(); request.extend_from_slice(&bytes);
            let fake=Fake::start(target,CAPS,vec![(0x61,request,3,write_result(ID))]).await;
            let result=set_title_icon(target.into(),HOST.into(),fake.port,ID.into(),BASE64.encode(bytes)).await.unwrap();
            assert_eq!(result.written,2); assert!(result.backed_up); assert_eq!(result.refresh,"restart-required"); fake.done().await;
            let fake=Fake::start(target,CAPS,vec![(0x62,id_request(ID).unwrap(),3,write_result(ID))]).await;
            assert_eq!(restore_title_icon(target.into(),HOST.into(),fake.port,ID.into()).await.unwrap().title_id,ID); fake.done().await;
        }
    }
    #[tokio::test]
    async fn shell_only_sends_explicit_capability_supported_request() {
        let fake=Fake::start("ps5",CAPS,vec![(0x63,vec![],1,b"Home screen refreshed.".to_vec())]).await;
        assert_eq!(refresh_console_shell("ps5".into(),HOST.into(),fake.port).await.unwrap(),"Home screen refreshed."); fake.done().await;
        for target in ["ps4","ps5"] {
            let fake=Fake::start(target,&["title-icons-v1"],vec![]).await;
            assert_eq!(refresh_console_shell(target.into(),HOST.into(),fake.port).await.unwrap_err(),format!("Restart your {} to see the new icons.",target.to_uppercase()));
            assert_eq!(fake.done().await,vec![0x53]);
        }
    }
    #[tokio::test]
    async fn system_information_keeps_unknown_fields_null() {
        for target in ["ps4","ps5"] {
            let fake=Fake::start(target,CAPS,vec![(0x64,vec![],3,serde_json::to_vec(&system(target)).unwrap())]).await;
            let info=console_system_info(target.into(),HOST.into(),fake.port).await.unwrap();
            let json=serde_json::to_value(info).unwrap(); assert!(json["cpuTempC"].is_null()); assert!(json["firmware"].is_null()); assert_eq!(json["storage"][0]["freeBytes"],500); fake.done().await;
        }
    }
    #[tokio::test]
    async fn library_reads_ps4_sfo_and_ps5_param_json_without_changing_legacy_framing() {
        for target in ["ps4","ps5"] {
            let mut titles=vec![ID]; if target=="ps5" { titles.push("PPSA12345"); }
            let listing=json!({"titles":titles,"complete":true,"truncated":false,"errorsTruncated":false,"errors":[],"customIcons":[ID]});
            let mut steps=vec![(0x5e,vec![],3,serde_json::to_vec(&listing).unwrap()),(0x5f,id_request(ID).unwrap(),3,metadata(&[&sfo(),&[],&png()]))];
            if target=="ps5" {
                let json=json!({"titleId":"PPSA12345","contentId":"EP0000-PPSA12345_00-ABCDEFGHIJKLMNOP","contentVersion":"01.002.000","requiredSystemSoftwareVersion":"0x0520000000000000","localizedParameters":{"defaultLanguage":"en-US","en-US":{"titleName":"PS5 host title"}}});
                steps.push((0x5f,id_request("PPSA12345").unwrap(),3,metadata(&[&serde_json::to_vec(&json).unwrap(),&[],&[]])));
            }
            let fake=Fake::start(target,CAPS,steps).await;
            let result=list_console_library(target.into(),HOST.into(),fake.port).await.unwrap();
            assert!(result.complete); assert!(result.metadata_warnings.is_empty()); assert_eq!(result.entries[0].name,"Host title");
            assert_eq!(result.entries[0].required_firmware.as_deref(),Some("9.00")); assert!(result.entries[0].content_id.is_some()); assert!(result.entries[0].custom_icon);
            assert_eq!(result.entries[0].platform,"ps4"); assert!(result.entries[0].icon.as_ref().unwrap().starts_with("data:image/jpeg;base64,"));
            if target=="ps5" { assert_eq!(result.entries[1].platform,"ps5"); assert_eq!(result.entries[1].name,"PS5 host title"); assert_eq!(result.entries[1].required_firmware.as_deref(),Some("5.20")); }
            fake.done().await;
        }
    }
    #[tokio::test]
    async fn every_command_refuses_missing_capabilities_before_sending_operation() {
        for target in ["ps4","ps5"] { for command in 0..6 {
            let fake=Fake::start(target,&[],vec![]).await;
            let result=match command {
                0=>list_console_library(target.into(),HOST.into(),fake.port).await.map(|_| ()),
                1=>get_title_icon(target.into(),HOST.into(),fake.port,ID.into(),true).await.map(|_| ()),
                2=>set_title_icon(target.into(),HOST.into(),fake.port,ID.into(),BASE64.encode(png())).await.map(|_| ()),
                3=>restore_title_icon(target.into(),HOST.into(),fake.port,ID.into()).await.map(|_| ()),
                4=>refresh_console_shell(target.into(),HOST.into(),fake.port).await.map(|_| ()),
                _=>console_system_info(target.into(),HOST.into(),fake.port).await.map(|_| ()),
            };
            let error=result.unwrap_err(); assert!(error.starts_with(&format!("Reload the {} receiver",target.to_uppercase())),"{error}"); assert!(error.contains("1.0.5")); assert_eq!(fake.done().await,vec![0x53]);
        } }
    }
    #[tokio::test]
    async fn malformed_responses_are_rejected_for_every_command() {
        for command in [0x5e,0x60,0x61,0x62,0x63,0x64] {
            let mut body=if matches!(command,0x60..=0x62) {id_request(ID).unwrap()} else {vec![]};
            if command==0x60 { body.push(0); }
            else if command==0x61 {body.extend_from_slice(&png());}
            let fake=Fake::start("ps5",CAPS,vec![(command,body,if command==0x63 {1} else {3},vec![0xff])]).await;
            let result=match command {
                0x5e=>list_console_library("ps5".into(),HOST.into(),fake.port).await.map(|_| ()),
                0x60=>get_title_icon("ps5".into(),HOST.into(),fake.port,ID.into(),false).await.map(|_| ()),
                0x61=>set_title_icon("ps5".into(),HOST.into(),fake.port,ID.into(),BASE64.encode(png())).await.map(|_| ()),
                0x62=>restore_title_icon("ps5".into(),HOST.into(),fake.port,ID.into()).await.map(|_| ()),
                0x63=>refresh_console_shell("ps5".into(),HOST.into(),fake.port).await.map(|_| ()),
                _=>console_system_info("ps5".into(),HOST.into(),fake.port).await.map(|_| ()),
            }; assert!(result.is_err()); fake.done().await;
        }
    }
    #[tokio::test]
    async fn invalid_inputs_fail_before_connecting() {
        for id in ["../a12345","CUSA1234","cusa12345","CUSA12/45","ééAA12345"] {
            assert!(get_title_icon("ps4".into(),HOST.into(),0,id.into(),true).await.unwrap_err().contains("title ID"));
        }
        assert!(set_title_icon("ps5".into(),HOST.into(),0,ID.into(),"data:image/png;base64,xx".into()).await.unwrap_err().contains("base64"));
        assert!(set_title_icon("ps5".into(),HOST.into(),0,ID.into(),BASE64.encode(b"not png")).await.unwrap_err().contains("PNG"));
        assert!(set_title_icon("ps5".into(),HOST.into(),0,ID.into(),"A".repeat(MAX_PNG*2)).await.unwrap_err().contains("2 MiB"));
    }
    #[tokio::test]
    async fn mismatched_identity_and_private_errors_are_not_exposed() {
        let fake=Fake::start("ps4",CAPS,vec![]).await;
        assert!(console_system_info("ps5".into(),HOST.into(),fake.port).await.unwrap_err().contains("not a PS5")); fake.done().await;
        let fake=Fake::start("ps5",CAPS,vec![(0x62,id_request(ID).unwrap(),3,write_result("CUSA99999"))]).await;
        assert!(restore_title_icon("ps5".into(),HOST.into(),fake.port,ID.into()).await.is_err()); fake.done().await;
        let fake=Fake::start("ps5",CAPS,vec![(0x62,id_request(ID).unwrap(),2,b"Cannot read /user/appmeta/CUSA12345/icon0.png from 192.168.9.8".to_vec())]).await;
        let error=restore_title_icon("ps5".into(),HOST.into(),fake.port,ID.into()).await.unwrap_err(); assert!(!error.contains("/user/")); assert!(!error.contains("192.168")); fake.done().await;
    }
    #[tokio::test]
    async fn partial_library_and_bad_metadata_preserve_snapshot_honesty() {
        let listing=json!({"titles":[ID,ID,"../a12345"],"complete":true,"truncated":false,"errors":[],"customIcons":[]});
        let fake=Fake::start("ps4",CAPS,vec![(0x5e,vec![],3,serde_json::to_vec(&listing).unwrap()),(0x5f,id_request(ID).unwrap(),3,vec![0xff])]).await;
        let result=list_console_library("ps4".into(),HOST.into(),fake.port).await.unwrap(); assert!(!result.complete); assert_eq!(result.entries.len(),1); assert_eq!(result.metadata_warnings.len(),1); fake.done().await;
    }
    #[test]
    fn metadata_lengths_firmware_and_settings_defaults_are_checked() {
        assert!(metadata_field(&[255;4],&mut 0,MAX_META).is_err());
        assert!(decode_metadata("PPSA12345",br#"{"titleId":"PPSA99999"}"#).is_err());
        assert_eq!(firmware_version(0x11000000),Some("11.00".into()));
        let mut settings=serde_json::to_value(Settings::default()).unwrap(); settings.as_object_mut().unwrap().remove("ps5LoaderPort");
        let restored: Settings=serde_json::from_value(settings).unwrap(); assert_eq!(restored.ps5_loader_port,9021);
    }
}
