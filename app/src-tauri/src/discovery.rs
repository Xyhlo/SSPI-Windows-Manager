//! Receiver detection and bounded private-LAN discovery.
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, UdpSocket},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tauri::State;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Semaphore,
    time::{timeout, Duration},
};

use crate::{AppState, PS4_RECEIVER_VERSION, RECEIVER_VERSION};

const MAX_RESPONSE: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const PROBE_STEP_TIMEOUT: Duration = Duration::from_millis(650);
const SCAN_CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
const FTP_BANNER_TIMEOUT: Duration = Duration::from_millis(800);
const MAX_FTP_BANNER: usize = 200;
const RECEIVER_PORT: u16 = 9114;
const FTP_PORT: u16 = 2121;
const MAX_CONCURRENT_CONNECTS: usize = 128;
const WHOLE_SCAN_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConsoleProbe {
    target: String,
    host: String,
    receiver: ReceiverStatus,
    checked_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiverStatus {
    state: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    expected_version: String,
    capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DiscoveredConsole {
    host: String,
    platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    receiver: Option<DiscoveredReceiver>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ftp: Option<DiscoveredFtp>,
    label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveredReceiver {
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveredFtp {
    port: u16,
    banner: String,
}

#[derive(Debug)]
struct ReceiverConfig {
    version: String,
    capabilities: Vec<String>,
    platform: Option<String>,
}

#[tauri::command]
pub(super) async fn probe_consoles(
    state: State<'_, AppState>,
) -> Result<Vec<ConsoleProbe>, String> {
    let settings = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let ps5 = probe_one(
        "ps5",
        settings.ps5_host,
        settings.ps5_port,
        RECEIVER_VERSION,
    );
    let ps4 = probe_one(
        "ps4",
        settings.ps4_host,
        settings.ps4_receiver_port,
        PS4_RECEIVER_VERSION,
    );
    let (ps5, ps4) = tokio::join!(ps5, ps4);
    Ok(vec![ps5, ps4])
}

#[tauri::command]
pub(super) async fn discover_consoles(
    state: State<'_, AppState>,
) -> Result<Vec<DiscoveredConsole>, String> {
    let settings = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let local_ip = primary_private_ipv4();
    let mut network_ips = Vec::new();
    if let Some(ip) = local_ip {
        network_ips.push(ip);
    }
    for configured in [&settings.ps5_host, &settings.ps4_host] {
        if let Ok(ip) = configured.parse::<Ipv4Addr>() {
            if is_private_ipv4(ip) {
                network_ips.push(ip);
            }
        }
    }
    let hosts = hosts_for_networks(&network_ips, local_ip);
    let scan = scan_hosts(hosts);
    Ok(timeout(WHOLE_SCAN_TIMEOUT, scan).await.unwrap_or_default())
}

async fn probe_one(
    target: &'static str,
    host: String,
    port: u16,
    expected_version: &'static str,
) -> ConsoleProbe {
    if host.trim().is_empty() {
        return probe_result(
            target,
            host,
            port,
            expected_version,
            "unconfigured",
            None,
            vec![],
            None,
            Some(format!(
                "Configure a {target_upper} address to detect its receiver.",
                target_upper = target.to_ascii_uppercase()
            )),
        );
    }
    let connected = timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await;
    let mut stream = match connected {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) | Err(_) => {
            return probe_result(
                target,
                host.clone(),
                port,
                expected_version,
                "offline",
                None,
                vec![],
                None,
                Some(format!("Nothing answered at {host}:{port}.")),
            );
        }
    };

    let ping_started = Instant::now();
    match exchange(&mut stream, 0x01, &[]).await {
        Ok((1, body)) if body == b"SSPI" => {}
        _ => {
            return probe_error(
                target,
                host,
                port,
                expected_version,
                "The service did not answer the receiver ping.",
            )
        }
    }
    let latency_ms = ping_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let (code, body) = match exchange(&mut stream, 0x53, &[]).await {
        Ok(reply) => reply,
        Err(_) => {
            return probe_error(
                target,
                host,
                port,
                expected_version,
                "The service did not return receiver configuration.",
            )
        }
    };
    if code != 3 {
        return probe_error(
            target,
            host,
            port,
            expected_version,
            "The service did not return receiver configuration.",
        );
    }
    let config = match parse_receiver_config(&body) {
        Some(config) => config,
        None => {
            return probe_error(
                target,
                host,
                port,
                expected_version,
                "The service returned invalid receiver configuration.",
            )
        }
    };
    if config.platform.as_deref() != Some(target) {
        return probe_error(
            target,
            host,
            port,
            expected_version,
            "A different or unknown receiver answered at this address.",
        );
    }

    let state = if config.version == expected_version {
        "online"
    } else {
        "outdated"
    };
    let message = (state == "outdated").then(|| {
        format!(
            "The {} receiver {} is loaded; this app needs {}.",
            target.to_ascii_uppercase(),
            config.version,
            expected_version
        )
    });
    probe_result(
        target,
        host,
        port,
        expected_version,
        state,
        Some(config.version),
        config.capabilities,
        Some(latency_ms),
        message,
    )
}

fn probe_error(
    target: &str,
    host: String,
    port: u16,
    expected_version: &str,
    detail: &str,
) -> ConsoleProbe {
    probe_result(
        target,
        host.clone(),
        port,
        expected_version,
        "error",
        None,
        vec![],
        None,
        Some(format!("Something answered at {host}:{port}, but {detail}")),
    )
}

fn probe_result(
    target: &str,
    host: String,
    port: u16,
    expected_version: &str,
    state: &str,
    version: Option<String>,
    capabilities: Vec<String>,
    latency_ms: Option<u64>,
    message: Option<String>,
) -> ConsoleProbe {
    ConsoleProbe {
        target: target.into(),
        host,
        receiver: ReceiverStatus {
            state: state.into(),
            port,
            version,
            expected_version: expected_version.into(),
            capabilities,
            latency_ms,
            message,
        },
        checked_at: unix_ms(),
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

async fn exchange(
    stream: &mut TcpStream,
    command: u8,
    body: &[u8],
) -> Result<(u8, Vec<u8>), String> {
    let body_len =
        u32::try_from(body.len()).map_err(|_| "The receiver request is too large".to_string())?;
    let mut request = Vec::with_capacity(5 + body.len());
    request.push(command);
    request.extend_from_slice(&body_len.to_le_bytes());
    request.extend_from_slice(body);
    timeout(PROBE_STEP_TIMEOUT, async {
        stream
            .write_all(&request)
            .await
            .map_err(|_| "Could not send the receiver request".to_string())?;
        let mut header = [0_u8; 5];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|_| "The receiver response was incomplete".to_string())?;
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if length > MAX_RESPONSE {
            return Err("The receiver response was too large".into());
        }
        let mut response = vec![0; length];
        stream
            .read_exact(&mut response)
            .await
            .map_err(|_| "The receiver response was incomplete".to_string())?;
        Ok((header[0], response))
    })
    .await
    .map_err(|_| "The receiver request timed out".to_string())?
}

fn parse_receiver_config(body: &[u8]) -> Option<ReceiverConfig> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let version = value.get("version")?.as_str()?.to_string();
    let capabilities = value
        .get("capabilities")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let platform = platform_from_config_value(&value, &capabilities);
    Some(ReceiverConfig {
        version,
        capabilities,
        platform,
    })
}

fn platform_from_config_value(value: &Value, capabilities: &[String]) -> Option<String> {
    if let Some(platform) = value.get("platform").and_then(Value::as_str) {
        let platform = platform.to_ascii_lowercase();
        if platform == "ps4" || platform == "ps5" {
            return Some(platform);
        }
    }
    let has = |marker: &str| {
        capabilities
            .iter()
            .any(|capability| capability.eq_ignore_ascii_case(marker))
    };
    if has("ps4") {
        Some("ps4".into())
    } else if has("dump-mount") || has("fih-install") {
        Some("ps5".into())
    } else {
        None
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
}

fn hosts_for_networks(network_ips: &[Ipv4Addr], excluded: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let mut hosts = std::collections::BTreeSet::new();
    for ip in network_ips
        .iter()
        .copied()
        .filter(|ip| is_private_ipv4(*ip))
    {
        let octets = ip.octets();
        for last in 1..=254 {
            let host = Ipv4Addr::new(octets[0], octets[1], octets[2], last);
            if Some(host) != excluded {
                hosts.insert(host);
            }
        }
    }
    hosts.into_iter().collect()
}

fn primary_private_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 168, 1, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if is_private_ipv4(ip) => Some(ip),
        _ => None,
    }
}

async fn scan_hosts(hosts: Vec<Ipv4Addr>) -> Vec<DiscoveredConsole> {
    let semaphore = std::sync::Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTS));
    let mut joins = tokio::task::JoinSet::new();
    for host in hosts {
        for port in [RECEIVER_PORT, FTP_PORT] {
            let semaphore = semaphore.clone();
            joins.spawn(async move { scan_endpoint(host, port, semaphore).await });
        }
    }
    let mut found: BTreeMap<Ipv4Addr, DiscoveredConsole> = BTreeMap::new();
    while let Some(joined) = joins.join_next().await {
        let Ok(Some((host, port, result))) = joined else {
            continue;
        };
        let entry = found.entry(host).or_insert_with(|| DiscoveredConsole {
            host: host.to_string(),
            platform: "unknown".into(),
            receiver: None,
            ftp: None,
            label: "Unknown console".into(),
        });
        match (port, result) {
            (RECEIVER_PORT, EndpointResult::Receiver { version, platform }) => {
                entry.platform = platform.clone().unwrap_or_else(|| "unknown".into());
                entry.label = match platform.as_deref() {
                    Some("ps4") => format!("PS4 receiver {}", version.as_deref().unwrap_or("")),
                    Some("ps5") => format!("PS5 receiver {}", version.as_deref().unwrap_or("")),
                    _ => format!("Console receiver {}", version.as_deref().unwrap_or("")),
                }
                .trim()
                .to_string();
                entry.receiver = Some(DiscoveredReceiver {
                    port,
                    version,
                    platform,
                });
            }
            (
                FTP_PORT,
                EndpointResult::Ftp {
                    banner,
                    platform,
                    label,
                },
            ) => {
                if entry.receiver.is_none() {
                    entry.platform = platform.unwrap_or_else(|| "unknown".into());
                    entry.label = label;
                }
                entry.ftp = Some(DiscoveredFtp { port, banner });
            }
            _ => {}
        }
    }
    found.into_values().collect()
}

