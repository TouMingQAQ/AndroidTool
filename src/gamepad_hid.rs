//! Standard HID gamepad reports over the bundled scrcpy 5.0.1 control protocol.
//! Based on scrcpy's report layout, with standard pedal usages and L2/R2 bits.
//! Descriptor source: Genymobile scrcpy, Apache-2.0 (see assets license).
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::{adb::Adb, gamepad::GamepadVisualState, util};

const DESCRIPTOR: &[u8] = include_bytes!("../assets/gamepad-report-descriptor.bin");
const NAME: &str = "AndroidTool Gamepad";
const KEYS: [(&str, u16); 11] = [
    ("south", 1),
    ("east", 2),
    ("west", 8),
    ("north", 16),
    ("left_bumper", 64),
    ("right_bumper", 128),
    ("select", 1024),
    ("start", 2048),
    ("mode", 4096),
    ("left_thumb", 8192),
    ("right_thumb", 16384),
];

pub fn button_name(key: &str) -> Option<&'static str> {
    KEYS.iter()
        .map(|(key, _)| *key)
        .chain([
            "dpad_up",
            "dpad_down",
            "dpad_left",
            "dpad_right",
            "left_trigger",
            "right_trigger",
        ])
        .find(|name| *name == key)
}

pub fn report(state: &GamepadVisualState, deadzone: f32) -> [u8; 15] {
    let mut data = [0; 15];
    for i in 0..4 {
        let value = state.sticks[i / 2][i % 2];
        let value = if !value.is_finite() || value.abs() < deadzone {
            0.0
        } else {
            value.clamp(-1.0, 1.0)
        };
        let value = if i % 2 == 1 { -value } else { value };
        let axis = ((value + 1.0) * 32767.5).round() as u16;
        data[i * 2..i * 2 + 2].copy_from_slice(&axis.to_le_bytes());
    }
    for (i, key) in ["left_trigger", "right_trigger"].into_iter().enumerate() {
        let value = if state.buttons.contains(key) {
            1.0
        } else {
            state.triggers[i]
        };
        let trigger = (value.clamp(0.0, 1.0) * 32767.0).round() as u16;
        data[8 + i * 2..10 + i * 2].copy_from_slice(&trigger.to_le_bytes());
    }
    let mut bits = KEYS
        .iter()
        .filter(|(key, _)| state.buttons.contains(key))
        .fold(0, |bits, (_, bit)| bits | bit);
    // Standard button usages 9/10 become BTN_TL2/BTN_TR2 in Linux/Android.
    // Pedal axes retain analog travel for games using MotionEvent triggers.
    for (i, bit) in [(0, 0x0100), (1, 0x0200)] {
        if u16::from_le_bytes([data[8 + i * 2], data[9 + i * 2]]) >= 16384 {
            bits |= bit;
        }
    }
    data[12..14].copy_from_slice(&bits.to_le_bytes());
    let x = i8::from(state.buttons.contains("dpad_right"))
        - i8::from(state.buttons.contains("dpad_left"));
    let y =
        i8::from(state.buttons.contains("dpad_down")) - i8::from(state.buttons.contains("dpad_up"));
    data[14] = match (x, y) {
        (0, -1) => 1,
        (1, -1) => 2,
        (1, 0) => 3,
        (1, 1) => 4,
        (0, 1) => 5,
        (-1, 1) => 6,
        (-1, 0) => 7,
        (-1, -1) => 8,
        _ => 0,
    };
    data
}

fn create_message(name: &str) -> Vec<u8> {
    // Use Generic.kl: the Xbox-specific layout omits BUTTON_L2/BUTTON_R2.
    let mut data = vec![12, 0, 1, 0, 0, 0, 0, name.len() as u8];
    data.extend_from_slice(name.as_bytes());
    data.extend_from_slice(&(DESCRIPTOR.len() as u16).to_be_bytes());
    data.extend_from_slice(DESCRIPTOR);
    data
}

fn input_message(report: [u8; 15]) -> Vec<u8> {
    let mut data = vec![13, 0, 1, 0, 15];
    data.extend_from_slice(&report);
    data
}

