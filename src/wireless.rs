use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qrcodegen::{QrCode, QrCodeEcc};

use crate::adb::{Adb, DeviceInfo, MdnsService, format_adb_endpoint};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualWirelessMode {
    PairOnly,
    ConnectOnly,
    PairAndConnect,
}

#[derive(Debug)]
struct ManualRequest {
    host: String,
    pair_port: Option<u16>,
    connect_port: Option<u16>,
    code: String,
    mode: ManualWirelessMode,
}

impl ManualRequest {
    fn parse(
        host: &str,
        pair_port: &str,
        connect_port: &str,
        code: &str,
        mode: ManualWirelessMode,
    ) -> Result<Self, String> {
        let (host, inline_port) = normalize_host(host)?;
        let pair_port = if mode == ManualWirelessMode::ConnectOnly {
            None
        } else {
            Some(
                resolve_port(pair_port, inline_port, "配对端口")?
                    .ok_or("请输入手机“使用配对码配对设备”弹窗中显示的配对端口")?,
            )
        };
        let connect_port = if mode == ManualWirelessMode::PairOnly {
            None
        } else {
            resolve_port(
                connect_port,
                if mode == ManualWirelessMode::ConnectOnly {
                    inline_port
                } else {
                    None
                },
                "连接端口",
            )?
        };
        if mode == ManualWirelessMode::ConnectOnly && connect_port.is_none() {
            return Err("请输入手机“无线调试”主页显示的连接端口，该端口与配对端口不同".to_owned());
        }
        let code = code.trim().to_owned();
        if mode != ManualWirelessMode::ConnectOnly
            && (code.len() != 6 || !code.chars().all(|character| character.is_ascii_digit()))
        {
            return Err("配对码必须是 6 位数字".to_owned());
        }
        Ok(Self {
            host,
            pair_port,
            connect_port,
            code,
            mode,
        })
    }
}

trait WirelessAdb {
    fn pair(&self, endpoint: &str, code: &str, stop: &AtomicBool) -> Result<String, String>;
    fn connect(&self, endpoint: &str, stop: &AtomicBool) -> Result<String, String>;
    fn mdns_services(&self, stop: &AtomicBool) -> Result<Vec<MdnsService>, String>;
    fn devices(&self, stop: &AtomicBool) -> Result<Vec<DeviceInfo>, String>;
}

impl WirelessAdb for Adb {
    fn pair(&self, endpoint: &str, code: &str, stop: &AtomicBool) -> Result<String, String> {
        self.pair(endpoint, code, stop)
    }

    fn connect(&self, endpoint: &str, stop: &AtomicBool) -> Result<String, String> {
        self.connect(endpoint, stop)
    }

    fn mdns_services(&self, stop: &AtomicBool) -> Result<Vec<MdnsService>, String> {
        self.mdns_services(stop)
    }

    fn devices(&self, stop: &AtomicBool) -> Result<Vec<DeviceInfo>, String> {
        self.wireless_devices(stop)
    }
}

#[derive(Debug)]
pub enum WirelessEvent {
    Progress(String),
    Finished { success: bool, message: String },
}

#[derive(Debug, Clone)]
pub struct QrPairing {
    pub service_name: String,
    pub code: String,
    pub payload: String,
}

#[derive(Debug, Clone)]
pub struct QrMatrix {
    pub size: usize,
    pub modules: Vec<bool>,
}

impl QrPairing {
    pub fn generate() -> Self {
        let entropy = random_u64();
        let service_name = format!("androidtool-{:08x}", entropy as u32);
        let code = format!("{:06}", (entropy >> 32) % 1_000_000);
        let payload = format!("WIFI:T:ADB;S:{service_name};P:{code};;");
        Self {
            service_name,
            code,
            payload,
        }
    }

    pub fn matrix(&self) -> Result<QrMatrix, String> {
        let qr = QrCode::encode_text(&self.payload, QrCodeEcc::Medium)
            .map_err(|error| format!("无法生成二维码: {error}"))?;
        let size = qr.size() as usize;
        let mut modules = Vec::with_capacity(size * size);
        for y in 0..qr.size() {
            for x in 0..qr.size() {
                modules.push(qr.get_module(x, y));
            }
        }
        Ok(QrMatrix { size, modules })
    }
}