enum EndpointResult {
    Receiver {
        version: Option<String>,
        platform: Option<String>,
    },
    Ftp {
        banner: String,
        platform: Option<String>,
        label: String,
    },
}

async fn scan_endpoint(
    host: Ipv4Addr,
    port: u16,
    semaphore: std::sync::Arc<Semaphore>,
) -> Option<(Ipv4Addr, u16, EndpointResult)> {
    let _permit = semaphore.acquire_owned().await.ok()?;
    let mut stream = match timeout(SCAN_CONNECT_TIMEOUT, TcpStream::connect((host, port))).await {
        Ok(Ok(stream)) => stream,
        _ => return None,
    };
    let result = if port == RECEIVER_PORT {
        let (ping_code, ping_body) = exchange_scan(&mut stream, 0x01, &[]).await.ok()?;
        if ping_code != 1 || ping_body != b"SSPI" {
            return None;
        }
        let (config_code, body) = exchange_scan(&mut stream, 0x53, &[]).await.ok()?;
        if config_code != 3 {
            return None;
        }
        let config = parse_receiver_config(&body)?;
        EndpointResult::Receiver {
            version: Some(config.version),
            platform: config.platform,
        }
    } else {
        let banner = read_banner(&mut stream).await;
        let _ = timeout(PROBE_STEP_TIMEOUT, stream.write_all(b"QUIT\r\n")).await;
        let text = String::from_utf8_lossy(&banner).trim().to_string();
        let (platform, label) = classify_banner(&text);
        EndpointResult::Ftp {
            banner: text,
            platform,
            label,
        }
    };
    Some((host, port, result))
}

