use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::util;

#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub serial: String,
    pub state: String,
    pub model: String,
    pub product: String,
    pub device: String,
    pub transport_id: String,
    pub connection: String,
}

impl DeviceInfo {
    pub fn display_name(&self) -> String {
        if self.model.is_empty() {
            self.serial.clone()
        } else {
            self.model.replace('_', " ")
        }
    }

    pub fn is_online(&self) -> bool {
        self.state == "device"
    }

    pub fn state_label(&self) -> &'static str {
        match self.state.as_str() {
            "device" => "在线",
            "unauthorized" => "等待授权",
            "offline" => "离线",
            "recovery" => "Recovery",
            "sideload" => "Sideload",
            _ => "未知",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsService {
    pub name: String,
    pub service_type: String,
    pub endpoint: String,
}

#[derive(Debug, Clone)]
pub struct Adb {
    pub path: PathBuf,
}

impl Adb {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        self.wireless_devices(&AtomicBool::new(false))
    }

    pub fn shell(&self, serial: &str, args: &[&str]) -> Result<String, String> {
        let mut full_args = vec!["-s", serial, "shell"];
        full_args.extend_from_slice(args);
        let output = util::run(&self.path, &full_args)?;
        if !output.status.success() {
            return Err(util::output_text(&output));
        }
        if args.first() == Some(&"input") {
            let text = util::output_text(&output);
            if text.contains("Exception")
                || text.contains("Error:")
                || text.contains("Permission denied")
            {
                return Err(text);
            }
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn shell_cancellable(
        &self,
        serial: &str,
        args: &[&str],
        stop: &AtomicBool,
    ) -> Result<String, String> {
        let mut full_args = vec!["-s", serial, "shell"];
        full_args.extend_from_slice(args);
        let output = self.wireless_command(&full_args, Duration::from_secs(8), stop)?;
        if !output.status.success() {
            return Err(util::output_text(&output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn exec_out(&self, serial: &str, args: &[&str]) -> Result<Vec<u8>, String> {
        let mut full_args = vec!["-s", serial, "exec-out"];
        full_args.extend_from_slice(args);
        let output = util::run(&self.path, &full_args)?;
        if !output.status.success() {
            return Err(util::output_text(&output));
        }
        Ok(output.stdout)
    }

    pub fn push(&self, serial: &str, local: &Path, remote: &str) -> Result<String, String> {
        let output = util::run(
            &self.path,
            &["-s", serial, "push", &local.to_string_lossy(), remote],
        )?;
        if !output.status.success() {
            return Err(util::output_text(&output));
        }
        Ok(util::output_text(&output))
    }

    pub fn pull(&self, serial: &str, remote: &str, local: &Path) -> Result<String, String> {
        let output = util::run(
            &self.path,
            &["-s", serial, "pull", remote, &local.to_string_lossy()],
        )?;
        if !output.status.success() {
            return Err(util::output_text(&output));
        }
        Ok(util::output_text(&output))
    }

    pub fn install(&self, serial: &str, apk: &Path) -> Result<String, String> {
        let output = util::run(
            &self.path,
            &["-s", serial, "install", "-r", "-d", &apk.to_string_lossy()],
        )?;
        let text = util::output_text(&output);
        if !output.status.success() || !text.contains("Success") {
            return Err(text);
        }
        Ok(text)
    }

    pub fn pair(&self, endpoint: &str, code: &str, stop: &AtomicBool) -> Result<String, String> {
        let output =
            self.wireless_command(&["pair", endpoint, code], Duration::from_secs(20), stop)?;
        let text = util::output_text(&output);
        if !output.status.success() || !pair_succeeded(&text) {
            return Err(non_empty_error(text, "ADB 配对失败"));
        }
        Ok(text)
    }

    pub fn connect(&self, endpoint: &str, stop: &AtomicBool) -> Result<String, String> {
        let output =
            self.wireless_command(&["connect", endpoint], Duration::from_secs(10), stop)?;
        let text = util::output_text(&output);
        if !output.status.success() || !connect_succeeded(&text) {
            return Err(non_empty_error(text, "ADB 无线连接失败"));
        }
        Ok(text)
    }

    pub fn mdns_services(&self, stop: &AtomicBool) -> Result<Vec<MdnsService>, String> {
        let output = self
            .wireless_command(&["mdns", "services"], Duration::from_secs(5), stop)
            .map_err(|error| format!("mDNS 服务发现失败：{error}。内置 ADB 仍会共享默认后台服务；请停止等待后重启 ADB 服务。"))?;
        let text = util::output_text(&output);
        if !output.status.success() || command_failed(&text) {
            return Err(non_empty_error(text, "无法读取 ADB mDNS 服务"));
        }
        let services = parse_mdns_services(&text);
        if services.is_empty() {
            let check = self.wireless_command(&["mdns", "check"], Duration::from_secs(5), stop)?;
            let check_text = util::output_text(&check);
            if !check.status.success() || command_failed(&check_text) {
                return Err(format!(
                    "ADB mDNS 不可用：{}。请检查是否有其他 SDK 的 ADB 服务正在运行；关闭相关工具后重启 ADB 服务，并检查防火墙和局域网设置。也可使用配对码并手动填写连接端口。",
                    non_empty_error(check_text, "无法检查 mDNS 状态")
                ));
            }
        }
        Ok(services)
    }

    pub fn wireless_devices(&self, stop: &AtomicBool) -> Result<Vec<DeviceInfo>, String> {
        let output = self.wireless_command(&["devices", "-l"], Duration::from_secs(5), stop)?;
        if !output.status.success() {
            return Err(util::output_text(&output));
        }
        Ok(parse_devices(&String::from_utf8_lossy(&output.stdout)))
    }

    fn wireless_command(
        &self,
        args: &[&str],
        timeout: Duration,
        stop: &AtomicBool,
    ) -> Result<std::process::Output, String> {
        util::run_cancellable(util::command(&self.path).args(args), timeout, stop)
    }

    pub fn screenshot(&self, serial: &str) -> Result<Vec<u8>, String> {
        self.exec_out(serial, &["screencap", "-p"])
    }

    pub fn start_server(&self) -> Result<(), String> {
        let output = self.wireless_command(
            &["start-server"],
            Duration::from_secs(15),
            &AtomicBool::new(false),
        )?;
        if output.status.success() {
            Ok(())
        } else {
            Err(util::output_text(&output))
        }
    }

    pub fn restart_server(&self, stop: &AtomicBool) -> Result<String, String> {
        for args in [["kill-server"], ["start-server"]] {
            let output = self.wireless_command(&args, Duration::from_secs(15), stop)?;
            if !output.status.success() {
                return Err(non_empty_error(
                    util::output_text(&output),
                    "重启 ADB 服务失败",
                ));
            }
        }
        let output = self.wireless_command(&["mdns", "check"], Duration::from_secs(10), stop)?;
        let text = util::output_text(&output);
        if !output.status.success() || command_failed(&text) {
            return Err(format!(
                "ADB 服务已重启，但 mDNS 仍不可用：{text}。请检查防火墙，或使用配对码并手动填写连接端口。"
            ));
        }
        Ok("ADB 服务已重启，mDNS 检查通过。请点击“重新等待扫描”，并重新扫描二维码。".to_owned())
    }

    pub fn spawn_logcat(&self, serial: &str) -> Result<std::process::Child, String> {
        util::command(&self.path)
            .args(["-s", serial, "logcat", "-v", "threadtime", "-T", "400"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法启动 logcat: {error}"))
    }
}

pub fn format_adb_endpoint(host: &str, port: u16) -> String {
    let host = host
        .trim()
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub fn parse_mdns_services(text: &str) -> Vec<MdnsService> {
    let mut services = Vec::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(type_index) = tokens
            .iter()
            .position(|token| token.starts_with("_adb-tls-"))
        else {
            continue;
        };
        if type_index == 0 || type_index + 1 >= tokens.len() {
            continue;
        }
        let name = tokens[..type_index].join(" ");
        let service_type = tokens[type_index].trim_end_matches('.').to_owned();
        let endpoint = tokens[type_index + 1]
            .trim_matches(|character| character == ',' || character == ';')
            .to_owned();
        if name.is_empty() || endpoint.is_empty() {
            continue;
        }
        services.push(MdnsService {
            name,
            service_type,
            endpoint,
        });
    }
    services
}

fn command_failed(text: &str) -> bool {
    let lowercase = text.to_ascii_lowercase();
    [
        "failed",
        "failure",
        "cannot",
        "unable",
        "unavailable",
        "error:",
        "not found",
        "protocol fault",
    ]
    .iter()
    .any(|needle| lowercase.contains(needle))
}

fn pair_succeeded(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim().starts_with("Successfully paired to "))
}

fn connect_succeeded(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("connected to ") || line.starts_with("already connected to ")
    })
}

fn non_empty_error(text: String, fallback: &str) -> String {
    if text.trim().is_empty() {
        fallback.to_owned()
    } else {
        text
    }
}

pub fn parse_devices(text: &str) -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("List of devices")
            || line.starts_with("adb server")
            || line.starts_with('*')
        {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else {
            continue;
        };
        let Some(state) = parts.next() else {
            continue;
        };

        let mut device = DeviceInfo {
            serial: serial.to_owned(),
            state: state.to_owned(),
            ..Default::default()
        };

        for field in parts {
            if let Some((key, value)) = field.split_once(':') {
                match key {
                    "model" => device.model = value.to_owned(),
                    "product" => device.product = value.to_owned(),
                    "device" => device.device = value.to_owned(),
                    "transport_id" => device.transport_id = value.to_owned(),
                    "usb" => device.connection = format!("USB {value}"),
                    other => {
                        if device.connection.is_empty() && other.contains("wifi") {
                            device.connection = "无线 ADB".to_owned();
                        }
                    }
                }
            }
        }

        if device.connection.is_empty() {
            device.connection = if serial.contains(':') {
                "无线 ADB".to_owned()
            } else if serial.starts_with("emulator-") {
                "模拟器".to_owned()
            } else {
                "USB".to_owned()
            };
        }
        devices.push(device);
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_and_emulator_devices() {
        let text = "\
List of devices attached
R58M123456 device usb:1-2 product:beyond1q model:SM_G9730 device:beyond1q transport_id:3
emulator-5554 device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa transport_id:7
ABC123 unauthorized usb:2-1 transport_id:4
";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].model, "SM_G9730");
        assert_eq!(devices[0].connection, "USB 1-2");
        assert_eq!(devices[1].connection, "模拟器");
        assert_eq!(devices[2].state, "unauthorized");
        assert!(!devices[2].is_online());
    }

    #[test]
    fn parses_adb_mdns_services() {
        let text = "\
List of discovered mdns services
androidtool-a1b2c3d4 _adb-tls-pairing._tcp. 192.168.1.8:37123
adb-R58M123456-AbCdEf _adb-tls-connect._tcp. 192.168.1.8:40001
";
        let services = parse_mdns_services(text);
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].name, "androidtool-a1b2c3d4");
        assert_eq!(services[0].service_type, "_adb-tls-pairing._tcp");
        assert_eq!(services[0].endpoint, "192.168.1.8:37123");
        assert_eq!(services[1].endpoint, "192.168.1.8:40001");
    }

    #[test]
    fn formats_wireless_endpoints() {
        assert_eq!(format_adb_endpoint("192.168.1.8", 5555), "192.168.1.8:5555");
        assert_eq!(format_adb_endpoint("fe80::1", 5555), "[fe80::1]:5555");
    }

    #[test]
    fn requires_explicit_adb_success_messages() {
        assert!(command_failed("ERROR: mdns daemon unavailable"));
        assert!(!command_failed("mdns daemon version [Openscreen]"));
        assert!(pair_succeeded(
            "Successfully paired to 192.168.1.8:37123 [guid=adb-phone-token]"
        ));
        assert!(!pair_succeeded(""));
        assert!(!pair_succeeded(
            "error: protocol fault (couldn't read status message)"
        ));
        assert!(connect_succeeded("connected to 192.168.1.8:43000"));
        assert!(connect_succeeded("already connected to 192.168.1.8:43000"));
        assert!(!connect_succeeded("failed to connect to 192.168.1.8:43000"));
        assert!(!connect_succeeded("not connected to 192.168.1.8:43000"));
    }
}