pub struct WirelessSession {
    receiver: Receiver<WirelessEvent>,
    stop: Arc<AtomicBool>,
}

impl WirelessSession {
    pub fn restart_adb(adb_path: std::path::PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || {
            let result = Adb::new(adb_path).restart_server(&thread_stop);
            let event = match result {
                Ok(message) => WirelessEvent::Finished {
                    success: true,
                    message,
                },
                Err(message) => WirelessEvent::Finished {
                    success: false,
                    message,
                },
            };
            let _ = sender.send(event);
        });
        Self { receiver, stop }
    }

    pub fn start_manual(
        adb_path: std::path::PathBuf,
        host: String,
        pair_port: String,
        connect_port: String,
        code: String,
        mode: ManualWirelessMode,
    ) -> Result<Self, String> {
        let request = ManualRequest::parse(&host, &pair_port, &connect_port, &code, mode)?;

        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || {
            let result = run_manual(&Adb::new(adb_path), &request, &sender, &thread_stop);
            let _ = sender.send(match result {
                Ok(message) => WirelessEvent::Finished {
                    success: true,
                    message,
                },
                Err(message) => WirelessEvent::Finished {
                    success: false,
                    message,
                },
            });
        });
        Ok(Self { receiver, stop })
    }

    pub fn start_qr(adb_path: std::path::PathBuf, pairing: QrPairing) -> Self {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || {
            let result = run_qr(&Adb::new(adb_path), &pairing, &sender, &thread_stop);
            let _ = sender.send(match result {
                Ok(message) => WirelessEvent::Finished {
                    success: true,
                    message,
                },
                Err(message) => WirelessEvent::Finished {
                    success: false,
                    message,
                },
            });
        });
        Self { receiver, stop }
    }

    pub fn try_recv(&self) -> Option<WirelessEvent> {
        self.receiver.try_recv().ok()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for WirelessSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_manual(
    adb: &impl WirelessAdb,
    request: &ManualRequest,
    sender: &Sender<WirelessEvent>,
    stop: &AtomicBool,
) -> Result<String, String> {
    let mut progress = String::new();
    let mut guid = None;
    if let Some(port) = request.pair_port {
        let endpoint = format_adb_endpoint(&request.host, port);
        send_progress(sender, &mut progress, format!("正在配对 {endpoint}…"));
        let output = adb
            .pair(&endpoint, &request.code, stop)
            .map_err(|error| format!("配对 {endpoint} 失败：{error}"))?;
        if request.mode == ManualWirelessMode::PairOnly {
            return Ok(format!("设备配对成功：{endpoint}\n{output}"));
        }
        guid = parse_pair_guid(&output);
    }
    if let Some(port) = request.connect_port {
        let endpoint = format_adb_endpoint(&request.host, port);
        send_progress(sender, &mut progress, format!("正在连接 {endpoint}…"));
        let output = adb.connect(&endpoint, stop).map_err(|error| {
            format!(
                "{}连接 {endpoint} 失败：{error}\n请使用“无线调试”主页的连接端口；它与配对端口不同，通常也不是 5555。",
                if request.mode == ManualWirelessMode::PairAndConnect { "配对成功，但" } else { "" }
            )
        })?;
        wait_until_online(adb, &endpoint, guid.as_deref(), stop)?;
        return Ok(format!(
            "无线 ADB 已连接并确认设备在线：{endpoint}\n{output}"
        ));
    }
    send_progress(
        sender,
        &mut progress,
        "配对成功，正在自动发现该手机的无线调试连接端口…",
    );
    connect_discovered_service(
        adb,
        guid.as_deref(),
        &request.host,
        sender,
        stop,
        &mut progress,
    )
}

fn run_qr(
    adb: &impl WirelessAdb,
    pairing: &QrPairing,
    sender: &Sender<WirelessEvent>,
    stop: &AtomicBool,
) -> Result<String, String> {
    const WAIT_TIMEOUT: Duration = Duration::from_secs(120);
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut last_progress = String::new();
    let mut last_error = String::new();
    let mut discovery_failures = 0;

    send_progress(
        sender,
        &mut last_progress,
        "等待手机扫描二维码并建立配对服务…",
    );

    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        match adb.mdns_services(stop) {
            Ok(services) => {
                discovery_failures = 0;
                last_error.clear();
                if let Some(service) = services
                    .iter()
                    .find(|service| is_pairing_service(service, &pairing.service_name))
                {
                    send_progress(
                        sender,
                        &mut last_progress,
                        format!("已发现配对服务 {}，正在提交配对码…", service.endpoint),
                    );
                    match adb.pair(&service.endpoint, &pairing.code, stop) {
                        Ok(output) => {
                            let guid = parse_pair_guid(&output);
                            send_progress(
                                sender,
                                &mut last_progress,
                                "配对成功，正在等待无线调试连接端口…",
                            );
                            return connect_discovered_service(
                                adb,
                                guid.as_deref(),
                                &endpoint_host(&service.endpoint),
                                sender,
                                stop,
                                &mut last_progress,
                            );
                        }
                        Err(error) => {
                            return Err(format!("二维码配对失败：{error}"));
                        }
                    }
                }
            }
            Err(error) => {
                discovery_failures += 1;
                if discovery_failures >= 3 {
                    return Err(format!(
                        "mDNS 连续失败，已停止等待：{error}\n可确认后重启 ADB 服务，或改用配对码并手动填写连接端口。"
                    ));
                }
                if last_error != error {
                    last_error = error;
                    send_progress(
                        sender,
                        &mut last_progress,
                        format!("mDNS 暂不可用，正在重试：{last_error}"),
                    );
                }
            }
        }
        sleep_unless_stopped(stop, Duration::from_secs(1));
    }

    if stop.load(Ordering::Relaxed) {
        Err("已停止等待二维码配对".to_owned())
    } else if last_error.is_empty() {
        Err("等待超时。请确认手机与电脑在同一局域网，并重新打开无线调试二维码。".to_owned())
    } else {
        Err(format!("等待超时，ADB mDNS 状态：{last_error}"))
    }
}