async fn exchange_scan(
    stream: &mut TcpStream,
    command: u8,
    body: &[u8],
) -> Result<(u8, Vec<u8>), String> {
    let body_len =
        u32::try_from(body.len()).map_err(|_| "The receiver request is too large".to_string())?;
    let mut request = Vec::with_capacity(5 + body.len());
    request.push(command);
    request.extend_from_slice(&body_len.to_le_bytes());
    request.extend_from_slice(body);
    timeout(Duration::from_millis(500), async {
        stream
            .write_all(&request)
            .await
            .map_err(|_| "Could not send receiver request".to_string())?;
        let mut header = [0_u8; 5];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|_| "Incomplete receiver response".to_string())?;
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if length > MAX_RESPONSE {
            return Err("Receiver response too large".into());
        }
        let mut body = vec![0; length];
        stream
            .read_exact(&mut body)
            .await
            .map_err(|_| "Incomplete receiver response".to_string())?;
        Ok((header[0], body))
    })
    .await
    .map_err(|_| "Receiver request timed out".to_string())?
}

async fn read_banner(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAX_FTP_BANNER);
    let _ = timeout(FTP_BANNER_TIMEOUT, async {
        while bytes.len() < MAX_FTP_BANNER {
            let byte = stream.read_u8().await?;
            bytes.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    })
    .await;
    bytes
}