struct Session {
    adb: Adb,
    serial: String,
    port: Option<String>,
    child: Option<Child>,
    socket: Option<TcpStream>,
}

impl Session {
    fn start(
        adb: PathBuf,
        serial: String,
        generation: u64,
        current: &AtomicU64,
    ) -> Result<Self, String> {
        let executable = crate::mirror::detect_scrcpy().ok_or("未找到内置 scrcpy")?;
        let server = executable
            .parent()
            .unwrap_or(Path::new("."))
            .join("scrcpy-server");
        let mut session = Self {
            adb: Adb::new(adb),
            serial,
            port: None,
            child: None,
            socket: None,
        };
        let remote = format!(
            "/data/local/tmp/androidtool-gamepad-{}.jar",
            std::process::id()
        );
        session.adb.push(&session.serial, &server, &remote)?;
        let scid = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            ^ std::process::id())
            & 0x7fff_ffff;
        let name = format!("{NAME} {scid:08x}");
        let socket_name = format!("localabstract:scrcpy_{scid:08x}");
        let output = util::run(
            &session.adb.path,
            &["-s", &session.serial, "forward", "tcp:0", &socket_name],
        )?;
        if !output.status.success() {
            return Err(util::output_text(&output));
        }
        let port = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        port.parse::<u16>()
            .map_err(|_| "ADB 未返回有效端口".to_owned())?;
        session.port = Some(format!("tcp:{port}"));
        let mut command = util::command(&session.adb.path);
        session.child = Some(
            command
                .args([
                    "-s",
                    &session.serial,
                    "shell",
                    &format!("CLASSPATH={remote}"),
                    "app_process",
                    "/",
                    "com.genymobile.scrcpy.Server",
                    "5.0.1",
                    &format!("scid={scid:08x}"),
                    "video=false",
                    "audio=false",
                    "control=true",
                    "tunnel_forward=true",
                    "send_device_meta=false",
                    "send_dummy_byte=true",
                    "clipboard_autosync=false",
                    "power_on=false",
                    "cleanup=true",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?,
        );
        let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let started = Instant::now();
        loop {
            if current.load(Ordering::SeqCst) != generation {
                return Err("连接已取消".to_owned());
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err("UHID 服务连接超时".to_owned());
            }
            if session
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("scrcpy UHID 服务启动失败".to_owned());
            }
            if let Ok(mut socket) = TcpStream::connect_timeout(&address, Duration::from_millis(200))
            {
                socket
                    .set_read_timeout(Some(Duration::from_millis(300)))
                    .map_err(|e| e.to_string())?;
                let mut dummy = [1];
                if socket.read_exact(&mut dummy).is_ok() && dummy == [0] {
                    socket.set_nodelay(true).map_err(|e| e.to_string())?;
                    socket
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .map_err(|e| e.to_string())?;
                    session.socket = Some(socket);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        let mut incoming = session
            .socket
            .as_ref()
            .unwrap()
            .try_clone()
            .map_err(|e| e.to_string())?;
        // Drain device messages (e.g. UHID output) without blocking input writes.
        thread::spawn(move || {
            let mut data = [0; 1024];
            loop {
                match incoming.read(&mut data) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => break,
                }
            }
        });
        session
            .socket
            .as_mut()
            .unwrap()
            .write_all(&create_message(&name))
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        loop {
            if current.load(Ordering::SeqCst) != generation {
                return Err("连接已取消".to_owned());
            }
            let input = session.adb.shell(&session.serial, &["dumpsys", "input"])?;
            if input.contains(&name) {
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                return Err(
                    "设备未创建 UHID 手柄，请检查内核 /dev/uhid 支持和权限（不会回退触屏）"
                        .to_owned(),
                );
            }
            thread::sleep(Duration::from_millis(200));
        }
        session.write(report(&GamepadVisualState::default(), 0.0))?;
        Ok(session)
    }

    fn write(&mut self, state: [u8; 15]) -> Result<(), String> {
        if self
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("UHID 服务已退出".to_owned());
        }
        self.socket
            .as_mut()
            .unwrap()
            .write_all(&input_message(state))
            .map_err(|e| e.to_string())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.write_all(&input_message(report(&GamepadVisualState::default(), 0.0)));
            let _ = socket.write_all(&[14, 0, 1]);
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(port) = self.port.take() {
            let _ = util::run(
                &self.adb.path,
                &["-s", &self.serial, "forward", "--remove", &port],
            );
        }
    }
}

pub struct Status {
    pub message: String,
    pub failed: bool,
    generation: u64,
}
enum Command {
    Connect(PathBuf, String, u64),
    Disconnect,
    Input(u64, [u8; 15]),
}
pub struct GamepadTransport {
    sender: mpsc::Sender<Command>,
    receiver: mpsc::Receiver<Status>,
    latest: Mutex<[u8; 15]>,
    generation: Arc<AtomicU64>,
}

impl GamepadTransport {
    pub fn new() -> Self {
        let (sender, commands) = mpsc::channel();
        let (statuses, receiver) = mpsc::channel();
        let latest = Mutex::new(report(&GamepadVisualState::default(), 0.0));
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        thread::spawn(move || {
            let mut session: Option<Session> = None;
            let mut active_generation = 0;
            let mut last_check = Instant::now();
            let mut last_report = report(&GamepadVisualState::default(), 0.0);
            loop {
                match commands.recv_timeout(Duration::from_millis(100)) {
                    Ok(Command::Connect(adb, serial, id)) => {
                        session = None;
                        if current.load(Ordering::SeqCst) != id {
                            continue;
                        }
                        match Session::start(adb, serial, id, &current) {
                            Ok(value) => {
                                if current.load(Ordering::SeqCst) != id {
                                    continue;
                                }
                                session = Some(value);
                                active_generation = id;
                                last_report = report(&GamepadVisualState::default(), 0.0);
                                let _ = statuses.send(Status {
                                    message: "UHID 已连接 · AndroidTool Gamepad".to_owned(),
                                    failed: false,
                                    generation: id,
                                });
                            }
                            Err(error) => {
                                if current.load(Ordering::SeqCst) == id {
                                    let _ = statuses.send(Status {
                                        message: error,
                                        failed: true,
                                        generation: id,
                                    });
                                }
                            }
                        }
                    }
                    Ok(Command::Disconnect) => {
                        session = None;
                    }
                    Ok(Command::Input(id, report)) => {
                        if id == active_generation && id == current.load(Ordering::SeqCst) {
                            if let Some(active) = session.as_mut() {
                                last_report = report;
                                if let Err(error) = active.write(report) {
                                    session = None;
                                    let _ = statuses.send(Status {
                                        message: format!("UHID 连接中断：{error}"),
                                        failed: true,
                                        generation: id,
                                    });
                                }
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if active_generation != current.load(Ordering::SeqCst) {
                    session = None;
                }
                if last_check.elapsed() >= Duration::from_secs(1) {
                    last_check = Instant::now();
                    if active_generation != current.load(Ordering::SeqCst) {
                        session = None;
                    }
                    if let Some(active) = session.as_mut() {
                        if let Err(error) = active.write(last_report) {
                            session = None;
                            let _ = statuses.send(Status {
                                message: format!("UHID 连接中断：{error}"),
                                failed: true,
                                generation: active_generation,
                            });
                        }
                    }
                }
            }
        });
        Self {
            sender,
            receiver,
            latest,
            generation,
        }
    }
    pub fn connect(&self, adb: PathBuf, serial: String) {
        *self.latest.lock().unwrap() = report(&GamepadVisualState::default(), 0.0);
        let id = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        while self.receiver.try_recv().is_ok() {}
        let _ = self.sender.send(Command::Connect(adb, serial, id));
    }
    pub fn disconnect(&self) {
        *self.latest.lock().unwrap() = report(&GamepadVisualState::default(), 0.0);
        self.generation.fetch_add(1, Ordering::SeqCst);
        while self.receiver.try_recv().is_ok() {}
        let _ = self.sender.send(Command::Disconnect);
    }
    pub fn send(&self, report: [u8; 15]) {
        let mut latest = self.latest.lock().unwrap();
        if *latest != report {
            *latest = report;
            let _ = self.sender.send(Command::Input(
                self.generation.load(Ordering::SeqCst),
                report,
            ));
        }
    }
    pub fn poll(&self) -> Option<Status> {
        while let Ok(status) = self.receiver.try_recv() {
            if status.generation == self.generation.load(Ordering::SeqCst) {
                return Some(status);
            }
        }
        None
    }
}

impl Drop for GamepadTransport {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn east_button_and_dpad_are_independent() {
        let mut state = GamepadVisualState::default();
        state.buttons.insert("east");
        let b = report(&state, 0.16);
        assert_eq!(&b[12..15], &[2, 0, 0]);
        state.buttons.insert("dpad_down");
        assert_eq!(&report(&state, 0.16)[12..15], &[2, 0, 5]);
        state.buttons.clear();
        assert_eq!(&report(&state, 0.16)[12..15], &[0, 0, 0]);
    }

    #[test]
    fn protocol_and_axes_match_scrcpy() {
        let mut state = GamepadVisualState::default();
        state.sticks[0] = [1.0, 1.0];
        state.triggers[0] = 1.0;
        let data = report(&state, 0.16);
        assert_eq!(&data[0..4], &[255, 255, 0, 0]);
        assert_eq!(&data[8..10], &[255, 127]);
        assert_eq!(&input_message(data)[..5], &[13, 0, 1, 0, 15]);
        let create = create_message(NAME);
        assert_eq!(&create[..8], &[12, 0, 1, 0, 0, 0, 0, NAME.len() as u8]);
    }

    #[test]
    fn both_triggers_have_independent_analog_values_and_release_to_zero() {
        let mut state = GamepadVisualState::default();
        state.triggers = [0.5, 1.0];
        let data = report(&state, 0.16);
        assert_eq!(&data[8..12], &[0, 64, 255, 127]);
        assert_eq!(&data[12..15], &[0, 3, 0]);
        state.triggers = [0.0, 0.0];
        for (key, expected) in [
            ("left_trigger", [255, 127, 0, 0]),
            ("right_trigger", [0, 0, 255, 127]),
        ] {
            state.buttons.insert(key);
            let data = report(&state, 0.16);
            assert_eq!(&data[8..12], &expected);
            assert_eq!(data[13], if key == "left_trigger" { 1 } else { 2 });
            state.buttons.clear();
            assert_eq!(&report(&state, 0.16)[8..12], &[0, 0, 0, 0]);
        }
    }

    #[test]
    #[ignore]
    fn live_neutral_gamepad_registration() {
        let serial = std::env::var("ANDROIDTOOL_TEST_SERIAL").expect("set ANDROIDTOOL_TEST_SERIAL");
        let adb = std::env::current_dir()
            .unwrap()
            .join("vendor/windows/platform-tools/adb.exe");
        let current = AtomicU64::new(1);
        let session = Session::start(adb, serial, 1, &current).expect("UHID registration failed");
        let input = session
            .adb
            .shell(&session.serial, &["dumpsys", "input"])
            .unwrap();
        let block = input
            .split("\n  Device ")
            .find(|block| {
                block.contains(NAME) && block.contains("LTRIGGER:") && block.contains("BRAKE:")
            })
            .expect("native trigger axes missing");
        assert!(block.contains("RTRIGGER:"));
        assert!(block.contains("GAS:"));
        let event = input
            .lines()
            .skip_while(|line| !line.contains(NAME))
            .take(25)
            .find_map(|line| line.trim().strip_prefix("Path: "))
            .expect("gamepad input event path missing");
        let capabilities = session
            .adb
            .shell(&session.serial, &["getevent", "-lp", event])
            .unwrap();
        assert!(capabilities.contains("BTN_TL2"), "{capabilities}");
        assert!(capabilities.contains("BTN_TR2"), "{capabilities}");
        println!("{block}\n{capabilities}");
        drop(session);
    }
}