fn connect_discovered_service(
    adb: &impl WirelessAdb,
    guid: Option<&str>,
    host: &str,
    sender: &Sender<WirelessEvent>,
    stop: &AtomicBool,
    last_progress: &mut String,
) -> Result<String, String> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = String::new();
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        // ADB may have auto-connected the paired phone before mDNS is polled again.
        if let Some(guid) = guid
            && let Ok(devices) = adb.devices(stop)
            && let Some(device) = devices
                .iter()
                .find(|device| device.is_online() && device_matches(device, None, Some(guid)))
        {
            return Ok(format!("设备配对成功，ADB 已自动连接：{}", device.serial));
        }
        match adb.mdns_services(stop) {
            Ok(services) => {
                if let Some(service) = select_connect_service(&services, guid, host) {
                    send_progress(
                        sender,
                        last_progress,
                        format!("正在连接无线调试服务 {}…", service.endpoint),
                    );
                    match adb.connect(&service.endpoint, stop) {
                        Ok(output) => {
                            wait_until_online(adb, &service.endpoint, guid, stop)?;
                            return Ok(format!(
                                "配对并连接成功，设备已在线：{}\n{output}",
                                service.endpoint
                            ));
                        }
                        Err(error) => last_error = error,
                    }
                }
            }
            Err(error) => last_error = error,
        }
        sleep_unless_stopped(stop, Duration::from_secs(1));
    }

    if stop.load(Ordering::Relaxed) {
        return Err("配对成功，但连接等待已停止".to_owned());
    }
    if last_error.is_empty() {
        Err(format!(
            "配对成功，但未发现目标手机 {host} 的无线调试连接服务。请在 IP 配对页输入手机“无线调试”主页的 IP 和连接端口，点击“仅连接”。请同时检查局域网是否屏蔽 mDNS（UDP 5353）。"
        ))
    } else {
        Err(format!("配对成功，但无线连接失败：{last_error}"))
    }
}