fn classify_banner(banner: &str) -> (Option<String>, String) {
    let lower = banner.to_ascii_lowercase();
    if lower.contains("goldhen") {
        (Some("ps4".into()), "PS4, GoldHEN FTP".into())
    } else if lower.contains("etahen") {
        (Some("ps5".into()), "PS5, etaHEN FTP".into())
    } else if lower.contains("ps4") {
        (Some("ps4".into()), "PS4 FTP server".into())
    } else if lower.contains("ps5") {
        (Some("ps5".into()), "PS5 FTP server".into())
    } else {
        (None, "FTP server".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::{net::TcpListener, task::JoinHandle};

    async fn fake_receiver(config: Value, bad_ping: bool) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                for (expected_command, code, body) in [
                    (
                        0x01_u8,
                        1_u8,
                        if bad_ping {
                            b"wrong".to_vec()
                        } else {
                            b"SSPI".to_vec()
                        },
                    ),
                    (0x53_u8, 3_u8, serde_json::to_vec(&config).unwrap()),
                ] {
                    let mut request = [0_u8; 5];
                    if socket.read_exact(&mut request).await.is_err() {
                        break;
                    }
                    if request[0] != expected_command || request[1..] != [0, 0, 0, 0] {
                        break;
                    }
                    let mut response = vec![code];
                    response.extend_from_slice(&(body.len() as u32).to_le_bytes());
                    response.extend_from_slice(&body);
                    if socket.write_all(&response).await.is_err() {
                        break;
                    }
                }
            }
        });
        (port, task)
    }

    #[tokio::test]
    async fn probe_reports_online_outdated_offline_unconfigured_and_foreign_receiver() {
        let ps5_config =
            json!({"version": RECEIVER_VERSION, "platform":"ps5", "capabilities":["dump-mount"]});
        let (port, server) = fake_receiver(ps5_config, false).await;
        let online = probe_one("ps5", "127.0.0.1".into(), port, RECEIVER_VERSION).await;
        assert_eq!(online.receiver.state, "online");
        assert_eq!(online.receiver.version.as_deref(), Some(RECEIVER_VERSION));
        assert!(online.receiver.latency_ms.is_some());
        server.await.unwrap();

        let (port, server) = fake_receiver(
            json!({"version":"1.0.5", "platform":"ps5", "capabilities":["dump-mount"]}),
            false,
        )
        .await;
        let outdated = probe_one("ps5", "127.0.0.1".into(), port, "1.0.6").await;
        assert_eq!(outdated.receiver.state, "outdated");
        assert_eq!(
            outdated.receiver.message.as_deref(),
            Some("The PS5 receiver 1.0.5 is loaded; this app needs 1.0.6.")
        );
        server.await.unwrap();

        let (port, server) = fake_receiver(
            json!({"version":"1.0.3", "platform":"ps4", "capabilities":["ps4"]}),
            false,
        )
        .await;
        let foreign = probe_one("ps5", "127.0.0.1".into(), port, "1.0.6").await;
        assert_eq!(foreign.receiver.state, "error");
        server.await.unwrap();

        let unused = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = unused.local_addr().unwrap().port();
        drop(unused);
        let offline = probe_one("ps5", "127.0.0.1".into(), port, "1.0.6").await;
        assert_eq!(offline.receiver.state, "offline");
        assert_eq!(
            offline.receiver.message.as_deref(),
            Some(format!("Nothing answered at 127.0.0.1:{port}.").as_str())
        );

        let unconfigured = probe_one("ps4", String::new(), 9114, PS4_RECEIVER_VERSION).await;
        assert_eq!(unconfigured.receiver.state, "unconfigured");
    }

    #[tokio::test]
    async fn probe_treats_a_malformed_receiver_reply_as_an_error() {
        let (port, server) = fake_receiver(
            json!({"version":"1.0.6", "platform":"ps5", "capabilities":[]}),
            true,
        )
        .await;
        let result = probe_one("ps5", "127.0.0.1".into(), port, "1.0.6").await;
        assert_eq!(result.receiver.state, "error");
        server.await.unwrap();
    }

    #[test]
    fn private_ranges_and_hosts_for_networks_are_limited_to_private_ipv4() {
        assert!(is_private_ipv4(Ipv4Addr::new(10, 1, 2, 3)));
        assert!(is_private_ipv4(Ipv4Addr::new(172, 31, 255, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(192, 168, 1, 1)));
        for ip in [
            Ipv4Addr::new(172, 32, 0, 1),
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::LOCALHOST,
        ] {
            assert!(!is_private_ipv4(ip));
        }
        let hosts = hosts_for_networks(
            &[
                Ipv4Addr::new(192, 168, 7, 21),
                Ipv4Addr::new(192, 168, 7, 25),
            ],
            Some(Ipv4Addr::new(192, 168, 7, 21)),
        );
        assert_eq!(hosts.len(), 253);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 7, 1));
        assert_eq!(hosts[252], Ipv4Addr::new(192, 168, 7, 254));
        assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 7, 21)));
    }

    #[test]
    fn config_and_banner_platform_helpers_handle_legacy_and_named_receivers() {
        assert_eq!(
            platform_from_config_value(&json!({}), &["dump-mount".into()]).as_deref(),
            Some("ps5")
        );
        assert_eq!(
            platform_from_config_value(&json!({}), &["ps4".into()]).as_deref(),
            Some("ps4")
        );
        assert_eq!(platform_from_config_value(&json!({}), &[]), None);
        assert_eq!(
            classify_banner("220 GoldHEN FTP server"),
            (Some("ps4".into()), "PS4, GoldHEN FTP".into())
        );
        assert_eq!(
            classify_banner("220 etaHEN FTP server"),
            (Some("ps5".into()), "PS5, etaHEN FTP".into())
        );
        assert_eq!(classify_banner("220 Ready"), (None, "FTP server".into()));
    }
}