fn is_pairing_service(service: &MdnsService, requested_name: &str) -> bool {
    service.service_type == "_adb-tls-pairing._tcp"
        && service_instance_name(&service.name).eq_ignore_ascii_case(requested_name)
}

fn select_connect_service<'a>(
    services: &'a [MdnsService],
    guid: Option<&str>,
    host: &str,
) -> Option<&'a MdnsService> {
    let mut candidates = services
        .iter()
        .filter(|service| service.service_type == "_adb-tls-connect._tcp")
        .filter(|service| match guid {
            Some(guid) => matches_guid(&service.name, guid),
            None => endpoint_host(&service.endpoint).eq_ignore_ascii_case(host),
        });
    let selected = candidates.next()?;
    // Without a GUID, multiple different services on one host are ambiguous.
    if guid.is_none() && candidates.any(|other| other.name != selected.name) {
        return None;
    }
    Some(selected)
}

fn service_instance_name(name: &str) -> &str {
    name.split("._adb-tls-")
        .next()
        .unwrap_or(name)
        .trim_end_matches('.')
}

fn matches_guid(name: &str, guid: &str) -> bool {
    let name = service_instance_name(name).to_ascii_lowercase();
    let guid = guid.to_ascii_lowercase();
    name == guid
        || name
            .strip_prefix(&guid)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

fn endpoint_host(endpoint: &str) -> String {
    if let Ok(address) = endpoint.parse::<std::net::SocketAddr>() {
        return address.ip().to_string();
    }
    endpoint
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(endpoint)
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

fn device_matches(device: &DeviceInfo, endpoint: Option<&str>, guid: Option<&str>) -> bool {
    endpoint.is_some_and(|endpoint| device.serial == endpoint)
        || guid.is_some_and(|guid| matches_guid(&device.serial, guid))
}

fn wait_until_online(
    adb: &impl WirelessAdb,
    endpoint: &str,
    guid: Option<&str>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut state = "未出现在设备列表中".to_owned();
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        match adb.devices(stop) {
            Ok(devices) => {
                let mut matching = devices
                    .iter()
                    .filter(|device| device_matches(device, Some(endpoint), guid));
                if matching.clone().any(DeviceInfo::is_online) {
                    return Ok(());
                }
                if let Some(device) = matching.next() {
                    state = device.state.clone();
                }
            }
            Err(error) => state = error,
        }
        sleep_unless_stopped(stop, Duration::from_millis(250));
    }
    if stop.load(Ordering::Relaxed) {
        Err("操作已取消".to_owned())
    } else {
        Err(format!(
            "ADB 已接受连接，但目标设备尚未在线（{endpoint}：{state}）。请确认手机上的调试授权，或重新打开无线调试后重试。"
        ))
    }
}

fn parse_pair_guid(output: &str) -> Option<String> {
    let value = output.split("guid=").nth(1)?;
    let guid = value
        .split(|character: char| character.is_whitespace() || matches!(character, ']' | ',' | ';'))
        .next()?
        .trim();
    (!guid.is_empty()).then(|| guid.to_owned())
}

fn normalize_host(host: &str) -> Result<(String, Option<u16>), String> {
    let host = host
        .trim()
        .trim_start_matches("adb://")
        .trim_end_matches('/');
    if host.is_empty() {
        return Err("请输入设备 IP 地址或主机名".to_owned());
    }
    if host.chars().any(char::is_whitespace) {
        return Err("IP 地址或主机名中不能包含空格".to_owned());
    }
    if let Ok(address) = host.parse::<std::net::SocketAddr>() {
        return Ok((address.ip().to_string(), Some(address.port())));
    }
    if let Some(rest) = host.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']').ok_or("IPv6 地址缺少右方括号")?;
        let port = if suffix.is_empty() {
            None
        } else {
            resolve_port(
                suffix.strip_prefix(':').ok_or("IPv6 地址端口格式不正确")?,
                None,
                "地址中的端口",
            )?
        };
        return Ok((host.to_owned(), port));
    }
    if host.matches(':').count() == 1 {
        let (host, port) = host.split_once(':').expect("one colon");
        if host.is_empty() {
            return Err("请输入设备 IP 地址或主机名".to_owned());
        }
        return Ok((host.to_owned(), resolve_port(port, None, "地址中的端口")?));
    }
    Ok((host.to_owned(), None))
}

fn resolve_port(value: &str, fallback: Option<u16>, label: &str) -> Result<Option<u16>, String> {
    if value.trim().is_empty() {
        return Ok(fallback.filter(|port| *port != 0));
    }
    let port = value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("{label}必须是 1–65535 之间的数字"))?;
    if fallback.is_some_and(|inline| inline != port) {
        return Err(format!("{label}与 IP 地址后附带的端口不一致"));
    }
    Ok(Some(port))
}

fn send_progress(sender: &Sender<WirelessEvent>, last: &mut String, message: impl Into<String>) {
    let message = message.into();
    if *last != message {
        *last = message.clone();
        let _ = sender.send(WirelessEvent::Progress(message));
    }
}

fn sleep_unless_stopped(stop: &AtomicBool, duration: Duration) {
    let started = Instant::now();
    while started.elapsed() < duration && !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
}

fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut value = now ^ sequence.rotate_left(17) ^ (std::process::id() as u64) << 32;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn qr_payload_uses_android_wireless_pairing_schema() {
        let pairing = QrPairing::generate();
        assert_eq!(
            pairing.payload,
            format!("WIFI:T:ADB;S:{};P:{};;", pairing.service_name, pairing.code)
        );
        assert_eq!(pairing.code.len(), 6);
        assert!(pairing.matrix().is_ok());
    }

    #[test]
    fn parses_pairing_guid_from_adb_output() {
        assert_eq!(
            parse_pair_guid("Successfully paired to 192.168.1.9:37123 [guid=adb-R58M-AbCdEf]"),
            Some("adb-R58M-AbCdEf".to_owned())
        );
    }

    #[test]
    fn matches_pairing_and_connect_services() {
        let pairing_service = MdnsService {
            name: "androidtool-a1b2c3d4".to_owned(),
            service_type: "_adb-tls-pairing._tcp".to_owned(),
            endpoint: "192.168.1.8:37123".to_owned(),
        };
        assert!(is_pairing_service(&pairing_service, "androidtool-a1b2c3d4"));

        let services = vec![MdnsService {
            name: "adb-R58M-AbCdEf".to_owned(),
            service_type: "_adb-tls-connect._tcp".to_owned(),
            endpoint: "192.168.1.8:40001".to_owned(),
        }];
        assert_eq!(
            select_connect_service(&services, Some("adb-R58M-AbCdEf"), "192.168.1.8")
                .map(|service| service.endpoint.as_str()),
            Some("192.168.1.8:40001")
        );
    }

    fn connect_service(name: &str, endpoint: &str) -> MdnsService {
        MdnsService {
            name: name.to_owned(),
            service_type: "_adb-tls-connect._tcp".to_owned(),
            endpoint: endpoint.to_owned(),
        }
    }

    #[test]
    fn does_not_connect_to_another_device_when_guid_is_missing_from_discovery() {
        let services = vec![connect_service("adb-other-token", "192.168.1.9:42000")];
        assert!(
            select_connect_service(&services, Some("adb-target-token"), "192.168.1.8").is_none()
        );
        assert!(select_connect_service(&services, None, "192.168.1.8").is_none());
    }

    #[test]
    fn matches_complete_guid_and_service_suffix_not_substrings() {
        let services = vec![
            connect_service("adb-target-token-other", "192.168.1.8:42000"),
            connect_service(
                "adb-R58M-token-extra._adb-tls-connect._tcp.local.",
                "192.168.1.9:43000",
            ),
        ];
        assert!(!matches_guid("adb-target-token", "token"));
        assert!(select_connect_service(&services, Some("adb-target"), "unused").is_some());
        assert!(select_connect_service(&services, Some("adb-R58M-token"), "unused").is_some());
    }

    #[test]
    fn falls_back_to_same_host_only_without_guid() {
        let mut services = vec![
            connect_service("adb-other", "192.168.1.9:42000"),
            connect_service("adb-target", "192.168.1.8:43000"),
        ];
        assert_eq!(
            select_connect_service(&services, None, "192.168.1.8")
                .unwrap()
                .name,
            "adb-target"
        );
        services.push(connect_service("adb-second-target", "192.168.1.8:44000"));
        assert!(select_connect_service(&services, None, "192.168.1.8").is_none());
        assert_eq!(endpoint_host("[fe80::1]:42123"), "fe80::1");
    }

    #[test]
    fn rejects_partial_qr_service_name_matches() {
        let mut service = MdnsService {
            name: "old-androidtool-abcd".to_owned(),
            service_type: "_adb-tls-pairing._tcp".to_owned(),
            endpoint: "192.168.1.8:37123".to_owned(),
        };
        assert!(!is_pairing_service(&service, "androidtool-abcd"));
        service.name = "androidtool-abcd._adb-tls-pairing._tcp.local.".to_owned();
        assert!(is_pairing_service(&service, "androidtool-abcd"));
    }

    #[test]
    fn validates_only_the_ports_needed_by_the_selected_action() {
        let request = ManualRequest::parse(
            "192.168.1.8",
            "invalid",
            "41234",
            "",
            ManualWirelessMode::ConnectOnly,
        )
        .unwrap();
        assert_eq!(request.pair_port, None);
        assert_eq!(request.connect_port, Some(41234));
        let request = ManualRequest::parse(
            "192.168.1.8",
            "37123",
            "invalid",
            "001234",
            ManualWirelessMode::PairOnly,
        )
        .unwrap();
        assert_eq!(request.connect_port, None);
        let request = ManualRequest::parse(
            "192.168.1.8",
            "37123",
            "",
            "001234",
            ManualWirelessMode::PairAndConnect,
        )
        .unwrap();
        assert_eq!(request.connect_port, None);
        assert!(
            ManualRequest::parse("192.168.1.8", "", "", "", ManualWirelessMode::ConnectOnly)
                .is_err()
        );
    }

    #[test]
    fn accepts_copied_ip_port_and_ipv6_addresses() {
        let request = ManualRequest::parse(
            "192.168.1.8:37123",
            "",
            "",
            "001234",
            ManualWirelessMode::PairAndConnect,
        )
        .unwrap();
        assert_eq!(request.host, "192.168.1.8");
        assert_eq!(request.pair_port, Some(37123));
        assert_eq!(request.connect_port, None);
        let request = ManualRequest::parse(
            "adb://[fe80::1]:41234",
            "",
            "",
            "",
            ManualWirelessMode::ConnectOnly,
        )
        .unwrap();
        assert_eq!(request.host, "fe80::1");
        assert_eq!(request.connect_port, Some(41234));
        assert!(
            ManualRequest::parse(
                "192.168.1.8:37123",
                "37124",
                "",
                "001234",
                ManualWirelessMode::PairOnly
            )
            .is_err()
        );
        assert!(
            ManualRequest::parse(
                "192.168.1.8",
                "0",
                "",
                "001234",
                ManualWirelessMode::PairOnly
            )
            .is_err()
        );
    }

    #[derive(Default)]
    struct FakeAdb {
        services: Vec<MdnsService>,
        calls: RefCell<Vec<String>>,
        online_before_connect: bool,
        discovery_error: bool,
    }

    impl WirelessAdb for FakeAdb {
        fn pair(&self, endpoint: &str, _code: &str, _stop: &AtomicBool) -> Result<String, String> {
            self.calls.borrow_mut().push(format!("pair {endpoint}"));
            Ok(format!(
                "Successfully paired to {endpoint} [guid=adb-target-token]"
            ))
        }

        fn connect(&self, endpoint: &str, _stop: &AtomicBool) -> Result<String, String> {
            self.calls.borrow_mut().push(format!("connect {endpoint}"));
            Ok(format!("connected to {endpoint}"))
        }

        fn mdns_services(&self, _stop: &AtomicBool) -> Result<Vec<MdnsService>, String> {
            self.calls.borrow_mut().push("mdns".to_owned());
            if self.discovery_error {
                return Err("ADB timeout".to_owned());
            }
            Ok(self.services.clone())
        }

        fn devices(&self, _stop: &AtomicBool) -> Result<Vec<DeviceInfo>, String> {
            let connected = self
                .calls
                .borrow()
                .iter()
                .any(|call| call.starts_with("connect "));
            self.calls.borrow_mut().push("devices".to_owned());
            Ok(if self.online_before_connect || connected {
                vec![DeviceInfo {
                    serial: "adb-target-token._adb-tls-connect._tcp".to_owned(),
                    state: "device".to_owned(),
                    ..Default::default()
                }]
            } else {
                Vec::new()
            })
        }
    }

    #[test]
    fn repeated_discovery_errors_stop_qr_wait_with_recovery_instructions() {
        let adb = FakeAdb {
            discovery_error: true,
            ..Default::default()
        };
        let (sender, _) = mpsc::channel();
        let error = run_qr(
            &adb,
            &QrPairing::generate(),
            &sender,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.contains("重启 ADB"));
        assert!(error.contains("ADB timeout"));
        assert_eq!(*adb.calls.borrow(), ["mdns", "mdns", "mdns"]);
    }

    #[test]
    fn pairing_discovers_dynamic_connection_port_and_confirms_online_state() {
        let adb = FakeAdb {
            services: vec![
                connect_service("adb-other", "192.168.1.9:42000"),
                connect_service("adb-target-token", "192.168.1.8:43000"),
            ],
            ..Default::default()
        };
        let request = ManualRequest::parse(
            "192.168.1.8",
            "37123",
            "",
            "001234",
            ManualWirelessMode::PairAndConnect,
        )
        .unwrap();
        let (sender, _) = mpsc::channel();
        let result = run_manual(&adb, &request, &sender, &AtomicBool::new(false)).unwrap();
        assert!(result.contains("192.168.1.8:43000"));
        assert_eq!(
            *adb.calls.borrow(),
            [
                "pair 192.168.1.8:37123",
                "devices",
                "mdns",
                "connect 192.168.1.8:43000",
                "devices"
            ]
        );
    }

    #[test]
    fn qr_scan_pairs_connects_and_verifies_the_correct_device() {
        let pairing = QrPairing::generate();
        let adb = FakeAdb {
            services: vec![
                MdnsService {
                    name: pairing.service_name.clone(),
                    service_type: "_adb-tls-pairing._tcp".to_owned(),
                    endpoint: "192.168.1.8:37123".to_owned(),
                },
                connect_service("adb-other", "192.168.1.9:42000"),
                connect_service("adb-target-token", "192.168.1.8:43000"),
            ],
            ..Default::default()
        };
        let (sender, _) = mpsc::channel();
        assert!(run_qr(&adb, &pairing, &sender, &AtomicBool::new(false)).is_ok());
        assert_eq!(
            *adb.calls.borrow(),
            [
                "mdns",
                "pair 192.168.1.8:37123",
                "devices",
                "mdns",
                "connect 192.168.1.8:43000",
                "devices"
            ]
        );
    }

    #[test]
    fn accepts_adb_auto_connection_without_another_connect_command() {
        let adb = FakeAdb {
            online_before_connect: true,
            ..Default::default()
        };
        let request = ManualRequest::parse(
            "192.168.1.8",
            "37123",
            "",
            "001234",
            ManualWirelessMode::PairAndConnect,
        )
        .unwrap();
        let (sender, _) = mpsc::channel();
        assert!(run_manual(&adb, &request, &sender, &AtomicBool::new(false)).is_ok());
        assert_eq!(*adb.calls.borrow(), ["pair 192.168.1.8:37123", "devices"]);
    }
}
