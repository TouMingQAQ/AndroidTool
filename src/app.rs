use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Layout, Margin, Order,
    PointerButton, ProgressBar, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, TextureHandle,
    TextureOptions, Ui, Vec2,
};

use crate::adb::{Adb, DeviceInfo};
use crate::config::AppConfig;
use crate::device::{DeviceSnapshot, DeviceWatcher};
use crate::gamepad::{GamepadHub, GamepadKind, GamepadVisualState};
use crate::installer::{
    InstallEvent, InstallRequest, InstallSession, PackageKind, converted_output_path,
};
use crate::logcat::{
    LogEntry, LogLevel, LogcatEvent, LogcatSession, export_file, import_file,
    process_matches_package,
};
use crate::mirror::{MirrorOptions, MirrorSession};
use crate::screen::{ScreenEvent, ScreenSession};
use crate::theme;
use crate::tools::{ToolEvent, ToolKind, ToolSession, ToolVersion};
use crate::util;
use crate::wireless::{ManualWirelessMode, QrMatrix, QrPairing, WirelessEvent, WirelessSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Page {
    Overview,
    Screen,
    Logcat,
    Installer,
    Gamepad,
    Files,
    Settings,
}

impl Page {
    const ALL: [Self; 7] = [
        Self::Overview,
        Self::Screen,
        Self::Logcat,
        Self::Installer,
        Self::Gamepad,
        Self::Files,
        Self::Settings,
    ];

    fn icon(self) -> Icon {
        match self {
            Self::Overview => Icon::Overview,
            Self::Screen => Icon::Screen,
            Self::Logcat => Icon::Terminal,
            Self::Installer => Icon::Package,
            Self::Gamepad => Icon::Gamepad,
            Self::Files => Icon::Folder,
            Self::Settings => Icon::Sliders,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "设备总览",
            Self::Screen => "实时投屏",
            Self::Logcat => "Logcat",
            Self::Installer => "统一安装器",
            Self::Gamepad => "手柄控制",
            Self::Files => "文件工作区",
            Self::Settings => "设置",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Overview => "管理所有 ADB 设备与常用操作",
            Self::Screen => "查看画面并直接发送触摸与按键事件",
            Self::Logcat => "实时日志、Android Studio 格式导入导出",
            Self::Installer => "自动识别 APK / AAB / APKS",
            Self::Gamepad => "Xbox · PlayStation · Nintendo · 虚拟手柄",
            Self::Files => "拖拽上传、浏览与下载设备文件",
            Self::Settings => "工具路径、流媒体与便携配置",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Icon {
    Overview,
    Screen,
    Terminal,
    Package,
    Gamepad,
    Folder,
    Sliders,
    Device,
    Warning,
    Link,
    Activity,
    Code,
}

#[derive(Debug)]
struct Toast {
    text: String,
    success: bool,
    created: Instant,
}

#[derive(Debug)]
enum AdbTask {
    Shell(Vec<String>),
    Push { local: PathBuf, remote: String },
    Pull { remote: String, local: PathBuf },
    Screenshot(PathBuf),
}

#[derive(Debug)]
struct AdbJob {
    adb_path: PathBuf,
    serial: String,
    task: AdbTask,
    label: String,
}

#[derive(Debug)]
struct AdbResult {
    label: String,
    serial: String,
    result: Result<String, String>,
}

struct AdbWorker {
    sender: Sender<AdbJob>,
    receiver: Receiver<AdbResult>,
}

impl AdbWorker {
    fn new() -> Self {
        let (sender, job_receiver) = mpsc::channel::<AdbJob>();
        let (result_sender, receiver) = mpsc::channel::<AdbResult>();
        thread::spawn(move || {
            while let Ok(job) = job_receiver.recv() {
                let adb = Adb::new(&job.adb_path);
                let result = match job.task {
                    AdbTask::Shell(args) => {
                        let args: Vec<&str> = args.iter().map(String::as_str).collect();
                        adb.shell(&job.serial, &args)
                    }
                    AdbTask::Push { local, remote } => adb.push(&job.serial, &local, &remote),
                    AdbTask::Pull { remote, local } => adb.pull(&job.serial, &remote, &local),
                    AdbTask::Screenshot(path) => adb
                        .screenshot(&job.serial)
                        .and_then(|bytes| {
                            std::fs::write(&path, bytes)
                                .map_err(|error| format!("无法保存截图: {error}"))
                        })
                        .map(|_| format!("截图已保存：{}", path.display())),
                };
                if result_sender
                    .send(AdbResult {
                        label: job.label,
                        serial: job.serial,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { sender, receiver }
    }

    fn submit(&self, job: AdbJob) {
        let _ = self.sender.send(job);
    }

    fn poll(&self) -> Option<AdbResult> {
        self.receiver.try_recv().ok()
    }
}

struct ScreenState {
    mirror: Option<MirrorSession>,
    mirror_requested: bool,
    mirror_status: String,
    mirror_rect: Option<Rect>,
    gamepad_overlay_open: bool,
    canvas_rect: Option<Rect>,
    gamepad_overlay_position: Option<egui::Pos2>,
    gamepad_drag_offset: Option<Vec2>,
    session: Option<ScreenSession>,
    texture: Option<TextureHandle>,
    texture_size: Option<[usize; 2]>,
    device_size: Option<[usize; 2]>,
    last_frame: Option<Instant>,
    measured_fps: f32,
    error: Option<String>,
    text_input: String,
    pointer_start: Option<egui::Pos2>,
}

impl Default for ScreenState {
    fn default() -> Self {
        Self {
            mirror: None,
            mirror_requested: true,
            mirror_status: String::new(),
            mirror_rect: None,
            gamepad_overlay_open: false,
            canvas_rect: None,
            gamepad_overlay_position: None,
            gamepad_drag_offset: None,
            session: None,
            texture: None,
            texture_size: None,
            device_size: None,
            last_frame: None,
            measured_fps: 0.0,
            error: None,
            text_input: String::new(),
            pointer_start: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogPackageMode {
    Foreground,
    Package,
    All,
}

struct LogState {
    session: Option<LogcatSession>,
    entries: VecDeque<LogEntry>,
    min_level: LogLevel,
    search: String,
    paused: bool,
    filter_cache: Vec<usize>,
    filter_dirty: bool,
    error: Option<String>,
    package_mode: LogPackageMode,
    package_input: String,
    foreground: Option<String>,
    process_names: HashMap<String, String>,
    process_error: Option<String>,
    scanned_foreground: BTreeSet<String>,
    package_search: String,
}

impl Default for LogState {
    fn default() -> Self {
        Self {
            session: None,
            entries: VecDeque::new(),
            min_level: LogLevel::Verbose,
            search: String::new(),
            paused: false,
            filter_cache: Vec::new(),
            filter_dirty: true,
            error: None,
            package_mode: LogPackageMode::Foreground,
            package_input: String::new(),
            foreground: None,
            process_names: HashMap::new(),
            process_error: None,
            scanned_foreground: BTreeSet::new(),
            package_search: String::new(),
        }
    }
}

impl LogState {
    fn application_choices(&self) -> Vec<String> {
        let mut packages = self.scanned_foreground.clone();
        for process in self.process_names.values() {
            let package = process.split(':').next().unwrap_or(process);
            if package.contains('.')
                && package
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_'))
            {
                packages.insert(package.to_owned());
            }
        }
        if let Some(package) = &self.foreground {
            packages.insert(package.clone());
        }
        if self.package_mode == LogPackageMode::Package && !self.package_input.is_empty() {
            packages.insert(self.package_input.clone());
        }
        let mut choices: Vec<_> = packages.into_iter().collect();
        choices.sort_by_key(|package| (self.foreground.as_ref() != Some(package), package.clone()));
        choices
    }
}

#[derive(Debug, Clone)]
struct InstallQueueItem {
    path: PathBuf,
    kind: PackageKind,
    convert_only: bool,
}

#[derive(Default)]
struct InstallerState {
    queue: VecDeque<InstallQueueItem>,
    session: Option<InstallSession>,
    active_name: Option<String>,
    active_convert_only: bool,
    progress: f32,
    stage: String,
    logs: Vec<String>,
    last_success: bool,
    converted: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct RemoteEntry {
    name: String,
    is_dir: bool,
    size: u64,
    permission: String,
    modified: String,
}

struct FilesState {
    remote_path: String,
    entries: Vec<RemoteEntry>,
    local_files: Vec<PathBuf>,
    selected_remote: Option<usize>,
    status: String,
    error: Option<String>,
    loading: bool,
}

impl Default for FilesState {
    fn default() -> Self {
        Self {
            remote_path: "/sdcard/Download".to_owned(),
            entries: Vec::new(),
            local_files: Vec::new(),
            selected_remote: None,
            status: String::new(),
            error: None,
            loading: false,
        }
    }
}

struct GamepadState {
    hub: GamepadHub,
    selected_index: Option<usize>,
    show_virtual: bool,
    virtual_kind: Option<GamepadKind>,
    virtual_state: GamepadVisualState,
    transport: crate::gamepad_hid::GamepadTransport,
    target: Option<String>,
    enabled: bool,
    last_action: String,
}

#[derive(Default)]
struct ToolTask {
    session: Option<ToolSession>,
    status: String,
    progress: Option<f32>,
    versions: Vec<ToolVersion>,
    selected: Option<usize>,
}

struct ToolState {
    tasks: [ToolTask; 3],
    java_major: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WirelessTab {
    Ip,
    Qr,
}

struct WirelessState {
    open: bool,
    tab: WirelessTab,
    host: String,
    pair_port: String,
    connect_port: String,
    pairing_code: String,
    qr: Option<QrPairing>,
    session: Option<WirelessSession>,
    busy: bool,
    status: String,
    error: Option<String>,
    confirm_adb_restart: bool,
}

impl Default for WirelessState {
    fn default() -> Self {
        Self {
            open: false,
            tab: WirelessTab::Ip,
            host: String::new(),
            pair_port: String::new(),
            connect_port: String::new(),
            pairing_code: String::new(),
            qr: None,
            session: None,
            busy: false,
            status: "输入配对弹窗中的地址、配对端口和 6 位配对码；连接端口留空可自动发现。"
                .to_owned(),
            error: None,
            confirm_adb_restart: false,
        }
    }
}

pub struct AndroidToolApp {
    #[cfg(windows)]
    native_window: Option<isize>,
    page: Page,
    page_changed_at: Instant,
    config: AppConfig,
    config_dirty: bool,
    config_edit_at: Instant,
    config_status: String,
    watcher: Option<DeviceWatcher>,
    watcher_path: Option<PathBuf>,
    devices: Vec<DeviceInfo>,
    active_serial: Option<String>,
    device_error: Option<String>,
    last_device_refresh: Instant,
    adb_worker: AdbWorker,
    input_worker: AdbWorker,
    screen: ScreenState,
    log: LogState,
    installer: InstallerState,
    files: FilesState,
    gamepad: GamepadState,
    tools: ToolState,
    wireless: WirelessState,
    toasts: Vec<Toast>,
    last_adb_error: Option<(String, Instant)>,
}

impl AndroidToolApp {
    pub fn new(cc: &eframe::CreationContext<'_>, config: AppConfig) -> Self {
        let mut app = Self::from_config(config, true);
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = cc.window_handle() {
                if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                    app.native_window = Some(handle.hwnd.get());
                }
            }
        }
        app
    }

    fn from_config(mut config: AppConfig, connect_devices: bool) -> Self {
        let mut auto_detected = false;
        if config.bundletool_path.trim().is_empty() {
            if let Some(path) = util::detect_bundletool("") {
                config.bundletool_path = util::portable_path(&path);
                auto_detected = true;
            }
        }
        if config.java_path.trim().is_empty() {
            if let Some(path) = util::detect_java("") {
                config.java_path = util::portable_path(&path);
                auto_detected = true;
            }
        }

        let java_major = if [17, 21, 25].contains(&config.java_major) {
            config.java_major
        } else {
            17
        };
        let mut app = Self {
            #[cfg(windows)]
            native_window: None,
            page: Page::Overview,
            page_changed_at: Instant::now(),
            config,
            config_dirty: auto_detected,
            config_edit_at: Instant::now(),
            config_status: format!("配置：{}", util::settings_path().display()),
            watcher: None,
            watcher_path: None,
            devices: Vec::new(),
            active_serial: None,
            device_error: None,
            last_device_refresh: Instant::now(),
            adb_worker: AdbWorker::new(),
            input_worker: AdbWorker::new(),
            screen: ScreenState::default(),
            log: LogState::default(),
            installer: InstallerState::default(),
            files: FilesState::default(),
            gamepad: GamepadState {
                hub: GamepadHub::new(),
                selected_index: None,
                show_virtual: true,
                virtual_kind: None,
                virtual_state: GamepadVisualState::default(),
                transport: crate::gamepad_hid::GamepadTransport::new(),
                target: None,
                enabled: false,
                last_action: "未连接原生手柄".to_owned(),
            },
            tools: ToolState {
                tasks: std::array::from_fn(|_| ToolTask::default()),
                java_major,
            },
            wireless: WirelessState::default(),
            toasts: Vec::new(),
            last_adb_error: None,
        };
        if connect_devices {
            app.ensure_watcher();
            app.refresh_devices_now();
        }
        app
    }

    fn set_page(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.page_changed_at = Instant::now();
            if page == Page::Files && self.active_serial_online().is_some() {
                self.load_remote_dir(self.files.remote_path.clone());
            }
        }
    }

    fn toast(&mut self, text: impl Into<String>, success: bool) {
        self.toasts.push(Toast {
            text: text.into(),
            success,
            created: Instant::now(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn mark_config_dirty(&mut self) {
        self.config_dirty = true;
        self.config_edit_at = Instant::now();
    }

    fn save_config(&mut self) {
        match self.config.save() {
            Ok(()) => {
                self.config_dirty = false;
                self.config_status = format!("已保存 · {}", util::settings_path().display());
            }
            Err(error) => {
                self.config_status = error.clone();
                self.toast(error, false);
            }
        }
    }

    fn open_wireless_pairing(&mut self, tab: WirelessTab) {
        if self.wireless.busy && self.wireless.tab != tab {
            self.stop_wireless_pairing();
        }
        self.wireless.open = true;
        self.wireless.tab = tab;
        self.wireless.error = None;
        if tab == WirelessTab::Qr {
            self.start_qr_wireless();
        }
    }

    fn close_wireless_pairing(&mut self) {
        if let Some(session) = self.wireless.session.take() {
            session.stop();
        }
        self.wireless.open = false;
        self.wireless.busy = false;
        self.wireless.qr = None;
    }

    fn ensure_qr_pairing(&mut self) {
        if self.wireless.qr.is_none() {
            self.wireless.qr = Some(QrPairing::generate());
        }
    }

    fn regenerate_qr_pairing(&mut self) {
        if let Some(session) = self.wireless.session.take() {
            session.stop();
        }
        self.wireless.qr = Some(QrPairing::generate());
        self.wireless.busy = false;
        self.wireless.error = None;
        self.start_qr_wireless();
    }

    fn stop_wireless_pairing(&mut self) {
        if let Some(session) = self.wireless.session.take() {
            session.stop();
        }
        self.wireless.busy = false;
        self.wireless.error = None;
        self.wireless.status = "已停止本次无线配对。".to_owned();
    }

    fn start_manual_wireless(&mut self, mode: ManualWirelessMode) {
        if self.wireless.busy {
            return;
        }
        let Some(adb_path) = self.config.resolved_adb() else {
            self.wireless.error = Some("未找到 ADB。请先在设置中安装或选择 ADB。".to_owned());
            return;
        };
        let code = self.wireless.pairing_code.trim().to_owned();
        match WirelessSession::start_manual(
            adb_path,
            self.wireless.host.clone(),
            self.wireless.pair_port.clone(),
            self.wireless.connect_port.clone(),
            code,
            mode,
        ) {
            Ok(session) => {
                self.wireless.session = Some(session);
                self.wireless.busy = true;
                self.wireless.error = None;
                self.wireless.status = match mode {
                    ManualWirelessMode::PairOnly => "正在提交配对信息…".to_owned(),
                    ManualWirelessMode::ConnectOnly => "正在连接无线 ADB…".to_owned(),
                    ManualWirelessMode::PairAndConnect => "正在配对并连接设备…".to_owned(),
                };
            }
            Err(error) => {
                self.wireless.error = Some(error);
            }
        }
    }

    fn start_qr_wireless(&mut self) {
        if self.wireless.busy {
            return;
        }
        let Some(adb_path) = self.config.resolved_adb() else {
            self.wireless.error = Some("未找到 ADB。请先在设置中安装或选择 ADB。".to_owned());
            return;
        };
        self.ensure_qr_pairing();
        let Some(pairing) = self.wireless.qr.clone() else {
            return;
        };
        self.wireless.session = Some(WirelessSession::start_qr(adb_path, pairing));
        self.wireless.busy = true;
        self.wireless.error = None;
        self.wireless.status = "等待手机扫描二维码…".to_owned();
    }

    fn poll_wireless(&mut self) {
        let mut events = Vec::new();
        if let Some(session) = self.wireless.session.as_ref() {
            while let Some(event) = session.try_recv() {
                events.push(event);
            }
        }
        let mut refresh_devices = false;
        let mut finished = false;
        for event in events {
            match event {
                WirelessEvent::Progress(message) => {
                    self.wireless.status = message;
                }
                WirelessEvent::Finished { success, message } => {
                    self.wireless.busy = false;
                    self.wireless.status = message.clone();
                    if success {
                        self.wireless.error = None;
                        refresh_devices = true;
                    } else {
                        self.wireless.error = Some(message.clone());
                    }
                    self.toast(message, success);
                    finished = true;
                }
            }
        }
        if finished {
            self.wireless.session = None;
        }
        if refresh_devices {
            self.refresh_devices_now();
        }
    }

    fn ensure_watcher(&mut self) {
        let adb_path = self.config.resolved_adb();
        if adb_path == self.watcher_path {
            return;
        }
        self.watcher = None;
        self.watcher_path = None;
        self.devices.clear();
        self.active_serial = None;
        self.screen.texture = None;
        self.screen.mirror = None;
        self.screen.mirror_requested = true;
        self.screen.session = None;
        self.screen.last_frame = None;
        self.screen.measured_fps = 0.0;
        self.log.session = None;

        if let Some(path) = adb_path {
            self.watcher = Some(DeviceWatcher::start(path.clone()));
            self.watcher_path = Some(path);
        } else {
            self.device_error =
                Some("未找到 adb。请连接 Android SDK 或安装 platform-tools。".to_owned());
        }
    }

    fn refresh_devices_now(&mut self) {
        let Some(adb_path) = self.config.resolved_adb() else {
            self.device_error = Some("未找到 adb".to_owned());
            return;
        };
        let adb = Adb::new(adb_path);
        match adb.devices() {
            Ok(devices) => {
                self.devices = devices;
                self.device_error = None;
                self.last_device_refresh = Instant::now();
                self.reconcile_active_device();
            }
            Err(error) => {
                self.device_error = Some(error.clone());
                self.toast(format!("设备刷新失败：{error}"), false);
            }
        }
    }

    fn reconcile_active_device(&mut self) {
        let active_valid = self
            .active_serial
            .as_ref()
            .is_some_and(|serial| self.devices.iter().any(|device| &device.serial == serial));
        if !active_valid {
            let next = self
                .devices
                .iter()
                .find(|device| device.is_online())
                .or_else(|| self.devices.first())
                .map(|device| device.serial.clone());
            self.set_active_device(next);
        }
    }

    fn set_active_device(&mut self, serial: Option<String>) {
        if self.active_serial == serial {
            return;
        }
        self.active_serial = serial;
        self.screen.mirror = None;
        self.screen.mirror_requested = true;
        self.screen.texture = None;
        self.screen.texture_size = None;
        self.screen.device_size = None;
        self.screen.error = None;
        self.screen.session = None;
        self.log.session = None;
        self.log.entries.clear();
        self.log.process_names.clear();
        self.log.scanned_foreground.clear();
        self.log.foreground = None;
        self.log.process_error = None;
        self.log.filter_dirty = true;
        self.files.entries.clear();
        self.files.selected_remote = None;
        self.files.loading = false;
        self.disconnect_gamepad();
        self.queue_shell(vec!["wm".to_owned(), "size".to_owned()], "SIZE".to_owned());
        if self.page == Page::Files && self.active_serial_online().is_some() {
            self.load_remote_dir(self.files.remote_path.clone());
        }
    }

    fn active_device(&self) -> Option<&DeviceInfo> {
        let serial = self.active_serial.as_ref()?;
        self.devices.iter().find(|device| &device.serial == serial)
    }

    fn active_serial_online(&self) -> Option<String> {
        self.active_device()
            .filter(|device| device.is_online())
            .map(|device| device.serial.clone())
    }

    fn adb_path(&self) -> Option<PathBuf> {
        self.config.resolved_adb()
    }

    fn queue_shell(&self, args: Vec<String>, label: String) {
        let (Some(adb_path), Some(serial)) = (self.adb_path(), self.active_serial_online()) else {
            return;
        };
        self.adb_worker.submit(AdbJob {
            adb_path,
            serial,
            task: AdbTask::Shell(args),
            label,
        });
    }

    fn queue_input_shell(&self, args: Vec<String>, label: String) -> bool {
        let (Some(adb_path), Some(serial)) = (self.adb_path(), self.active_serial_online()) else {
            return false;
        };
        self.input_worker.submit(AdbJob {
            adb_path,
            serial,
            task: AdbTask::Shell(args),
            label,
        });
        true
    }

    fn queue_key(&mut self, code: i32) {
        self.queue_input_shell(
            vec!["input".to_owned(), "keyevent".to_owned(), code.to_string()],
            format!("KEY:{code}"),
        );
    }

    fn queue_tap_norm(&mut self, x: f32, y: f32, hold_ms: u32) {
        let [width, height] = self.screen.device_size.unwrap_or([1080, 2400]);
        let px = (x.clamp(0.0, 1.0) * width as f32) as i32;
        let py = (y.clamp(0.0, 1.0) * height as f32) as i32;
        if hold_ms <= 80 {
            self.queue_input_shell(
                vec![
                    "input".to_owned(),
                    "tap".to_owned(),
                    px.to_string(),
                    py.to_string(),
                ],
                "TAP".to_owned(),
            );
        } else {
            self.queue_input_shell(
                vec![
                    "input".to_owned(),
                    "swipe".to_owned(),
                    px.to_string(),
                    py.to_string(),
                    px.to_string(),
                    py.to_string(),
                    hold_ms.to_string(),
                ],
                "HOLD".to_owned(),
            );
        }
    }

    fn queue_swipe_px(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, duration: u32) {
        self.queue_input_shell(
            vec![
                "input".to_owned(),
                "swipe".to_owned(),
                x1.to_string(),
                y1.to_string(),
                x2.to_string(),
                y2.to_string(),
                duration.to_string(),
            ],
            "SWIPE".to_owned(),
        );
    }

    fn enqueue_package(&mut self, path: PathBuf, convert_only: bool) {
        let kind = PackageKind::detect(&path);
        if kind == PackageKind::Unknown {
            self.toast(format!("无法识别安装包：{}", util::file_name(&path)), false);
            return;
        }
        if convert_only && kind != PackageKind::Aab {
            self.toast("仅转换模式只支持 AAB", false);
            return;
        }
        self.installer.queue.push_back(InstallQueueItem {
            path: path.clone(),
            kind,
            convert_only,
        });
        self.toast(
            format!(
                "已加入安装队列：{} ({})",
                util::file_name(&path),
                kind.label()
            ),
            true,
        );
        self.set_page(Page::Installer);
    }

    fn start_next_install(&mut self) {
        if self.installer.session.is_some() {
            return;
        }
        let Some(item) = self.installer.queue.pop_front() else {
            return;
        };
        let adb_path = self.config.resolved_adb();
        if !item.convert_only && adb_path.is_none() {
            self.installer.queue.push_front(item);
            return;
        }
        let serial = if item.convert_only {
            String::new()
        } else {
            match self.active_serial_online() {
                Some(serial) => serial,
                None => {
                    self.installer.queue.push_front(item);
                    return;
                }
            }
        };
        let java_path = self
            .config
            .resolved_java()
            .unwrap_or_else(|| PathBuf::from("java"));
        let bundletool_path = self
            .config
            .resolved_bundletool()
            .unwrap_or_else(|| util::install_dir().join("tools").join("bundletool.jar"));
        let output_apk = item.convert_only.then(|| converted_output_path(&item.path));
        let request = InstallRequest {
            adb_path: adb_path.unwrap_or_default(),
            java_path,
            bundletool_path,
            serial,
            package: item.path.clone(),
            convert_only: item.convert_only,
            output_apk,
        };
        self.installer.active_name = Some(util::file_name(&item.path));
        self.installer.active_convert_only = item.convert_only;
        self.installer.progress = 0.0;
        self.installer.stage = "准备中".to_owned();
        self.installer.logs.clear();
        self.installer.converted = None;
        self.installer.session = Some(InstallSession::start(request));
    }

    fn poll_installer(&mut self) {
        let mut finished = false;
        let mut events = Vec::new();
        if let Some(session) = self.installer.session.as_ref() {
            while let Some(event) = session.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            match event {
                InstallEvent::Progress { progress, stage } => {
                    self.installer.progress = progress;
                    self.installer.stage = stage;
                }
                InstallEvent::Log(line) => {
                    self.installer.logs.push(line);
                    if self.installer.logs.len() > 200 {
                        self.installer.logs.remove(0);
                    }
                }
                InstallEvent::Finished {
                    success,
                    message,
                    converted,
                } => {
                    self.installer.last_success = success;
                    self.installer.progress = 1.0;
                    self.installer.stage = if success {
                        "完成".to_owned()
                    } else {
                        "失败".to_owned()
                    };
                    self.installer.logs.push(message.clone());
                    self.installer.converted = converted;
                    self.toast(message, success);
                    finished = true;
                }
            }
        }
        if finished {
            self.installer.session = None;
        }
    }

    fn ensure_streams(&mut self) {
        if self.page != Page::Screen
            || !self.config.scrcpy_enabled
            || self.active_serial_online().is_none()
        {
            self.screen.mirror = None;
            self.screen.mirror_requested = true;
        } else {
            if let Some(session) = self.screen.mirror.as_mut() {
                match session.poll() {
                    Ok(None) => self.screen.mirror_status = session.latest_status().to_owned(),
                    Ok(Some(message)) => {
                        self.screen.mirror_status = message;
                        self.screen.mirror = None;
                    }
                    Err(error) => {
                        self.screen.error = Some(error);
                        self.screen.mirror = None;
                    }
                }
            }
            if self.screen.mirror_requested {
                self.screen.mirror_requested = false;
                if let (Some(adb), Some(serial)) = (self.adb_path(), self.active_serial_online()) {
                    let options = MirrorOptions {
                        fps: self.config.mirror_fps,
                        max_size: self.config.mirror_max_size,
                        bit_rate_mbps: self.config.mirror_bit_rate_mbps,
                    };
                    self.screen.mirror = None;
                    self.screen.error = None;
                    match MirrorSession::start_with_mode(
                        &adb,
                        serial,
                        options,
                        self.config.mirror_embedded,
                    ) {
                        Ok(session) => {
                            self.screen.mirror = Some(session);
                            self.screen.mirror_status = "正在启动 scrcpy 视频流…".to_owned();
                        }
                        Err(error) => self.screen.error = Some(error),
                    }
                }
            }
        }
        // Do not compete with scrcpy for bandwidth by running the screenshot loop too.
        let need_screen = self.page == Page::Screen && !self.config.scrcpy_enabled;
        let serial = self.active_serial_online();
        if need_screen {
            if let (Some(path), Some(serial)) = (self.adb_path(), serial.as_ref()) {
                let restart = self.screen.session.as_ref().is_none_or(|session| {
                    &session.serial != serial || session.fps != self.config.screenshot_fps
                });
                if restart {
                    self.screen.texture = None;
                    self.screen.last_frame = None;
                    self.screen.measured_fps = 0.0;
                    self.screen.session = Some(ScreenSession::start(
                        path,
                        serial.clone(),
                        self.config.screenshot_fps.max(1),
                    ));
                    self.screen.error = None;
                }
            }
        } else {
            self.screen.session = None;
        }

        let need_log = self.page == Page::Logcat;
        if need_log {
            if let (Some(path), Some(serial)) = (self.adb_path(), serial.as_ref()) {
                let restart = self
                    .log
                    .session
                    .as_ref()
                    .is_none_or(|session| &session.serial != serial);
                if restart {
                    self.log.process_names.clear();
                    self.log.foreground = None;
                    self.log.filter_dirty = true;
                    self.log.session = Some(LogcatSession::start(path, serial.clone()));
                    self.log.error = None;
                }
            }
        } else {
            self.log.session = None;
        }
    }

    fn poll_devices(&mut self) {
        let Some(watcher) = self.watcher.as_ref() else {
            return;
        };
        if let Some(snapshot) = watcher.try_recv() {
            self.apply_device_snapshot(snapshot);
        }
    }

    fn apply_device_snapshot(&mut self, snapshot: DeviceSnapshot) {
        self.devices = snapshot.devices;
        self.device_error = snapshot.error;
        self.last_device_refresh = Instant::now();
        self.reconcile_active_device();
    }

    fn poll_screen(&mut self, ctx: &egui::Context) {
        let mut frames = Vec::new();
        if let Some(session) = self.screen.session.as_ref() {
            while let Some(event) = session.try_recv() {
                frames.push(event);
            }
        }
        for event in frames {
            match event {
                ScreenEvent::Frame {
                    width,
                    height,
                    screen_width,
                    screen_height,
                    rgba,
                } => {
                    if let Some(last) = self.screen.last_frame {
                        let delta = last.elapsed().as_secs_f32();
                        if delta > 0.001 && delta < 2.0 {
                            let fps = 1.0 / delta;
                            self.screen.measured_fps = if self.screen.measured_fps == 0.0 {
                                fps
                            } else {
                                self.screen.measured_fps * 0.8 + fps * 0.2
                            };
                        }
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                    self.screen.texture = Some(ctx.load_texture(
                        "device-screen-stream",
                        image,
                        TextureOptions::LINEAR,
                    ));
                    self.screen.texture_size = Some([width, height]);
                    self.screen.device_size = Some([screen_width, screen_height]);
                    self.screen.last_frame = Some(Instant::now());
                    self.screen.error = None;
                }
                ScreenEvent::Error(error) => {
                    self.screen.error = Some(error);
                }
            }
        }
    }

    fn poll_logcat(&mut self) {
        let mut events = Vec::new();
        if let Some(session) = self.log.session.as_ref() {
            while let Some(event) = session.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            match event {
                LogcatEvent::Batch(entries) => {
                    if !self.log.paused {
                        for mut entry in entries {
                            if entry.process.is_empty() {
                                entry.process = self
                                    .log
                                    .process_names
                                    .get(&entry.pid)
                                    .cloned()
                                    .unwrap_or_default();
                            }
                            self.log.entries.push_back(entry);
                        }
                        while self.log.entries.len() > self.config.max_log_lines {
                            self.log.entries.pop_front();
                        }
                        self.log.filter_dirty = true;
                    }
                }
                LogcatEvent::Error(error) => {
                    if self.log.error.as_deref() != Some(error.as_str()) {
                        self.log.error = Some(error.clone());
                        self.toast(format!("Logcat：{error}"), false);
                    }
                }
                LogcatEvent::Processes { foreground, names } => {
                    for entry in &mut self.log.entries {
                        if entry.process.is_empty() {
                            entry.process = names.get(&entry.pid).cloned().unwrap_or_default();
                        }
                    }
                    if let Some(package) = &foreground {
                        self.log.scanned_foreground.insert(package.clone());
                    }
                    self.log.foreground = foreground;
                    self.log.process_names = names;
                    self.log.process_error = None;
                    self.log.filter_dirty = true;
                }
                LogcatEvent::ProcessError(error) => {
                    self.log.foreground = None;
                    self.log.process_error = Some(error);
                    self.log.filter_dirty = true;
                }
            }
        }
    }

    fn poll_adb_results(&mut self) {
        let mut results = Vec::new();
        while let Some(result) = self.adb_worker.poll() {
            results.push(result);
        }
        while let Some(result) = self.input_worker.poll() {
            results.push(result);
        }
        for result in results {
            if result.label == "SIZE" {
                if let Ok(text) = &result.result {
                    if let Some(size) = parse_wm_size(text) {
                        self.screen.device_size = Some(size);
                    }
                }
                continue;
            }
            if let Some(path) = result.label.strip_prefix("LS:") {
                if path != self.files.remote_path
                    || self.active_serial.as_deref() != Some(&result.serial)
                {
                    continue;
                }
                self.files.loading = false;
                match result.result {
                    Ok(text) => {
                        self.files.entries = parse_remote_entries(&text);
                        self.files.error = None;
                        self.files.status = format!("已加载 {} 项", self.files.entries.len());
                        self.files.remote_path = path.to_owned();
                    }
                    Err(error) => {
                        self.files.error = Some(error.clone());
                        self.files.status = error;
                    }
                }
                continue;
            }
            match result.result {
                Ok(output) => {
                    if result.label.starts_with("PUSH") || result.label.starts_with("PULL") {
                        self.files.status = if output.is_empty() {
                            "文件传输完成".to_owned()
                        } else {
                            output.clone()
                        };
                        self.toast("文件传输完成", true);
                    }
                    if result.label == "SHOT" {
                        self.toast(output, true);
                    }
                }
                Err(error) => {
                    let now = Instant::now();
                    let duplicate = self.last_adb_error.as_ref().is_some_and(|(last, at)| {
                        last == &error && at.elapsed() < Duration::from_secs(2)
                    });
                    if !duplicate {
                        self.toast(format!("ADB：{error}"), false);
                        self.last_adb_error = Some((error, now));
                    }
                }
            }
        }
    }

    fn poll_tools(&mut self) {
        let mut events = Vec::new();
        for kind in ToolKind::ALL {
            let task = &mut self.tools.tasks[kind.index()];
            if let Some(session) = task.session.as_mut() {
                while let Some(event) = session.try_recv() {
                    events.push((kind, event));
                }
            }
        }
        for (kind, event) in events {
            let (event, fallback_message) = match event {
                ToolEvent::VersionsFallback { versions, message } => {
                    (ToolEvent::Versions(versions), Some(message))
                }
                event => (event, None),
            };
            let task = &mut self.tools.tasks[kind.index()];
            match event {
                ToolEvent::Progress { message, fraction } => {
                    task.status = message;
                    task.progress = fraction;
                }
                ToolEvent::Versions(versions) => {
                    let selected_name = match kind {
                        ToolKind::Bundletool => &self.config.bundletool_version,
                        ToolKind::Java => &self.config.java_version,
                        _ => &self.config.adb_path,
                    };
                    task.selected = versions
                        .iter()
                        .position(|v| &v.name == selected_name)
                        .or(Some(0));
                    task.status = fallback_message
                        .unwrap_or_else(|| format!("找到 {} 个版本", versions.len()));
                    task.progress = None;
                    task.versions = versions;
                    if let Some(version) = task.selected.and_then(|i| task.versions.get(i)) {
                        match kind {
                            ToolKind::Bundletool => {
                                self.config.bundletool_version = version.name.clone()
                            }
                            ToolKind::Java => self.config.java_version = version.name.clone(),
                            _ => {}
                        }
                        self.config_dirty = true;
                        self.config_edit_at = Instant::now();
                    }
                    task.session = None;
                }
                ToolEvent::Finished {
                    success,
                    message,
                    path,
                } => {
                    task.status = message.clone();
                    task.progress = None;
                    task.session = None;
                    if let Some(path) = path {
                        let saved_path = util::portable_path(&path);
                        match kind {
                            ToolKind::Adb => {
                                self.config.adb_path = saved_path;
                                self.watcher = None;
                                self.watcher_path = None;
                            }
                            ToolKind::Bundletool => self.config.bundletool_path = saved_path,
                            ToolKind::Java => self.config.java_path = saved_path,
                        }
                        self.mark_config_dirty();
                    }
                    self.toast(message, success);
                }
                ToolEvent::VersionsFallback { .. } => unreachable!(),
            }
        }
    }

    fn start_tool(&mut self, kind: ToolKind, install: bool) {
        let task = &mut self.tools.tasks[kind.index()];
        let version = task.selected.and_then(|i| task.versions.get(i)).cloned();
        if install && kind != ToolKind::Adb && version.is_none() {
            task.status = "请先获取并选择版本".to_owned();
            return;
        }
        let mut overwrite = false;
        if install {
            let placeholder = ToolVersion {
                name: "latest".into(),
                url: String::new(),
                checksum: None,
            };
            let target =
                crate::tools::download_directory(kind, version.as_ref().unwrap_or(&placeholder));
            if target.exists() {
                let result = rfd::MessageDialog::new()
                    .set_title("版本已存在")
                    .set_description(format!("目标目录已存在：\n{}\n\n是否下载并覆盖？原有文件会在新版本校验成功后替换。", target.display()))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();
                if result != rfd::MessageDialogResult::Yes {
                    task.status = "已取消覆盖".into();
                    return;
                }
                overwrite = true;
            }
        }
        match ToolSession::start(
            kind,
            &self.config.download_proxy,
            version,
            self.tools.java_major,
            install,
            overwrite,
        ) {
            Ok(session) => {
                task.session = Some(session);
                task.status = "正在启动…".into();
                task.progress = None;
            }
            Err(error) => task.status = error,
        }
    }

    fn disconnect_gamepad(&mut self) {
        self.gamepad.transport.disconnect();
        self.gamepad.target = None;
        self.gamepad.enabled = false;
        self.gamepad.virtual_state = GamepadVisualState::default();
        self.gamepad.last_action = "手柄已断开".to_owned();
    }

    fn connect_gamepad(&mut self) {
        let (Some(adb), Some(serial)) = (self.adb_path(), self.active_serial_online()) else {
            self.gamepad.last_action = "请先连接在线设备并检查 ADB".to_owned();
            return;
        };
        self.gamepad.transport.connect(adb, serial.clone());
        self.gamepad.target = Some(serial);
        self.gamepad.enabled = true;
        self.gamepad.last_action = "正在创建 Android UHID 手柄…".to_owned();
    }

    fn poll_gamepad(&mut self) {
        self.gamepad.hub.poll();
        if self.gamepad.target.is_some() && self.gamepad.target != self.active_serial_online() {
            self.disconnect_gamepad();
        }
        while let Some(status) = self.gamepad.transport.poll() {
            self.gamepad.last_action = status.message;
            if status.failed {
                self.gamepad.enabled = false;
                self.gamepad.target = None;
            }
        }
        self.gamepad.virtual_state = GamepadVisualState::default();
    }

    fn send_gamepad_state(&self) {
        let mut state = self.gamepad.hub.visual_state();
        if self.page == Page::Gamepad
            || (self.page == Page::Screen && self.screen.gamepad_overlay_open)
        {
            state
                .buttons
                .extend(self.gamepad.virtual_state.buttons.iter().copied());
            for i in 0..2 {
                if self.gamepad.virtual_state.sticks[i] != [0.0; 2] {
                    state.sticks[i] = self.gamepad.virtual_state.sticks[i];
                }
                state.triggers[i] = state.triggers[i].max(self.gamepad.virtual_state.triggers[i]);
            }
        }
        if self.gamepad.enabled {
            self.gamepad.transport.send(crate::gamepad_hid::report(
                &state,
                self.config.gamepad.deadzone,
            ));
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }

        for path in dropped {
            let kind = PackageKind::detect(&path);
            if kind != PackageKind::Unknown && self.config.auto_install_drops {
                self.enqueue_package(path, false);
            } else {
                if !self.files.local_files.contains(&path) {
                    self.files.local_files.push(path.clone());
                }
                self.toast(
                    format!("已加入文件工作区：{}", util::file_name(&path)),
                    true,
                );
                self.set_page(Page::Files);
            }
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.modifiers.command) {
            let shortcut = [
                (egui::Key::Num1, Page::Overview),
                (egui::Key::Num2, Page::Screen),
                (egui::Key::Num3, Page::Logcat),
                (egui::Key::Num4, Page::Installer),
                (egui::Key::Num5, Page::Gamepad),
                (egui::Key::Num6, Page::Files),
                (egui::Key::Num7, Page::Settings),
            ];
            for (key, page) in shortcut {
                if ctx.input(|input| input.key_pressed(key)) {
                    self.set_page(page);
                }
            }
        }
        self.handle_dropped_files(ctx);
        self.ensure_watcher();
        self.poll_devices();
        self.ensure_streams();
        self.poll_screen(ctx);
        self.poll_logcat();
        self.poll_installer();
        self.poll_tools();
        self.poll_wireless();
        self.poll_adb_results();
        self.poll_gamepad();
        let installer_ready = self.installer.queue.front().is_some_and(|item| {
            item.convert_only
                || (self.config.resolved_adb().is_some() && self.active_serial_online().is_some())
        });
        if installer_ready {
            self.start_next_install();
        }

        while self
            .toasts
            .first()
            .is_some_and(|toast| toast.created.elapsed() > Duration::from_secs(4))
        {
            self.toasts.remove(0);
        }

        if self.config_dirty && self.config_edit_at.elapsed() > Duration::from_millis(900) {
            self.save_config();
        }

        if self.screen.session.is_some() {
            ctx.request_repaint_after(Duration::from_millis(
                (1000 / self.config.screenshot_fps.max(1) as u64).max(16),
            ));
        } else if self.wireless.busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.log.session.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if self.gamepad.enabled || self.gamepad.hub.active.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn paint_background(&self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(rect, 0.0, colors.bg);

        let time = ctx.input(|input| input.time) as f32;
        let drift_x = (time * 0.12).sin() * 40.0;
        let drift_y = (time * 0.09).cos() * 30.0;
        painter.circle_filled(
            rect.right_top() + egui::vec2(-170.0 + drift_x, 80.0 + drift_y),
            270.0,
            Color32::from_rgba_unmultiplied(43, 218, 195, 16),
        );
        painter.circle_filled(
            rect.left_bottom() + egui::vec2(-80.0 - drift_x, -50.0 - drift_y),
            330.0,
            Color32::from_rgba_unmultiplied(255, 179, 88, 9),
        );
        painter.circle_filled(
            rect.center_bottom() + egui::vec2(120.0, 220.0),
            240.0,
            Color32::from_rgba_unmultiplied(97, 178, 255, 6),
        );

        let grid = Color32::from_rgba_unmultiplied(85, 120, 140, 10);
        let mut x = rect.left();
        while x < rect.right() {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(1.0, grid),
            );
            x += 56.0;
        }
        let mut y = rect.top();
        while y < rect.bottom() {
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                Stroke::new(1.0, grid),
            );
            y += 56.0;
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            crate::branding::image(ui, 40.0);
            ui.vertical(|ui| {
                ui.add_space(1.0);
                ui.label(
                    RichText::new("AndroidTool")
                        .size(19.0)
                        .strong()
                        .color(colors.text),
                );
                ui.label(
                    RichText::new("ANDROID DEVICE TOOLKIT")
                        .size(8.5)
                        .color(colors.muted)
                        .strong(),
                );
            });
        });
        ui.add_space(20.0);
        ui.label(
            RichText::new("工作区")
                .size(10.0)
                .color(colors.muted)
                .strong(),
        );
        ui.add_space(3.0);

        let width = ui.available_width();
        for page in Page::ALL {
            let selected = self.page == page;
            let animation = ui
                .ctx()
                .animate_bool_with_time(Id::new(("nav", page)), selected, 0.18);
            let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 42.0), Sense::click());
            let hovered = response.hovered();
            let bg = if selected {
                theme::mix(colors.card, colors.accent_dark, 0.35)
            } else if hovered {
                colors.card
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
            if animation > 0.01 {
                let bar = Rect::from_min_size(
                    rect.left_top() + egui::vec2(0.0, 8.0),
                    egui::vec2(3.0, rect.height() - 16.0),
                );
                ui.painter().rect_filled(
                    bar,
                    CornerRadius::same(2),
                    theme::alpha(colors.accent, animation),
                );
            }
            paint_icon(
                ui.painter(),
                Rect::from_center_size(
                    rect.left_center() + egui::vec2(17.0, 0.0),
                    Vec2::splat(18.0),
                ),
                page.icon(),
                if selected {
                    colors.accent
                } else {
                    colors.muted
                },
            );
            ui.painter().text(
                rect.left_center() + egui::vec2(43.0, 0.0),
                Align2::LEFT_CENTER,
                page.title(),
                FontId::new(14.0, egui::FontFamily::Proportional),
                if selected {
                    colors.text
                } else {
                    theme::mix(colors.muted, colors.text, 0.35)
                },
            );
            if response.clicked() {
                self.set_page(page);
            }
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            let adb_ok = self.config.resolved_adb().is_some();
            Frame::new()
                .fill(colors.bg_soft)
                .stroke(Stroke::new(1.0, colors.border))
                .corner_radius(12)
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let pulse = ((ui.input(|input| input.time) * 2.4).sin() * 0.5 + 0.5) as f32;
                        let color = if adb_ok { colors.green } else { colors.red };
                        ui.painter().circle_filled(
                            ui.cursor().left_center() + egui::vec2(5.0, 0.0),
                            4.0,
                            theme::alpha(color, 0.55 + pulse * 0.45),
                        );
                        ui.add_space(14.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(if adb_ok {
                                    "ADB 就绪"
                                } else {
                                    "ADB 未配置"
                                })
                                .size(12.0)
                                .strong()
                                .color(if adb_ok {
                                    colors.green
                                } else {
                                    colors.red
                                }),
                            );
                            ui.label(
                                RichText::new(format!("{} 台设备", self.devices.len()))
                                    .size(10.5)
                                    .color(colors.muted),
                            );
                        });
                    });
                });
            ui.add_space(8.0);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::click());
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(8),
                if response.hovered() {
                    colors.card
                } else {
                    Color32::TRANSPARENT
                },
            );
            ui.painter().text(
                rect.left_center() + egui::vec2(8.0, 0.0),
                Align2::LEFT_CENTER,
                if self.config_dirty {
                    "● 配置待保存"
                } else {
                    "● 配置已同步"
                },
                FontId::new(10.5, egui::FontFamily::Proportional),
                if self.config_dirty {
                    colors.orange
                } else {
                    colors.muted
                },
            );
            if response.clicked() {
                self.set_page(Page::Settings);
            }
        });
    }

    fn topbar(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(self.page.title())
                        .size(25.0)
                        .strong()
                        .color(colors.text),
                );
                ui.label(
                    RichText::new(self.page.subtitle())
                        .size(11.5)
                        .color(colors.muted),
                );
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("刷新").size(11.0))
                            .fill(colors.card)
                            .stroke(Stroke::new(1.0, colors.border))
                            .corner_radius(10)
                            .min_size(egui::vec2(46.0, 36.0)),
                    )
                    .on_hover_text("刷新设备")
                    .clicked()
                {
                    self.refresh_devices_now();
                }

                let mut selected = self.active_serial.clone();
                let selected_label = self
                    .active_device()
                    .map(|device| device.display_name())
                    .unwrap_or_else(|| "未选择设备".to_owned());
                egui::ComboBox::from_id_salt("top-device-selector")
                    .selected_text(
                        RichText::new(format!("设备 · {selected_label}"))
                            .size(12.5)
                            .color(colors.text),
                    )
                    .width(210.0)
                    .show_ui(ui, |ui| {
                        ui.set_width(330.0);
                        ui.label(RichText::new("选择操作设备").size(11.0).color(colors.muted));
                        let devices = self.devices.clone();
                        if devices.is_empty() {
                            ui.label("暂无设备，请连接手机或使用无线配对");
                        }
                        ScrollArea::vertical()
                            .id_salt("device-card-list")
                            .max_height(360.0)
                            .show(ui, |ui| {
                                for device in &devices {
                                    if device_picker_card(
                                        ui,
                                        device,
                                        selected.as_deref() == Some(&device.serial),
                                        310.0,
                                    )
                                    .clicked()
                                    {
                                        selected = Some(device.serial.clone());
                                        ui.close();
                                    }
                                }
                            });
                    });
                if selected != self.active_serial {
                    self.set_active_device(selected);
                }

                let ok = self.active_device().is_some_and(DeviceInfo::is_online);
                self.status_chip(
                    ui,
                    if ok { "设备在线" } else { "等待设备" },
                    if ok { colors.green } else { colors.orange },
                );
            });
        });
    }

    fn status_chip(&self, ui: &mut Ui, text: &str, color: Color32) {
        let galley = ui.painter().layout_no_wrap(
            text.to_owned(),
            FontId::new(11.0, egui::FontFamily::Proportional),
            color,
        );
        let size = egui::vec2(galley.size().x + 26.0, 28.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(14),
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 22),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(14),
            Stroke::new(1.0, theme::alpha(color, 0.35)),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .circle_filled(rect.left_center() + egui::vec2(11.0, 0.0), 3.0, color);
        ui.painter().galley(
            rect.left_center() + egui::vec2(18.0, -galley.size().y / 2.0),
            galley,
            color,
        );
    }

    fn card_frame(&self, ui: &Ui) -> Frame {
        let colors = theme::palette(ui.ctx());
        Frame::new()
            .fill(colors.panel)
            .stroke(Stroke::new(1.0, colors.border))
            .corner_radius(16)
            .inner_margin(Margin::same(18))
    }

    fn section_title(&self, ui: &mut Ui, title: &str, detail: &str) {
        let colors = theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).size(15.0).strong().color(colors.text));
            ui.label(RichText::new(detail).size(10.5).color(colors.muted));
        });
    }

    fn page_overview(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        let online = self
            .devices
            .iter()
            .filter(|device| device.is_online())
            .count();
        let gamepads = self.gamepad.hub.gamepads.len();
        ui.horizontal_wrapped(|ui| {
            self.stat_card(
                ui,
                "在线设备",
                &online.to_string(),
                "台",
                colors.accent,
                Icon::Device,
            );
            self.stat_card(
                ui,
                "ADB 设备",
                &self.devices.len().to_string(),
                "台",
                colors.blue,
                Icon::Link,
            );
            self.stat_card(
                ui,
                "手柄",
                &gamepads.to_string(),
                "个",
                colors.orange,
                Icon::Gamepad,
            );
            self.stat_card(
                ui,
                "日志缓存",
                &self.log.entries.len().to_string(),
                "行",
                colors.green,
                Icon::Terminal,
            );
        });

        if let Some(error) = self.device_error.clone() {
            ui.add_space(12.0);
            Frame::new()
                .fill(Color32::from_rgba_unmultiplied(255, 105, 120, 18))
                .stroke(Stroke::new(1.0, theme::alpha(colors.red, 0.45)))
                .corner_radius(12)
                .inner_margin(Margin::symmetric(14, 11))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("ADB").strong().color(colors.red));
                        ui.label(RichText::new(error).size(12.0).color(colors.muted));
                        if ui.link("打开设置").clicked() {
                            self.set_page(Page::Settings);
                        }
                    });
                });
        }

        ui.add_space(18.0);
        ui.horizontal(|ui| {
            self.section_title(ui, "设备矩阵", "点击设备切换当前操作目标");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("刷新").size(11.5))
                            .fill(colors.card)
                            .corner_radius(9),
                    )
                    .clicked()
                {
                    self.refresh_devices_now();
                }
            });
        });
        ui.add_space(2.0);

        if self.devices.is_empty() {
            self.empty_devices_card(ui);
        } else {
            let devices = self.devices.clone();
            let columns = ((ui.available_width() + 12.0) / 300.0).floor().max(1.0) as usize;
            let gap = 12.0;
            let width =
                (ui.available_width() - gap * (columns.saturating_sub(1)) as f32) / columns as f32;
            for chunk in devices.chunks(columns) {
                ui.horizontal(|ui| {
                    for device in chunk {
                        self.device_card(ui, device, width);
                    }
                });
            }
        }

        ui.add_space(20.0);
        self.section_title(ui, "快速操作", "常用路径一步直达");
        ui.add_space(3.0);
        ui.horizontal_wrapped(|ui| {
            if self.quick_action(
                ui,
                Icon::Screen,
                "打开投屏",
                "触摸控制与截图",
                colors.accent,
            ) {
                self.set_page(Page::Screen);
            }
            if self.quick_action(
                ui,
                Icon::Terminal,
                "实时日志",
                "筛选与导入导出",
                colors.green,
            ) {
                self.set_page(Page::Logcat);
            }
            if self.quick_action(
                ui,
                Icon::Package,
                "安装应用",
                "APK · AAB · APKS",
                colors.orange,
            ) {
                self.set_page(Page::Installer);
            }
            if self.quick_action(
                ui,
                Icon::Gamepad,
                "手柄映射",
                "PC / macOS 手柄",
                colors.blue,
            ) {
                self.set_page(Page::Gamepad);
            }
        });

        ui.add_space(20.0);
        self.card_frame(ui).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let icon_rect = Rect::from_min_size(
                    ui.cursor().min + egui::vec2(0.0, 1.0),
                    Vec2::splat(38.0),
                );
                ui.painter().rect_filled(
                    icon_rect,
                    CornerRadius::same(11),
                    Color32::from_rgba_unmultiplied(43, 218, 195, 18),
                );
                paint_icon(ui.painter(), icon_rect.shrink(9.0), Icon::Link, colors.accent);
                ui.add_space(50.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("连接新设备").size(15.0).strong());
                    ui.add_space(3.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "USB 连接：启用开发者选项与 USB 调试后接入数据线，并在手机上允许调试授权。",
                            )
                            .size(11.5)
                            .color(colors.muted)
                            .line_height(Some(18.0)),
                        )
                        .wrap(),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "无线连接：让手机与电脑处于同一局域网，使用 Android 无线调试页面显示的 IP 端口，或扫描二维码完成配对。",
                            )
                            .size(11.5)
                            .color(colors.muted)
                            .line_height(Some(18.0)),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("打开无线配对").strong().color(colors.bg),
                                )
                                .fill(colors.accent)
                                .stroke(Stroke::NONE)
                                .corner_radius(10),
                            )
                            .clicked()
                        {
                            self.open_wireless_pairing(WirelessTab::Ip);
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("扫码配对").color(colors.text),
                                )
                                .fill(colors.card)
                                .stroke(Stroke::new(1.0, colors.border))
                                .corner_radius(10),
                            )
                            .clicked()
                        {
                            self.open_wireless_pairing(WirelessTab::Qr);
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("ADB 设置").color(colors.text),
                                )
                                .fill(colors.card)
                                .stroke(Stroke::new(1.0, colors.border))
                                .corner_radius(10),
                            )
                            .clicked()
                        {
                            self.set_page(Page::Settings);
                        }
                    });
                });
            });
        });
    }

    fn stat_card(
        &self,
        ui: &mut Ui,
        label: &str,
        value: &str,
        suffix: &str,
        color: Color32,
        icon: Icon,
    ) {
        let colors = theme::palette(ui.ctx());
        let size = egui::vec2((ui.available_width() - 36.0) / 4.0, 92.0);
        let (rect, _) = ui.allocate_exact_size(size.max(egui::vec2(155.0, 92.0)), Sense::hover());
        ui.painter()
            .rect_filled(rect, CornerRadius::same(15), colors.panel);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(15),
            Stroke::new(1.0, colors.border),
            egui::StrokeKind::Inside,
        );
        let icon_center = rect.right_top() + egui::vec2(-34.0, 28.0);
        ui.painter().circle_filled(
            icon_center,
            28.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 20),
        );
        paint_icon(
            ui.painter(),
            Rect::from_center_size(icon_center, Vec2::splat(19.0)),
            icon,
            color,
        );
        ui.painter().text(
            rect.left_top() + egui::vec2(16.0, 16.0),
            Align2::LEFT_TOP,
            label,
            FontId::new(10.5, egui::FontFamily::Proportional),
            colors.muted,
        );
        let value_rect = ui.painter().text(
            rect.left_bottom() + egui::vec2(16.0, -18.0),
            Align2::LEFT_BOTTOM,
            value,
            FontId::new(26.0, egui::FontFamily::Proportional),
            colors.text,
        );
        ui.painter().text(
            value_rect.right_center() + egui::vec2(5.0, 0.0),
            Align2::LEFT_CENTER,
            suffix,
            FontId::new(10.5, egui::FontFamily::Proportional),
            colors.muted,
        );
    }

    fn empty_devices_card(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        self.card_frame(ui).show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 150.0),
                Layout::top_down(Align::Center),
                |ui| {
                    ui.add_space(12.0);
                    let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
                    paint_icon(
                        ui.painter(),
                        icon_rect.shrink(5.0),
                        Icon::Device,
                        colors.muted,
                    );
                    ui.label(RichText::new("没有发现 ADB 设备").size(15.0).strong());
                    ui.label(
                        RichText::new("请连接设备并在手机上确认 USB 调试授权")
                            .size(11.5)
                            .color(colors.muted),
                    );
                    ui.add_space(6.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("重新扫描").color(colors.bg))
                                .fill(colors.accent)
                                .corner_radius(9),
                        )
                        .clicked()
                    {
                        self.refresh_devices_now();
                    }
                },
            );
        });
    }

    fn device_card(&mut self, ui: &mut Ui, device: &DeviceInfo, width: f32) {
        let selected = self.active_serial.as_deref() == Some(&device.serial);
        if device_picker_card(ui, device, selected, width).clicked() {
            self.set_active_device(Some(device.serial.clone()));
        }
    }

    fn quick_action(
        &mut self,
        ui: &mut Ui,
        icon: Icon,
        title: &str,
        subtitle: &str,
        color: Color32,
    ) -> bool {
        let colors = theme::palette(ui.ctx());
        let size = egui::vec2((ui.available_width() - 36.0) / 4.0, 78.0);
        let (rect, response) =
            ui.allocate_exact_size(size.max(egui::vec2(185.0, 78.0)), Sense::click());
        let hover = response.hovered();
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(14),
            if hover {
                colors.card_hover
            } else {
                colors.panel
            },
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(14),
            Stroke::new(
                1.0,
                if hover {
                    theme::alpha(color, 0.5)
                } else {
                    colors.border
                },
            ),
            egui::StrokeKind::Inside,
        );
        ui.painter().circle_filled(
            rect.left_center() + egui::vec2(30.0, 0.0),
            20.0,
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 18),
        );
        paint_icon(
            ui.painter(),
            Rect::from_center_size(
                rect.left_center() + egui::vec2(30.0, 0.0),
                Vec2::splat(20.0),
            ),
            icon,
            color,
        );
        ui.painter().text(
            rect.left_center() + egui::vec2(60.0, -8.0),
            Align2::LEFT_CENTER,
            title,
            FontId::new(12.5, egui::FontFamily::Proportional),
            colors.text,
        );
        ui.painter().text(
            rect.left_center() + egui::vec2(60.0, 10.0),
            Align2::LEFT_CENTER,
            subtitle,
            FontId::new(9.5, egui::FontFamily::Proportional),
            colors.muted,
        );
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }

    fn page_screen(&mut self, ui: &mut Ui) {
        if self.active_serial_online().is_none() {
            self.no_device_panel(ui);
            return;
        }
        if self.config.scrcpy_enabled {
            self.scrcpy_panel(ui);
            return;
        }
        self.compatible_screen_panel(ui);
    }

    fn scrcpy_panel(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        let embedded = self.config.mirror_embedded && cfg!(windows);
        ui.horizontal_wrapped(|ui| {
            if ui.button("截图兼容模式").clicked() {
                self.config.scrcpy_enabled = false;
                self.screen.mirror = None;
                self.screen.error = None;
                self.mark_config_dirty();
            }
            if ui
                .button(if self.screen.mirror.is_some() {
                    "重启视频流"
                } else {
                    "启动 scrcpy"
                })
                .clicked()
            {
                self.screen.mirror_requested = true;
            }
            if ui
                .add_enabled(self.screen.mirror.is_some(), egui::Button::new("停止"))
                .clicked()
            {
                self.screen.mirror = None;
                self.screen.mirror_requested = false;
                self.screen.mirror_status = "投屏已停止。".to_owned();
            }
            #[cfg(windows)]
            if ui
                .button(if embedded {
                    "独立窗口"
                } else {
                    "页面内嵌"
                })
                .clicked()
            {
                self.config.mirror_embedded = !self.config.mirror_embedded;
                self.screen.mirror = None;
                self.screen.mirror_requested = true;
                self.screen.error = None;
                self.mark_config_dirty();
            }
            let mut changed = false;
            changed |= ui
                .add(egui::Slider::new(&mut self.config.mirror_fps, 1..=60).suffix(" FPS"))
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut self.config.mirror_max_size)
                        .range(640..=2560)
                        .suffix(" px"),
                )
                .changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.config.mirror_bit_rate_mbps, 1..=32)
                        .suffix(" Mbps"),
                )
                .changed();
            if changed {
                self.mark_config_dirty();
            }
        });
        if let Some(session) = &self.screen.mirror {
            ui.label(
                RichText::new(format!(
                    "{} · 上限 {} FPS · {} px · {} Mbps",
                    session.serial,
                    session.options.fps,
                    session.options.max_size,
                    session.options.bit_rate_mbps
                ))
                .size(10.5)
                .color(colors.muted),
            );
        } else {
            ui.label(
                RichText::new(&self.screen.mirror_status)
                    .size(10.5)
                    .color(colors.muted),
            );
        }
        if let Some(error) = &self.screen.error {
            ui.add(egui::Label::new(RichText::new(error).color(colors.red)).wrap());
        }
        self.screen_with_controls(ui, embedded);
    }

    fn compatible_screen_panel(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            if ui.button("scrcpy 高性能视频流").clicked() {
                self.config.scrcpy_enabled = true;
                self.screen.error = None;
                self.mark_config_dirty();
            }
            ui.label(RichText::new("截图兼容模式").size(10.5).color(colors.muted));
        });
        self.screen_with_controls(ui, false);
    }

    fn screen_with_controls(&mut self, ui: &mut Ui, embedded: bool) {
        let colors = theme::palette(ui.ctx());
        ui.add_space(6.0);
        let (area, _) = ui.allocate_exact_size(
            ui.available_size().max(egui::vec2(1.0, 1.0)),
            Sense::hover(),
        );
        let controls_width = 250.0;
        let screen_area = Rect::from_min_max(
            area.min,
            egui::pos2(
                (area.right() - controls_width - 18.0).max(area.left() + 1.0),
                area.bottom(),
            ),
        );
        let controls_area =
            Rect::from_min_max(egui::pos2(screen_area.right() + 18.0, area.top()), area.max);
        let video_area = screen_area;
        self.screen.canvas_rect = Some(video_area);
        ui.scope_builder(egui::UiBuilder::new().max_rect(video_area), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(video_area));
            if self.config.scrcpy_enabled {
                let rect = video_area;
                ui.painter()
                    .rect_filled(rect, 18, Color32::from_rgb(6, 9, 13));
                ui.painter().rect_stroke(
                    rect,
                    18,
                    Stroke::new(1.0, colors.border),
                    egui::StrokeKind::Inside,
                );
                if !self
                    .screen
                    .mirror
                    .as_ref()
                    .is_some_and(MirrorSession::is_attached)
                {
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        if !embedded && self.screen.mirror.is_some() {
                            "视频正在 scrcpy 独立窗口显示"
                        } else if self.screen.mirror.is_some() {
                            "正在连接视频流…"
                        } else {
                            "投屏已停止 · 点击启动 scrcpy"
                        },
                        FontId::proportional(14.0),
                        theme::MUTED,
                    );
                }
                self.screen.mirror_rect =
                    embedded.then(|| rect.shrink(10.0).intersect(ui.clip_rect()));
            } else {
                self.screen.mirror_rect = None;
                self.screen_canvas(ui);
            }
        });
        ui.painter().line_segment(
            [
                egui::pos2(controls_area.left() - 9.0, area.top()),
                egui::pos2(controls_area.left() - 9.0, area.bottom()),
            ],
            Stroke::new(1.0, colors.border),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(controls_area), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(controls_area));
            ScrollArea::vertical()
                .id_salt("screen-right-panel")
                .auto_shrink([false, false])
                .show(ui, |ui| self.screen_controls(ui));
        });
    }

    fn show_gamepad_overlay(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        if self.page != Page::Screen
            || !self.screen.gamepad_overlay_open
            || self.wireless.open
            || egui::Popup::is_any_open(ctx)
            || ctx.input(|input| {
                input.viewport().minimized == Some(true) || !input.raw.hovered_files.is_empty()
            })
        {
            return;
        }
        let Some(area) = self.screen.canvas_rect else {
            return;
        };
        let width = 330.0_f32
            .min(area.width() - 16.0)
            .min((area.height() - 98.0) * 560.0 / 360.0)
            .max(1.0);
        let size = egui::vec2(width, 82.0 + (width - 16.0).max(1.0) * 360.0 / 560.0);
        let position = self
            .screen
            .gamepad_overlay_position
            .unwrap_or(egui::pos2(1.0, 1.0));
        let rect = gamepad_overlay_rect(area, size, position);
        let origin = ctx
            .input(|input| input.viewport().inner_rect.map(|rect| rect.min))
            .unwrap_or_default();
        let builder = egui::ViewportBuilder::default()
            .with_title("AndroidTool - Floating Gamepad")
            .with_decorations(false)
            .with_transparent(true)
            .with_has_shadow(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_active(false)
            .with_position(origin + rect.min.to_vec2())
            .with_inner_size(size);
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("floating-gamepad"),
            builder,
            |overlay_ctx, class| {
                let draw = |ui: &mut Ui, app: &mut Self| {
                    ui.spacing_mut().interact_size.y = 25.0;
                    ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
                    ui.horizontal(|ui| {
                        let (handle, response) =
                            ui.allocate_exact_size(egui::vec2(32.0, 25.0), Sense::drag());
                        let response = response
                            .on_hover_text("拖动手柄")
                            .on_hover_cursor(egui::CursorIcon::Grab);
                        for row in 0..3 {
                            for column in 0..2 {
                                ui.painter().circle_filled(
                                    handle.center()
                                        + egui::vec2(
                                            (column as f32 - 0.5) * 5.0,
                                            (row as f32 - 1.0) * 5.0,
                                        ),
                                    1.5,
                                    colors.accent,
                                );
                            }
                        }
                        if let Some(pointer) = response.interact_pointer_pos() {
                            let local = if class == egui::ViewportClass::Embedded {
                                pointer - rect.min.to_vec2()
                            } else {
                                pointer
                            };
                            if response.drag_started() {
                                app.screen.gamepad_drag_offset = Some(local.to_vec2());
                            }
                            if response.dragged() {
                                let global = if class == egui::ViewportClass::Embedded {
                                    pointer + origin.to_vec2()
                                } else {
                                    ui.input(|input| {
                                        input
                                            .viewport()
                                            .inner_rect
                                            .map(|rect| rect.min)
                                            .unwrap_or(origin + rect.min.to_vec2())
                                    }) + pointer.to_vec2()
                                };
                                let travel =
                                    (area.size() - size - Vec2::splat(16.0)).max(Vec2::splat(1.0));
                                let offset =
                                    app.screen.gamepad_drag_offset.unwrap_or(local.to_vec2());
                                let relative = (global
                                    - origin
                                    - offset
                                    - area.min.to_vec2()
                                    - Vec2::splat(8.0))
                                    / travel;
                                app.screen.gamepad_overlay_position = Some(egui::pos2(
                                    relative.x.clamp(0.0, 1.0),
                                    relative.y.clamp(0.0, 1.0),
                                ));
                                ctx.request_repaint();
                            }
                        }
                        if response.drag_stopped() {
                            app.screen.gamepad_drag_offset = None;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("隐藏").clicked() {
                                app.screen.gamepad_overlay_open = false;
                                ctx.request_repaint();
                            }
                            if ui
                                .small_button(if app.gamepad.enabled {
                                    "断开手柄"
                                } else {
                                    "连接手柄"
                                })
                                .clicked()
                            {
                                if app.gamepad.enabled {
                                    app.disconnect_gamepad();
                                } else {
                                    app.connect_gamepad();
                                }
                            }
                        });
                    });
                    ui.set_opacity(0.82);
                    app.virtual_gamepad(ui);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&app.gamepad.last_action)
                                .size(9.0)
                                .color(colors.muted),
                        )
                        .truncate(),
                    );
                };
                if class == egui::ViewportClass::Embedded {
                    egui::Area::new(Id::new("floating-gamepad-fallback"))
                        .fixed_pos(rect.min)
                        .order(Order::Foreground)
                        .show(overlay_ctx, |ui| {
                            ui.set_width(size.x - 16.0);
                            Frame::new()
                                .fill(theme::alpha(colors.panel, 0.90))
                                .corner_radius(14)
                                .inner_margin(8)
                                .show(ui, |ui| draw(ui, self));
                        });
                } else {
                    if overlay_ctx.input(|input| input.viewport().close_requested()) {
                        self.screen.gamepad_overlay_open = false;
                    }
                    egui::CentralPanel::default()
                        .frame(
                            Frame::new()
                                .fill(theme::alpha(colors.panel, 0.86))
                                .stroke(Stroke::new(1.0, theme::alpha(colors.accent, 0.4)))
                                .corner_radius(14)
                                .inner_margin(8),
                        )
                        .show(overlay_ctx, |ui| draw(ui, self));
                    overlay_ctx.request_repaint_after(Duration::from_millis(16));
                }
            },
        );
        #[cfg(windows)]
        if let Some(parent) = self.native_window {
            crate::mirror_window::own_gamepad_overlay(parent as _);
        }
    }

    #[cfg(windows)]
    fn sync_embedded_mirror(&mut self, ctx: &egui::Context) {
        let Some(session) = self
            .screen
            .mirror
            .as_mut()
            .filter(|session| session.embedded)
        else {
            return;
        };
        let Some(parent) = self.native_window else {
            self.screen.error = Some("无法获取主窗口，请切换独立窗口模式。".to_owned());
            self.screen.mirror = None;
            return;
        };
        let rect = self
            .screen
            .mirror_rect
            .unwrap_or(Rect::ZERO)
            .intersect(ctx.content_rect());
        let visible = self.page == Page::Screen
            && self.config.scrcpy_enabled
            && self.config.mirror_embedded
            && rect.width() > 1.0
            && rect.height() > 1.0
            && !self.wireless.open
            && !egui::Popup::is_any_open(ctx)
            && !ctx.input(|input| !input.raw.hovered_files.is_empty());
        let scale = ctx.pixels_per_point();
        let bounds = [
            (rect.left() * scale).round() as i32,
            (rect.top() * scale).round() as i32,
            (rect.width() * scale).round() as i32,
            (rect.height() * scale).round() as i32,
        ];
        if let Err(error) = session.sync_embedded(parent as _, bounds, visible) {
            self.screen.error = Some(error);
            self.screen.mirror = None;
        }
    }

    fn screen_canvas(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        let available = ui.available_size();
        let frame = Frame::new()
            .fill(Color32::from_rgb(6, 9, 13))
            .stroke(Stroke::new(1.0, colors.border))
            .corner_radius(18)
            .inner_margin(Margin::same(10));
        frame.show(ui, |ui| {
            ui.set_min_size(available - egui::vec2(22.0, 22.0));
            if let Some(texture) = self.screen.texture.clone() {
                let image_size = texture.size_vec2();
                let max_size =
                    (ui.available_size() - egui::vec2(8.0, 8.0)).max(egui::vec2(1.0, 1.0));
                let scale = (max_size.x / image_size.x)
                    .min(max_size.y / image_size.y)
                    .min(1.4);
                let display = image_size * scale;
                let (outer, _) = ui.allocate_exact_size(
                    egui::vec2(max_size.x, max_size.y.max(display.y)),
                    Sense::hover(),
                );
                let rect = Rect::from_center_size(outer.center(), display);
                let response =
                    ui.interact(rect, Id::new("screen-interact"), Sense::click_and_drag());
                ui.painter().rect_filled(
                    rect.expand(2.0),
                    CornerRadius::same(12),
                    Color32::from_black_alpha(150),
                );
                ui.painter().image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(12),
                    Stroke::new(1.0, theme::alpha(colors.accent, 0.25)),
                    egui::StrokeKind::Inside,
                );

                if response.drag_started_by(PointerButton::Primary) {
                    self.screen.pointer_start = response.interact_pointer_pos();
                }
                if response.drag_stopped_by(PointerButton::Primary) {
                    if let (Some(start), Some(end)) = (
                        self.screen.pointer_start.take(),
                        response.interact_pointer_pos(),
                    ) {
                        if start.distance(end) > 7.0 {
                            let (x1, y1) = normalized_screen_point(start, rect);
                            let (x2, y2) = normalized_screen_point(end, rect);
                            let [width, height] = self.screen.device_size.unwrap_or([1080, 2400]);
                            self.queue_swipe_px(
                                (x1 * width as f32) as i32,
                                (y1 * height as f32) as i32,
                                (x2 * width as f32) as i32,
                                (y2 * height as f32) as i32,
                                160,
                            );
                        }
                    }
                }
                if response.clicked_by(PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let (x, y) = normalized_screen_point(pos, rect);
                        self.queue_tap_norm(x, y, 40);
                    }
                }
                if response.clicked_by(PointerButton::Secondary) {
                    self.queue_key(4);
                }
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
            } else {
                let (rect, _) = ui.allocate_exact_size(
                    ui.available_size().max(egui::vec2(1.0, 1.0)),
                    Sense::hover(),
                );
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "等待屏幕画面…",
                    FontId::new(15.0, egui::FontFamily::Proportional),
                    theme::MUTED,
                );
                ui.painter().text(
                    rect.center() + egui::vec2(0.0, 24.0),
                    Align2::CENTER_CENTER,
                    self.screen
                        .error
                        .as_deref()
                        .unwrap_or("ADB screencap 正在启动"),
                    FontId::new(10.5, egui::FontFamily::Proportional),
                    if self.screen.error.is_some() {
                        theme::RED
                    } else {
                        theme::MUTED
                    },
                );
            }
        });
    }

    fn screen_controls(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        ui.label(RichText::new("设备控制").size(14.0).strong());
        ui.add_space(2.0);
        let resolution = self
            .screen
            .device_size
            .map(|[width, height]| format!("{width} × {height}"))
            .unwrap_or_else(|| "读取中…".to_owned());
        ui.label(RichText::new(resolution).size(10.5).color(colors.muted));
        ui.horizontal_wrapped(|ui| {
            self.status_chip(
                ui,
                &format!(
                    "{} {} FPS",
                    if self.config.scrcpy_enabled {
                        "上限"
                    } else {
                        "目标"
                    },
                    if self.config.scrcpy_enabled {
                        self.config.mirror_fps
                    } else {
                        self.config.screenshot_fps
                    }
                ),
                colors.accent,
            );
            if self.config.scrcpy_enabled {
                ui.label(
                    RichText::new("scrcpy · H.264")
                        .size(10.0)
                        .color(colors.muted),
                );
            } else if let Some(last) = self.screen.last_frame {
                ui.label(
                    RichText::new(format!(
                        "实际 {:.1} FPS · {:.1}s 前",
                        self.screen.measured_fps,
                        last.elapsed().as_secs_f32()
                    ))
                    .size(10.0)
                    .color(colors.muted),
                );
            }
        });
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            if control_button(ui, "返回", "返回").clicked() {
                self.queue_key(4);
            }
            if control_button(ui, "主页", "主页").clicked() {
                self.queue_key(3);
            }
            if control_button(ui, "多任务", "多任务").clicked() {
                self.queue_key(187);
            }
            if control_button(ui, "电源", "电源").clicked() {
                self.queue_key(26);
            }
            if control_button(ui, "音量+", "音量 +").clicked() {
                self.queue_key(24);
            }
            if control_button(ui, "音量-", "音量 -").clicked() {
                self.queue_key(25);
            }
            if control_button(ui, "菜单", "菜单").clicked() {
                self.queue_key(82);
            }
            if control_button(ui, "搜索", "搜索").clicked() {
                self.queue_key(84);
            }
        });

        ui.add_space(12.0);
        ui.label(RichText::new("文本输入").size(11.0).color(colors.muted));
        let response = ui.add(
            TextEdit::singleline(&mut self.screen.text_input)
                .hint_text("ASCII 文本")
                .desired_width(f32::INFINITY),
        );
        let send = if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
        {
            true
        } else {
            ui.add_sized(
                [ui.available_width(), 32.0],
                egui::Button::new("发送到设备")
                    .fill(colors.card)
                    .corner_radius(9),
            )
            .clicked()
        };
        if send && !self.screen.text_input.is_empty() {
            self.queue_shell(
                vec![
                    "input".to_owned(),
                    "text".to_owned(),
                    self.screen.text_input.replace(' ', "%s"),
                ],
                "TEXT".to_owned(),
            );
            self.screen.text_input.clear();
        }

        ui.add_space(12.0);
        ui.label(RichText::new("快捷操作").size(11.0).color(colors.muted));
        if ui
            .add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new("保存当前截图")
                    .fill(colors.card)
                    .corner_radius(9),
            )
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(format!(
                    "androidtool-{}.png",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ))
                .add_filter("PNG", &["png"])
                .save_file()
            {
                if let (Some(adb_path), Some(serial)) =
                    (self.adb_path(), self.active_serial_online())
                {
                    self.adb_worker.submit(AdbJob {
                        adb_path,
                        serial,
                        task: AdbTask::Screenshot(path),
                        label: "SHOT".to_owned(),
                    });
                }
            }
        }
        if ui
            .add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new("唤醒屏幕")
                    .fill(colors.card)
                    .corner_radius(9),
            )
            .clicked()
        {
            self.queue_key(224);
        }
        if !self.config.scrcpy_enabled
            && ui
                .add_sized(
                    [ui.available_width(), 34.0],
                    egui::Button::new("屏幕截图刷新")
                        .fill(colors.card)
                        .corner_radius(9),
                )
                .clicked()
        {
            self.screen.session = None;
        }
        if let Some(error) = self.screen.error.as_ref() {
            ui.add_space(8.0);
            ui.label(RichText::new(error).size(10.0).color(colors.red));
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new(if self.config.scrcpy_enabled {
                "点击画面使用 scrcpy 鼠标键盘控制。"
            } else {
                "左键点击 / 拖动发送触摸，右键返回。"
            })
            .size(9.5)
            .color(colors.muted),
        );
        ui.add_space(10.0);
        if ui
            .add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new(if self.screen.gamepad_overlay_open {
                    "隐藏手柄"
                } else {
                    "显示手柄"
                })
                .fill(colors.card)
                .corner_radius(9),
            )
            .clicked()
        {
            self.screen.gamepad_overlay_open = !self.screen.gamepad_overlay_open;
        }
        ui.add(
            egui::Label::new(
                RichText::new("半透明悬浮手柄 · 拖动图标调整位置")
                    .size(9.5)
                    .color(colors.muted),
            )
            .wrap(),
        );
    }

    fn page_logcat(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        if self.active_serial_online().is_none() {
            self.no_device_panel(ui);
            return;
        }
        let choices = self.log.application_choices();
        ui.horizontal_wrapped(|ui| {
            ui.label("应用 / 包名");
            let selected_label = match self.log.package_mode {
                LogPackageMode::Foreground => format!(
                    "自动前台 · {}",
                    self.log.foreground.as_deref().unwrap_or("扫描中")
                ),
                LogPackageMode::Package => self.log.package_input.clone(),
                LogPackageMode::All => "全部日志".to_owned(),
            };
            egui::ComboBox::from_id_salt("log-application-selector")
                .selected_text(util::truncate_chars(&selected_label, 48))
                .width(350.0)
                .show_ui(ui, |ui| {
                    ui.set_width(420.0);
                    if ui
                        .selectable_label(
                            self.log.package_mode == LogPackageMode::Foreground,
                            "自动选择当前前台应用",
                        )
                        .clicked()
                    {
                        self.log.package_mode = LogPackageMode::Foreground;
                        self.log.filter_dirty = true;
                        ui.close();
                    }
                    if ui
                        .selectable_label(
                            self.log.package_mode == LogPackageMode::All,
                            "全部日志（不筛选包名）",
                        )
                        .clicked()
                    {
                        self.log.package_mode = LogPackageMode::All;
                        self.log.filter_dirty = true;
                        ui.close();
                    }
                    ui.separator();
                    ui.label(
                        RichText::new("自动扫描的应用（前台优先，包含已识别的前台应用）")
                            .size(10.0)
                            .color(colors.muted),
                    );
                    ui.add(
                        TextEdit::singleline(&mut self.log.package_search)
                            .hint_text("搜索或输入包名…")
                            .desired_width(ui.available_width()),
                    );
                    let query = self.log.package_search.trim().to_ascii_lowercase();
                    ScrollArea::vertical()
                        .id_salt("log-applications")
                        .max_height(260.0)
                        .show(ui, |ui| {
                            let mut count = 0;
                            for package in choices
                                .iter()
                                .filter(|package| package.to_ascii_lowercase().contains(&query))
                            {
                                count += 1;
                                let label = if self.log.foreground.as_ref() == Some(package) {
                                    format!("{package}  [当前前台]")
                                } else if self.log.scanned_foreground.contains(package) {
                                    format!("{package}  [曾在前台]")
                                } else {
                                    package.clone()
                                };
                                if ui
                                    .selectable_label(
                                        self.log.package_mode == LogPackageMode::Package
                                            && self.log.package_input == *package,
                                        util::truncate_chars(&label, 62),
                                    )
                                    .on_hover_text(&label)
                                    .clicked()
                                {
                                    self.log.package_mode = LogPackageMode::Package;
                                    self.log.package_input = package.clone();
                                    self.log.filter_dirty = true;
                                    ui.close();
                                }
                            }
                            if count == 0 {
                                ui.label(
                                    "暂无匹配应用；可打开手机应用等待自动扫描，或直接输入包名。",
                                );
                            }
                        });
                    let package = self.log.package_search.trim().to_owned();
                    if !package.is_empty() && ui.button("使用输入的包名").clicked() {
                        self.log.package_input = package;
                        self.log.package_mode = LogPackageMode::Package;
                        self.log.filter_dirty = true;
                        ui.close();
                    }
                });
            if ui.button("立即扫描").clicked() {
                if let Some(session) = &self.log.session {
                    session.scan_now();
                }
            }
            ui.label(
                RichText::new("自动扫描约每秒一次 · 包名筛选包含子进程")
                    .size(10.0)
                    .color(colors.muted),
            );
        });
        if let Some(error) = &self.log.process_error {
            ui.label(
                RichText::new(format!("应用识别失败：{error}"))
                    .size(10.0)
                    .color(colors.orange),
            );
        }

        ui.horizontal_wrapped(|ui| {
            let old_level = self.log.min_level;
            egui::ComboBox::from_id_salt("log-level")
                .selected_text(format!("级别 ≥ {}", self.log.min_level.letter()))
                .width(90.0)
                .show_ui(ui, |ui| {
                    for level in LogLevel::all() {
                        ui.selectable_value(
                            &mut self.log.min_level,
                            level,
                            format!("{}  {:?}", level.letter(), level),
                        );
                    }
                });
            if old_level != self.log.min_level {
                self.log.filter_dirty = true;
            }

            let search = ui.add(
                TextEdit::singleline(&mut self.log.search)
                    .hint_text("搜索 tag 或消息…")
                    .desired_width(260.0),
            );
            if search.changed() {
                self.log.filter_dirty = true;
            }

            let pause_text = if self.log.paused { "继续" } else { "暂停" };
            if ui
                .add(
                    egui::Button::new(pause_text)
                        .fill(colors.card)
                        .corner_radius(9),
                )
                .clicked()
            {
                self.log.paused = !self.log.paused;
            }
            if ui
                .add(
                    egui::Button::new("清空视图")
                        .fill(colors.card)
                        .corner_radius(9),
                )
                .clicked()
            {
                self.log.entries.clear();
                self.log.filter_dirty = true;
            }
            if ui
                .add(
                    egui::Button::new("清空设备日志")
                        .fill(colors.card)
                        .corner_radius(9),
                )
                .clicked()
            {
                self.queue_shell(
                    vec!["logcat".to_owned(), "-c".to_owned()],
                    "LOGCAT_CLEAR".to_owned(),
                );
            }
            if ui
                .add(egui::Button::new("导入").fill(colors.card).corner_radius(9))
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Logcat", &["log", "txt"])
                    .pick_file()
                {
                    match import_file(&path) {
                        Ok(entries) => {
                            let count = entries.len();
                            self.log.entries.extend(entries);
                            while self.log.entries.len() > self.config.max_log_lines {
                                self.log.entries.pop_front();
                            }
                            self.log.filter_dirty = true;
                            self.toast(format!("已导入 {count} 行日志"), true);
                        }
                        Err(error) => self.toast(error, false),
                    }
                }
            }
            if ui
                .add(egui::Button::new("导出").fill(colors.card).corner_radius(9))
                .clicked()
            {
                self.export_logs();
            }
            let mut auto = self.config.log_auto_scroll;
            if ui.checkbox(&mut auto, "自动滚动").changed() {
                self.config.log_auto_scroll = auto;
                self.mark_config_dirty();
            }
        });

        self.ensure_log_filter();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} / {} 行",
                    self.log.filter_cache.len(),
                    self.log.entries.len()
                ))
                .size(10.5)
                .color(colors.muted),
            );
            if self.log.paused {
                self.status_chip(ui, "已暂停", colors.orange);
            } else {
                self.status_chip(ui, "实时接收", colors.green);
            }
            if let Some(error) = self.log.error.as_ref() {
                ui.label(RichText::new(error).size(10.0).color(colors.red));
            }
        });

        let row_height = 24.0;
        let log = &self.log;
        let height = (ui.available_height() - 8.0).max(160.0);
        Frame::new()
            .fill(colors.bg_soft)
            .stroke(Stroke::new(1.0, colors.border))
            .corner_radius(13)
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.add_space(1.0);
                log_header(ui);
                ui.separator();
                ScrollArea::vertical()
                    .id_salt("logcat-scroll")
                    .auto_shrink([false, false])
                    .max_height(height - 38.0)
                    .stick_to_bottom(self.config.log_auto_scroll)
                    .show_rows(ui, row_height, log.filter_cache.len(), |ui, range| {
                        for row in range {
                            let Some(index) = log.filter_cache.get(row) else {
                                continue;
                            };
                            let Some(entry) = log.entries.get(*index) else {
                                continue;
                            };
                            log_row(ui, entry, row);
                        }
                    });
            });
    }

    fn ensure_log_filter(&mut self) {
        if !self.log.filter_dirty {
            return;
        }
        let query = self.log.search.trim().to_ascii_lowercase();
        self.log.filter_cache.clear();
        for (index, entry) in self.log.entries.iter().enumerate() {
            let package = match self.log.package_mode {
                LogPackageMode::All => None,
                LogPackageMode::Foreground => Some(self.log.foreground.as_deref().unwrap_or("")),
                LogPackageMode::Package => Some(self.log.package_input.trim()),
            };
            if let Some(package) = package {
                if package.is_empty() || !process_matches_package(&entry.process, package) {
                    continue;
                }
            }
            if entry.level < self.log.min_level {
                continue;
            }
            if !query.is_empty() && !entry.search_text().contains(&query) {
                continue;
            }
            self.log.filter_cache.push(index);
        }
        self.log.filter_dirty = false;
    }

    fn export_logs(&mut self) {
        self.ensure_log_filter();
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!(
                "logcat-{}.log",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ))
            .add_filter("Logcat", &["log"])
            .save_file()
        else {
            return;
        };
        let filtered: Vec<&LogEntry> = self
            .log
            .filter_cache
            .iter()
            .filter_map(|index| self.log.entries.get(*index))
            .collect();
        match export_file(&path, &filtered) {
            Ok(()) => self.toast(
                format!("已导出 {} 行到 {}", filtered.len(), path.display()),
                true,
            ),
            Err(error) => self.toast(error, false),
        }
    }

    fn page_installer(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            self.stat_card(
                ui,
                "队列",
                &self.installer.queue.len().to_string(),
                "项",
                colors.orange,
                Icon::Package,
            );
            self.stat_card(
                ui,
                "目标设备",
                &self
                    .active_device()
                    .map(|device| device.display_name())
                    .unwrap_or_else(|| "无".to_owned()),
                "",
                colors.accent,
                Icon::Device,
            );
            self.stat_card(
                ui,
                "Java",
                if self.config.resolved_java().is_some() {
                    "就绪"
                } else {
                    "未配置"
                },
                "",
                if self.config.resolved_java().is_some() {
                    colors.green
                } else {
                    colors.red
                },
                Icon::Code,
            );
            self.stat_card(
                ui,
                "bundletool",
                if self.config.resolved_bundletool().is_some() {
                    "就绪"
                } else {
                    "未配置"
                },
                "",
                if self.config.resolved_bundletool().is_some() {
                    colors.green
                } else {
                    colors.red
                },
                Icon::Package,
            );
        });

        ui.add_space(14.0);
        let drop_response = Frame::new()
            .fill(Color32::from_rgba_unmultiplied(43, 218, 195, 8))
            .stroke(Stroke::new(1.0, theme::alpha(colors.accent, 0.35)))
            .corner_radius(16)
            .inner_margin(Margin::same(22))
            .show(ui, |ui| {
                ui.set_min_height(112.0);
                ui.vertical_centered(|ui| {
                    let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                    paint_icon(
                        ui.painter(),
                        icon_rect.shrink(5.0),
                        Icon::Package,
                        colors.accent,
                    );
                    ui.label(
                        RichText::new("拖拽 APK / AAB / APKS 到此，自动识别并安装")
                            .size(14.0)
                            .strong(),
                    );
                    ui.label(
                        RichText::new("AAB 将自动构建 Universal APK，转换产物保存在 converted/")
                            .size(10.5)
                            .color(colors.muted),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("选择并安装").color(colors.bg))
                                    .fill(colors.accent)
                                    .corner_radius(9),
                            )
                            .clicked()
                        {
                            if let Some(paths) = rfd::FileDialog::new()
                                .add_filter("Android 安装包", &["apk", "aab", "apks"])
                                .pick_files()
                            {
                                for path in paths {
                                    self.enqueue_package(path, false);
                                }
                            }
                        }
                        if ui
                            .add(
                                egui::Button::new("AAB 仅转换")
                                    .fill(colors.card)
                                    .corner_radius(9),
                            )
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Android App Bundle", &["aab"])
                                .pick_file()
                            {
                                self.enqueue_package(path, true);
                            }
                        }
                    });
                });
            })
            .response;
        if drop_response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if !self.installer.queue.is_empty() {
            ui.add_space(12.0);
            self.section_title(ui, "等待队列", "按顺序执行");
            for (index, item) in self.installer.queue.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:02}", index + 1))
                            .monospace()
                            .size(10.0)
                            .color(colors.muted),
                    );
                    ui.label(
                        RichText::new(item.kind.label())
                            .size(10.0)
                            .color(colors.orange),
                    );
                    ui.label(
                        RichText::new(util::truncate_chars(&util::file_name(&item.path), 46))
                            .size(11.5),
                    );
                    if item.convert_only {
                        ui.label(RichText::new("仅转换").size(9.0).color(colors.accent));
                    }
                });
            }
        }

        if self.installer.session.is_some()
            || !self.installer.stage.is_empty()
            || !self.installer.logs.is_empty()
        {
            ui.add_space(14.0);
            self.card_frame(ui).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(
                            self.installer
                                .active_name
                                .clone()
                                .unwrap_or_else(|| "安装任务".to_owned()),
                        )
                        .size(14.0)
                        .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.installer.active_convert_only {
                            self.status_chip(ui, "AAB → APK", colors.accent);
                        } else {
                            self.status_chip(ui, "安装", colors.orange);
                        }
                    });
                });
                ui.add(
                    ProgressBar::new(self.installer.progress)
                        .text(&self.installer.stage)
                        .animate(true)
                        .fill(colors.accent_dark),
                );
                ScrollArea::vertical()
                    .id_salt("installer-log")
                    .max_height(160.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.installer.logs {
                            ui.label(
                                RichText::new(line)
                                    .monospace()
                                    .size(10.5)
                                    .color(colors.muted),
                            );
                        }
                    });
                if let Some(path) = self.installer.converted.as_ref() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("APK").size(10.0).color(colors.accent));
                        ui.label(
                            RichText::new(path.display().to_string())
                                .monospace()
                                .size(10.0)
                                .color(colors.muted),
                        );
                        if ui.link("打开目录").clicked() {
                            if let Some(parent) = path.parent() {
                                util::open_folder(parent);
                            }
                        }
                    });
                }
            });
        }
    }

    fn virtual_gamepad(&mut self, ui: &mut Ui) {
        let kind =
            self.gamepad
                .virtual_kind
                .unwrap_or_else(|| match self.gamepad.hub.active_kind() {
                    GamepadKind::Generic => GamepadKind::Xbox,
                    kind => kind,
                });
        let actions = crate::gamepad_view::show(
            ui,
            kind,
            &self.gamepad.hub.visual_state(),
            self.config.gamepad.deadzone,
        );
        let mut state = GamepadVisualState::default();
        for key in actions {
            if let Some(values) = key.strip_prefix("__stick_left:") {
                if let Some((x, y)) = parse_stick_values(values) {
                    state.sticks[0] = [x, y];
                }
            } else if let Some(values) = key.strip_prefix("__stick_right:") {
                if let Some((x, y)) = parse_stick_values(values) {
                    state.sticks[1] = [x, y];
                }
            } else if let Some(key) = crate::gamepad_hid::button_name(&key) {
                state.buttons.insert(key);
            }
        }
        self.gamepad.virtual_state = state;
        if !self.gamepad.enabled
            && (!self.gamepad.virtual_state.buttons.is_empty()
                || self.gamepad.virtual_state.sticks != [[0.0; 2]; 2])
        {
            self.gamepad.last_action = "请在手柄控制页连接原生手柄".to_owned();
        }
    }

    fn page_gamepad(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        let target_device = self
            .active_device()
            .map(|device| device.display_name())
            .unwrap_or_else(|| "无".to_owned());
        ui.horizontal_wrapped(|ui| {
            self.stat_card(
                ui,
                "已连接手柄",
                &self.gamepad.hub.gamepads.len().to_string(),
                "个",
                colors.orange,
                Icon::Gamepad,
            );
            self.stat_card(
                ui,
                "手柄类型",
                self.gamepad.hub.active_kind().label(),
                "",
                colors.blue,
                Icon::Gamepad,
            );
            self.stat_card(
                ui,
                "目标设备",
                &target_device,
                "",
                colors.accent,
                Icon::Device,
            );
            self.stat_card(
                ui,
                "最后动作",
                &util::truncate_chars(&self.gamepad.last_action, 15),
                "",
                colors.green,
                Icon::Activity,
            );
        });

        ui.add_space(14.0);
        self.card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("PC / macOS 手柄桥接").size(15.0).strong());
                    ui.label(
                        RichText::new(
                            "实体手柄与屏幕虚拟手柄通过 UHID 发送原生手柄输入，支持按住、组合键、双摇杆和模拟扳机。",
                        )
                        .size(11.0)
                        .color(colors.muted),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(if self.gamepad.show_virtual {
                                    "隐藏虚拟手柄"
                                } else {
                                    "显示虚拟手柄"
                                })
                                .size(11.5),
                            )
                            .fill(colors.card)
                            .corner_radius(9),
                        )
                        .clicked()
                    {
                        self.gamepad.show_virtual = !self.gamepad.show_virtual;
                    }
                });
            });
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                let labels: Vec<String> = self
                    .gamepad
                    .hub
                    .gamepads
                    .iter()
                    .map(|gamepad| {
                        format!(
                            "{} · {}{}",
                            gamepad.name,
                            gamepad.kind.label(),
                            if gamepad.connected { "" } else { " · 离线" }
                        )
                    })
                    .collect();
                if self.gamepad.selected_index.is_none() {
                    self.gamepad.selected_index = self.gamepad.hub.active.and_then(|active| {
                        self.gamepad
                            .hub
                            .gamepads
                            .iter()
                            .position(|gamepad| gamepad.id == active)
                    });
                }
                let selected = self
                    .gamepad
                    .selected_index
                    .and_then(|index| labels.get(index))
                    .cloned()
                    .unwrap_or_else(|| "未检测到手柄".to_owned());
                egui::ComboBox::from_id_salt("gamepad-select")
                    .selected_text(selected)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        let mut selection = self.gamepad.selected_index;
                        for (index, label) in labels.iter().enumerate() {
                            ui.selectable_value(&mut selection, Some(index), label);
                        }
                        if selection != self.gamepad.selected_index {
                            self.gamepad.selected_index = selection;
                            if let Some(index) = selection {
                                self.gamepad.hub.active =
                                    self.gamepad.hub.gamepads.get(index).map(|gamepad| gamepad.id);
                            }
                        }
                    });
                if ui
                    .add(egui::Button::new("重新扫描").fill(colors.card).corner_radius(9))
                    .clicked()
                {
                    self.gamepad.hub.refresh();
                }
                if let Some(error) = self.gamepad.hub.error.as_ref() {
                    ui.label(RichText::new(error).size(10.0).color(colors.red));
                }
            });
        });

        ui.add_space(14.0);
        if self.gamepad.show_virtual {
            self.card_frame(ui).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("虚拟手柄").size(15.0).strong());
                    let automatic_kind = match self.gamepad.hub.active_kind() {
                        GamepadKind::Generic => GamepadKind::Xbox,
                        kind => kind,
                    };
                    let current_kind = self.gamepad.virtual_kind.unwrap_or(automatic_kind);
                    egui::ComboBox::from_id_salt("virtual-gamepad-kind")
                        .selected_text(current_kind.label())
                        .width(140.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.gamepad.virtual_kind, None, "自动识别");
                            for kind in [
                                GamepadKind::Xbox,
                                GamepadKind::PlayStation,
                                GamepadKind::Nintendo,
                            ] {
                                ui.selectable_value(
                                    &mut self.gamepad.virtual_kind,
                                    Some(kind),
                                    kind.label(),
                                );
                            }
                        });
                    ui.label(
                        RichText::new("外观切换不改变标准 HID 按键位置")
                            .size(10.5)
                            .color(colors.muted),
                    );
                });
                ui.add_space(4.0);
                self.virtual_gamepad(ui);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("死区").size(10.5).color(colors.muted));
                    let mut deadzone = self.config.gamepad.deadzone;
                    if ui
                        .add(
                            egui::Slider::new(&mut deadzone, 0.02..=0.45)
                                .show_value(true)
                                .fixed_decimals(2),
                        )
                        .changed()
                    {
                        self.config.gamepad.deadzone = deadzone;
                        self.mark_config_dirty();
                    }
                });
            });
            ui.add_space(14.0);
        }

        self.card_frame(ui).show(ui, |ui| {
            self.section_title(
                ui,
                "Android 原生手柄",
                "通过 scrcpy UHID 创建标准 HID 游戏手柄",
            );
            ui.label(RichText::new(&self.gamepad.last_action).color(colors.accent));
            ui.label(
                "按键、双摇杆、模拟扳机直接发送到设备。支持 USB 和无线 ADB；游戏需要支持原生手柄。",
            );
            if self.gamepad.enabled {
                if ui.button("断开手柄").clicked() {
                    self.disconnect_gamepad();
                }
            } else if ui.button("连接手柄").clicked() {
                self.connect_gamepad();
            }
        });
    }

    fn page_files(&mut self, ui: &mut Ui) {
        if self.active_serial_online().is_none() {
            self.no_device_panel(ui);
            return;
        }
        ui.horizontal_top(|ui| {
            let left_width = (ui.available_width() * 0.29).clamp(220.0, 300.0);
            ui.allocate_ui_with_layout(
                egui::vec2(left_width, ui.available_height()),
                Layout::top_down(Align::Min),
                |ui| {
                    self.local_files_panel(ui);
                },
            );
            ui.separator();
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), ui.available_height()),
                Layout::top_down(Align::Min),
                |ui| self.remote_files_panel(ui),
            );
        });
    }

    fn local_files_panel(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        self.section_title(ui, "本地文件", "拖入窗口即可添加");
        ui.add_space(4.0);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 76.0), Sense::hover());
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(12),
            Color32::from_rgba_unmultiplied(43, 218, 195, 8),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(12),
            Stroke::new(1.0, theme::alpha(colors.accent, 0.3)),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "将任意文件拖入 AndroidTool\n上传到当前设备目录",
            FontId::new(11.0, egui::FontFamily::Proportional),
            colors.muted,
        );
        if ui
            .add_sized(
                [ui.available_width(), 33.0],
                egui::Button::new("添加文件")
                    .fill(colors.card)
                    .corner_radius(9),
            )
            .clicked()
        {
            if let Some(paths) = rfd::FileDialog::new().pick_files() {
                for path in paths {
                    if !self.files.local_files.contains(&path) {
                        self.files.local_files.push(path);
                    }
                }
            }
        }
        ui.add_space(8.0);
        let mut remove = None;
        ScrollArea::vertical()
            .id_salt("local-files")
            .max_height((ui.available_height() - 90.0).max(100.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.files.local_files.is_empty() {
                    ui.label(
                        RichText::new("还没有本地文件")
                            .size(10.5)
                            .color(colors.muted),
                    );
                }
                for (index, path) in self.files.local_files.iter().enumerate() {
                    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
                    Frame::new()
                        .fill(colors.panel)
                        .stroke(Stroke::new(1.0, colors.border))
                        .corner_radius(9)
                        .inner_margin(Margin::symmetric(9, 7))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(util::truncate_chars(
                                            &util::file_name(path),
                                            28,
                                        ))
                                        .size(10.5),
                                    );
                                    ui.label(
                                        RichText::new(util::format_bytes(size))
                                            .size(9.0)
                                            .color(colors.muted),
                                    );
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new("×")
                                                .fill(Color32::TRANSPARENT)
                                                .stroke(Stroke::NONE),
                                        )
                                        .clicked()
                                    {
                                        remove = Some(index);
                                    }
                                });
                            });
                        });
                }
            });
        if let Some(index) = remove {
            self.files.local_files.remove(index);
        }
        ui.add_space(8.0);
        if ui
            .add_sized(
                [ui.available_width(), 36.0],
                egui::Button::new(
                    RichText::new(format!("上传 {} 个文件", self.files.local_files.len()))
                        .color(colors.bg)
                        .strong(),
                )
                .fill(colors.accent)
                .corner_radius(9),
            )
            .clicked()
        {
            self.upload_local_files();
        }
        if !self.files.status.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new(util::truncate_chars(&self.files.status, 120))
                    .size(9.5)
                    .color(colors.muted),
            );
        }
    }

    fn remote_files_panel(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        self.section_title(ui, "设备文件", "浏览 / 下载 / 导航");
        ui.add_space(4.0);
        let response = ui.add(
            TextEdit::singleline(&mut self.files.remote_path).desired_width(ui.available_width()),
        );
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.load_remote_dir(self.files.remote_path.clone());
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(egui::Button::new("上级").fill(colors.card).corner_radius(8))
                .on_hover_text("上级目录")
                .clicked()
            {
                let parent = util::parent_remote_path(&self.files.remote_path);
                self.load_remote_dir(parent);
            }
            if ui
                .add(egui::Button::new("前往").fill(colors.card).corner_radius(8))
                .clicked()
            {
                let path = util::normalize_remote_path(&self.files.remote_path);
                self.load_remote_dir(path);
            }
            if ui
                .add(egui::Button::new("刷新").fill(colors.card).corner_radius(8))
                .clicked()
            {
                let path = self.files.remote_path.clone();
                self.load_remote_dir(path);
            }
        });
        if let Some(error) = self.files.error.as_ref() {
            ui.label(RichText::new(error).size(10.0).color(colors.red));
        }
        if self.files.loading {
            ui.add(ProgressBar::new(0.35).text("读取设备目录…").animate(true));
        }
        ui.add_space(5.0);
        let mut navigate = None;
        let mut download = None;
        let height = (ui.available_height() - 12.0).max(180.0);
        ScrollArea::both()
            .id_salt("remote-files")
            .auto_shrink([false, false])
            .max_height(height)
            .show(ui, |ui| {
                ui.set_min_width(620.0);
                if self.files.entries.is_empty()
                    && !self.files.loading
                    && self.files.error.is_none()
                {
                    ui.label("此目录为空");
                }
                egui::Grid::new("remote-file-grid")
                    .num_columns(4)
                    .spacing([12.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(RichText::new("名称").size(10.0).color(colors.muted));
                        ui.label(RichText::new("大小").size(10.0).color(colors.muted));
                        ui.label(RichText::new("权限").size(10.0).color(colors.muted));
                        ui.label(RichText::new("修改时间").size(10.0).color(colors.muted));
                        ui.end_row();
                        for (index, entry) in self.files.entries.iter().enumerate() {
                            let name = if entry.is_dir {
                                format!("[D] {}", entry.name)
                            } else {
                                format!("[F] {}", entry.name)
                            };
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(util::truncate_chars(&name, 34))
                                            .size(10.5)
                                            .color(if entry.is_dir {
                                                colors.accent
                                            } else {
                                                colors.text
                                            }),
                                    )
                                    .fill(if self.files.selected_remote == Some(index) {
                                        colors.card_hover
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .stroke(Stroke::NONE),
                                )
                                .clicked()
                            {
                                self.files.selected_remote = Some(index);
                                if entry.is_dir {
                                    navigate = Some(util::join_remote_path(
                                        &self.files.remote_path,
                                        &entry.name,
                                    ));
                                }
                            }
                            ui.label(
                                RichText::new(if entry.is_dir {
                                    "—".to_owned()
                                } else {
                                    util::format_bytes(entry.size)
                                })
                                .size(9.5)
                                .color(colors.muted),
                            );
                            ui.label(
                                RichText::new(&entry.permission)
                                    .monospace()
                                    .size(9.0)
                                    .color(colors.muted),
                            );
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(&entry.modified).size(9.0).color(colors.muted),
                                );
                                if !entry.is_dir && ui.small("下载").on_hover_text("下载").clicked()
                                {
                                    download = Some(util::join_remote_path(
                                        &self.files.remote_path,
                                        &entry.name,
                                    ));
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        if let Some(path) = navigate {
            self.load_remote_dir(path);
        }
        if let Some(remote) = download {
            if let Some(local) = rfd::FileDialog::new()
                .set_file_name(remote.rsplit('/').next().unwrap_or("download"))
                .save_file()
            {
                if let (Some(adb_path), Some(serial)) =
                    (self.adb_path(), self.active_serial_online())
                {
                    self.files.status = "正在下载…".to_owned();
                    self.adb_worker.submit(AdbJob {
                        adb_path,
                        serial,
                        task: AdbTask::Pull { remote, local },
                        label: "PULL".to_owned(),
                    });
                }
            }
        }
    }

    fn load_remote_dir(&mut self, path: String) {
        let path = util::normalize_remote_path(&path);
        if self.active_serial_online().is_none() {
            self.files.error = Some("设备未在线".to_owned());
            return;
        }
        self.files.remote_path = path.clone();
        self.files.entries.clear();
        self.files.selected_remote = None;
        self.files.loading = true;
        self.files.error = None;
        self.queue_shell(
            vec![
                "ls".to_owned(),
                "-lap".to_owned(),
                "--".to_owned(),
                util::quote_remote_arg(&path),
            ],
            format!("LS:{path}"),
        );
    }

    fn upload_local_files(&mut self) {
        let (Some(adb_path), Some(serial)) = (self.adb_path(), self.active_serial_online()) else {
            return;
        };
        if self.files.local_files.is_empty() {
            return;
        }
        let destination = util::normalize_remote_path(&self.files.remote_path);
        self.files.status = "正在上传…".to_owned();
        for local in &self.files.local_files {
            let remote = util::join_remote_path(&destination, &util::file_name(local));
            self.adb_worker.submit(AdbJob {
                adb_path: adb_path.clone(),
                serial: serial.clone(),
                task: AdbTask::Push {
                    local: local.clone(),
                    remote,
                },
                label: "PUSH".to_owned(),
            });
        }
    }

    fn page_settings(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        ui.set_max_width(ui.available_width().min(1100.0));
        self.card_frame(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            self.section_title(ui, "工具链", "便携式路径会优先于系统 PATH");
            ui.add_space(6.0);
            let adb_changed =
                Self::path_setting_row(ui, "ADB", "platform-tools/adb", &mut self.config.adb_path);
            ui.label(
                RichText::new(
                    "ADB 路径留空时优先使用随包附带的 platform-tools；手动指定的路径优先。",
                )
                .size(11.0)
                .color(colors.muted),
            );
            let bundletool_changed = Self::path_setting_row(
                ui,
                "bundletool",
                "bundletool.jar（AAB 转换）",
                &mut self.config.bundletool_path,
            );
            let java_changed = Self::path_setting_row(
                ui,
                "Java",
                "java 可执行文件（Java 17+）",
                &mut self.config.java_path,
            );
            if adb_changed || bundletool_changed || java_changed {
                self.mark_config_dirty();
                if adb_changed {
                    self.watcher = None;
                    self.watcher_path = None;
                }
            }
            ui.add_space(5.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        egui::Button::new("使用内置 ADB")
                            .fill(colors.card)
                            .corner_radius(9),
                    )
                    .clicked()
                {
                    let adb_name = if cfg!(windows) { "adb.exe" } else { "adb" };
                    if util::install_dir()
                        .join("platform-tools")
                        .join(adb_name)
                        .is_file()
                    {
                        self.config.adb_path.clear();
                        self.watcher = None;
                        self.watcher_path = None;
                        self.mark_config_dirty();
                        self.toast("已切换到随包附带的 ADB", true);
                    } else {
                        self.toast("未找到内置 ADB，请先下载 ADB 或使用完整便携包", false);
                    }
                }
                if ui
                    .add(egui::Button::new("自动检测").fill(colors.card).corner_radius(9))
                    .clicked()
                {
                    if let Some(path) = util::detect_adb(&self.config.adb_path) {
                        self.config.adb_path = path
                            .strip_prefix(util::install_dir())
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .into_owned();
                    }
                    if let Some(path) = util::detect_bundletool(&self.config.bundletool_path) {
                        self.config.bundletool_path = util::portable_path(&path);
                    }
                    if let Some(path) = util::detect_java(&self.config.java_path) {
                        self.config.java_path = util::portable_path(&path);
                    }
                    self.mark_config_dirty();
                    self.toast("已重新检测工具链", true);
                }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("下载代理").size(11.0).color(colors.muted));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.config.download_proxy)
                            .hint_text("http://127.0.0.1:7890（留空为直连）")
                            .desired_width(330.0),
                    )
                    .changed()
                {
                    self.mark_config_dirty();
                }
            });
            ui.label(
                RichText::new("使用 HTTP CONNECT / 混合代理端口；仅用于工具下载和版本查询，代理地址保存在配置文件中。")
                    .size(10.0)
                    .color(colors.muted),
            );
            for kind in ToolKind::ALL {
                self.tool_download_row(ui, kind);
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui.link("打开安装目录").clicked() {
                    util::open_folder(&util::install_dir());
                }
                if ui.link("打开 converted/").clicked() {
                    let _ = std::fs::create_dir_all(util::converted_dir());
                    util::open_folder(&util::converted_dir());
                }
            });
            ui.add(
                egui::Label::new(RichText::new(&self.config_status).size(9.5).color(colors.muted)).wrap(),
            );
        });

        ui.add_space(14.0);
        if ui.available_width() >= 900.0 {
            ui.columns(2, |columns| {
                self.settings_performance(&mut columns[0]);
                self.settings_portable(&mut columns[1]);
            });
        } else {
            self.settings_performance(ui);
            ui.add_space(14.0);
            self.settings_portable(ui);
        }
    }

    fn tool_download_row(&mut self, ui: &mut Ui, kind: ToolKind) {
        let colors = theme::palette(ui.ctx());
        let index = kind.index();
        let busy = self.tools.tasks[index].session.is_some();
        let mut refresh = false;
        let mut download = false;
        let mut cancel = false;
        let mut config_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.add_sized(
                [76.0, 32.0],
                egui::Label::new(RichText::new(kind.label()).strong()),
            );
            if kind == ToolKind::Java {
                let before = self.tools.java_major;
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt("java-major")
                        .width(95.0)
                        .selected_text(format!("Java {}", before))
                        .show_ui(ui, |ui| {
                            for major in [17, 21, 25] {
                                ui.selectable_value(
                                    &mut self.tools.java_major,
                                    major,
                                    format!("Java {major}"),
                                );
                            }
                        });
                });
                if before != self.tools.java_major {
                    self.config.java_major = self.tools.java_major;
                    self.tools.tasks[index].versions.clear();
                    self.tools.tasks[index].selected = None;
                    self.config.java_version.clear();
                    config_changed = true;
                }
            }
            if kind != ToolKind::Adb {
                let task = &mut self.tools.tasks[index];
                let label = task
                    .selected
                    .and_then(|i| task.versions.get(i))
                    .map(|v| v.name.as_str())
                    .unwrap_or("请获取版本");
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt(("tool-version", index))
                        .width(200.0)
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            for (i, version) in task.versions.iter().enumerate() {
                                if ui
                                    .selectable_value(&mut task.selected, Some(i), &version.name)
                                    .changed()
                                {
                                    match kind {
                                        ToolKind::Bundletool => {
                                            self.config.bundletool_version = version.name.clone()
                                        }
                                        ToolKind::Java => {
                                            self.config.java_version = version.name.clone()
                                        }
                                        _ => {}
                                    }
                                    config_changed = true;
                                }
                            }
                        });
                });
                refresh = ui
                    .add_enabled(!busy, egui::Button::new("获取版本"))
                    .clicked();
            }
            download = ui.add_enabled(!busy, egui::Button::new("下载")).clicked();
            if busy {
                ui.spinner();
                cancel = ui.button("取消").clicked();
            }
        });
        if config_changed {
            self.mark_config_dirty();
        }
        if cancel {
            let task = &mut self.tools.tasks[index];
            task.session = None;
            task.status = "已取消".into();
            task.progress = None;
        } else if refresh {
            self.start_tool(kind, false);
        } else if download {
            self.start_tool(kind, true);
        }
        let task = &self.tools.tasks[index];
        if let Some(progress) = task.progress {
            ui.add(
                ProgressBar::new(progress.clamp(0.0, 1.0))
                    .desired_width(270.0)
                    .show_percentage(),
            );
        }
        if !task.status.is_empty() {
            ui.label(RichText::new(&task.status).size(10.0).color(colors.muted));
        }
    }

    fn settings_performance(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        self.card_frame(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            self.section_title(ui, "体验与性能", "设置会自动保存到程序安装目录");
            ui.add_space(6.0);
            let before = self.config.theme;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("界面主题").size(11.0).color(colors.muted));
                for mode in theme::ThemeMode::ALL {
                    ui.selectable_value(&mut self.config.theme, mode, mode.label());
                }
            });
            if before != self.config.theme {
                self.mark_config_dirty();
                ui.ctx().request_repaint();
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("投屏帧率").size(11.0).color(colors.muted));
                let mut fps = self.config.screenshot_fps;
                if ui
                    .add(egui::Slider::new(&mut fps, 1..=60).suffix(" FPS"))
                    .changed()
                {
                    self.config.screenshot_fps = fps;
                    self.mark_config_dirty();
                }
            });
            ui.label(
                RichText::new("仅用于内嵌截图兼容模式；高性能投屏请在投屏页调整 scrcpy 设置。")
                    .size(9.5)
                    .color(colors.muted),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("日志上限").size(11.0).color(colors.muted));
                let mut max_logs = self.config.max_log_lines;
                if ui
                    .add(
                        egui::Slider::new(&mut max_logs, 2_000..=100_000)
                            .step_by(1000.0)
                            .suffix(" 行"),
                    )
                    .changed()
                {
                    self.config.max_log_lines = max_logs;
                    while self.log.entries.len() > max_logs {
                        self.log.entries.pop_front();
                    }
                    self.log.filter_dirty = true;
                    self.mark_config_dirty();
                }
            });
            let mut auto_scroll = self.config.log_auto_scroll;
            if ui
                .checkbox(&mut auto_scroll, "Logcat 自动滚动到最新日志")
                .changed()
            {
                self.config.log_auto_scroll = auto_scroll;
                self.mark_config_dirty();
            }
            let mut auto_install = self.config.auto_install_drops;
            if ui
                .checkbox(&mut auto_install, "拖入 APK / AAB 时自动加入安装器")
                .changed()
            {
                self.config.auto_install_drops = auto_install;
                self.mark_config_dirty();
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("立即保存配置").color(colors.bg))
                            .fill(colors.accent)
                            .corner_radius(9),
                    )
                    .clicked()
                {
                    self.save_config();
                }
                if ui
                    .add(
                        egui::Button::new("重新加载")
                            .fill(colors.card)
                            .corner_radius(9),
                    )
                    .clicked()
                {
                    self.config = AppConfig::load();
                    self.tools.java_major = if [17, 21, 25].contains(&self.config.java_major) {
                        self.config.java_major
                    } else {
                        17
                    };
                    for task in &mut self.tools.tasks {
                        if task.session.is_none() {
                            task.versions.clear();
                            task.selected = None;
                        }
                    }
                    self.config_dirty = false;
                    self.watcher = None;
                    self.watcher_path = None;
                    self.toast("已重新加载配置", true);
                }
            });
        });
    }

    fn settings_portable(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        self.card_frame(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            self.section_title(ui, "便携配置", "配置文件与程序放在一起");
            ui.add_space(5.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| { ui.add(
                egui::Label::new(RichText::new(util::settings_path().display().to_string())
                    .size(10.5)
                    .color(colors.accent))
                .wrap(),
            ); });
            ui.label(
                RichText::new(
                    "Windows 与 macOS 均使用安装目录中的 androidtool.settings.json。转换产物写入 converted/，AAB 临时文件写入 cache/。macOS 若将 .app 安装到只读位置，请将程序放入可写目录。",
                )
                .size(10.0)
                .color(colors.muted),
            );
            if ui.link("在文件管理器中显示配置").clicked() {
                util::reveal_file(&util::settings_path());
            }
        });
    }

    fn path_setting_row(ui: &mut Ui, label: &str, hint: &str, value: &mut String) -> bool {
        let colors = theme::palette(ui.ctx());
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.add_sized(
                [62.0, 32.0],
                egui::Label::new(RichText::new(label).size(11.0).strong().color(colors.muted)),
            );
            let spacing = ui.spacing().item_spacing.x;
            let input_width = (ui.available_width() - 38.0 - 70.0 - 3.0 * spacing).max(40.0);
            let response = ui.add_sized(
                [input_width, 32.0],
                TextEdit::singleline(value).hint_text(hint),
            );
            if response.changed() {
                changed = true;
            }
            let found = if label == "bundletool" {
                util::detect_bundletool(value)
            } else if label == "Java" {
                util::detect_java(value)
            } else {
                util::detect_adb(value)
            };
            ui.add_sized(
                [38.0, 32.0],
                egui::Label::new(
                    RichText::new(if found.is_some() { "正常" } else { "缺失" })
                        .size(9.5)
                        .strong()
                        .color(if found.is_some() {
                            colors.green
                        } else {
                            colors.red
                        }),
                ),
            );
            if ui
                .add_sized(
                    [70.0, 32.0],
                    egui::Button::new("浏览").fill(colors.card).corner_radius(8),
                )
                .clicked()
            {
                let dialog = rfd::FileDialog::new();
                let picked = if label == "bundletool" {
                    dialog.add_filter("JAR", &["jar"]).pick_file()
                } else {
                    dialog.pick_file()
                };
                if let Some(path) = picked {
                    *value = path.to_string_lossy().into_owned();
                    changed = true;
                }
            }
        });
        changed
    }

    fn no_device_panel(&mut self, ui: &mut Ui) {
        let colors = theme::palette(ui.ctx());
        self.card_frame(ui).show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 260.0),
                Layout::top_down(Align::Center),
                |ui| {
                    ui.add_space(28.0);
                    let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
                    paint_icon(
                        ui.painter(),
                        icon_rect.shrink(6.0),
                        Icon::Device,
                        colors.muted,
                    );
                    ui.label(RichText::new("没有在线设备").size(17.0).strong());
                    ui.label(
                        RichText::new("连接 Android 设备并确认 USB 调试授权后重试")
                            .size(11.5)
                            .color(colors.muted),
                    );
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("重新扫描").color(colors.bg))
                                .fill(colors.accent)
                                .corner_radius(9),
                        )
                        .clicked()
                    {
                        self.refresh_devices_now();
                    }
                },
            );
        });
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(Id::new("toast-area"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-22.0, -22.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::RIGHT), |ui| {
                    for toast in self.toasts.iter().rev() {
                        let age = toast.created.elapsed().as_secs_f32();
                        let alpha = if age > 3.5 {
                            ((4.0 - age) / 0.5).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };
                        let color = if toast.success {
                            colors.green
                        } else {
                            colors.red
                        };
                        Frame::new()
                            .fill(theme::alpha(colors.panel, alpha))
                            .stroke(Stroke::new(1.0, theme::alpha(color, alpha * 0.55)))
                            .corner_radius(12)
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(if toast.success { "●" } else { "!" })
                                            .color(theme::alpha(color, alpha)),
                                    );
                                    ui.label(
                                        RichText::new(&toast.text)
                                            .size(11.5)
                                            .color(theme::alpha(colors.text, alpha)),
                                    );
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
            });
    }

    fn show_wireless_modal(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        if !self.wireless.open {
            return;
        }

        let modal = egui::Modal::new(Id::new("wireless-pairing-modal"))
            .backdrop_color(Color32::from_black_alpha(185))
            .frame(
                Frame::new()
                    .fill(colors.panel)
                    .stroke(Stroke::new(1.0, colors.border))
                    .corner_radius(18)
                    .inner_margin(Margin::same(22)),
            )
            .show(ctx, |ui| {
                ui.set_width(570.0);
                let mut close = false;
                ui.horizontal_top(|ui| {
                    let icon_rect = Rect::from_min_size(
                        ui.cursor().min + egui::vec2(0.0, 1.0),
                        Vec2::splat(42.0),
                    );
                    ui.painter().rect_filled(
                        icon_rect,
                        CornerRadius::same(12),
                        Color32::from_rgba_unmultiplied(43, 218, 195, 20),
                    );
                    paint_icon(ui.painter(), icon_rect.shrink(10.0), Icon::Link, colors.accent);
                    ui.add_space(54.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("连接无线 Android 设备").size(20.0).strong());
                        ui.label(
                            RichText::new("通过 IP 端口或 Android 无线调试二维码完成 ADB 配对")
                                .size(11.5)
                                .color(colors.muted),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("关闭").size(11.0))
                                    .fill(colors.card)
                                    .stroke(Stroke::new(1.0, colors.border))
                                    .corner_radius(9),
                            )
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });

                ui.add_space(16.0);
                let mut tab = self.wireless.tab;
                ui.horizontal(|ui| {
                    if ui
                        .selectable_value(&mut tab, WirelessTab::Ip, "IP 端口配对")
                        .clicked()
                    {
                        self.wireless.error = None;
                    }
                    if ui
                        .selectable_value(&mut tab, WirelessTab::Qr, "二维码配对")
                        .clicked()
                    {
                        self.wireless.error = None;
                    }
                });
                if tab != self.wireless.tab {
                    self.stop_wireless_pairing();
                    self.wireless.tab = tab;
                    if tab == WirelessTab::Qr {
                        self.start_qr_wireless();
                    } else {
                        self.wireless.status = "输入配对弹窗中的地址、配对端口和 6 位配对码；连接端口留空可自动发现。".to_owned();
                    }
                }

                ui.add_space(8.0);
                match self.wireless.tab {
                    WirelessTab::Ip => {
                        ui.label(
                            RichText::new("在手机“无线调试”页面选择“使用配对码配对设备”。配对端口来自该弹窗；连接端口来自无线调试主页，两者不同。")
                                .size(11.0)
                                .color(colors.muted)
                                .line_height(Some(17.0)),
                        );
                        ui.add_space(10.0);
                        ui.add_enabled_ui(!self.wireless.busy, |ui| {
                            egui::Grid::new("wireless-ip-form")
                                .num_columns(2)
                                .spacing([16.0, 10.0])
                                .show(ui, |ui| {
                                    ui.label(form_label(ui, "IP 地址 / 主机名"));
                                    ui.add(
                                        TextEdit::singleline(&mut self.wireless.host)
                                            .hint_text("IP 或 IP:端口，例如 192.168.1.23:37123")
                                            .desired_width(330.0),
                                    );
                                    ui.end_row();

                                    ui.label(form_label(ui, "无线调试连接端口"));
                                    ui.add(
                                        TextEdit::singleline(&mut self.wireless.connect_port)
                                            .hint_text("留空自动发现；仅连接时必填")
                                            .desired_width(250.0),
                                    );
                                    ui.end_row();

                                    ui.label(form_label(ui, "配对端口"));
                                    ui.add(
                                        TextEdit::singleline(&mut self.wireless.pair_port)
                                            .hint_text("手机配对页面显示的端口")
                                            .desired_width(180.0),
                                    );
                                    ui.end_row();

                                    ui.label(form_label(ui, "6 位配对码"));
                                    ui.add(
                                        TextEdit::singleline(&mut self.wireless.pairing_code)
                                            .hint_text("000000")
                                            .char_limit(6)
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(110.0),
                                    );
                                    ui.end_row();
                                });
                        });
                        ui.add_space(14.0);
                        if self.wireless.busy {
                            if ui
                                .add(
                                    egui::Button::new("停止当前操作")
                                        .fill(colors.card)
                                        .stroke(Stroke::new(1.0, colors.border))
                                        .corner_radius(9),
                                )
                                .clicked()
                            {
                                self.stop_wireless_pairing();
                            }
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("配对并连接").strong().color(colors.bg),
                                        )
                                        .fill(colors.accent)
                                        .stroke(Stroke::NONE)
                                        .corner_radius(9),
                                    )
                                    .clicked()
                                {
                                    self.start_manual_wireless(
                                        ManualWirelessMode::PairAndConnect,
                                    );
                                }
                                if ui
                                    .add(
                                        egui::Button::new("仅配对")
                                            .fill(colors.card)
                                            .stroke(Stroke::new(1.0, colors.border))
                                            .corner_radius(9),
                                    )
                                    .clicked()
                                {
                                    self.start_manual_wireless(ManualWirelessMode::PairOnly);
                                }
                                if ui
                                    .add(
                                        egui::Button::new("仅连接")
                                            .fill(colors.card)
                                            .stroke(Stroke::new(1.0, colors.border))
                                            .corner_radius(9),
                                    )
                                    .clicked()
                                {
                                    self.start_manual_wireless(
                                        ManualWirelessMode::ConnectOnly,
                                    );
                                }
                            });
                        }
                    }
                    WirelessTab::Qr => {
                        ui.label(
                            RichText::new("手机进入“无线调试 → 使用二维码配对设备”，扫描下方二维码。电脑会自动发现配对服务并建立 ADB 连接。")
                                .size(11.0)
                                .color(colors.muted)
                                .line_height(Some(17.0)),
                        );
                        ui.add_space(10.0);
                        let pairing = self.wireless.qr.clone();
                        ui.horizontal_top(|ui| {
                            if let Some(pairing) = pairing.as_ref() {
                                match pairing.matrix() {
                                    Ok(matrix) => draw_qr_code(ui, &matrix),
                                    Err(error) => {
                                        ui.colored_label(colors.red, error);
                                    }
                                }
                            }
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new("扫码步骤").size(13.0).strong());
                                numbered_hint(ui, "1", "让手机和电脑连接同一个局域网。");
                                numbered_hint(ui, "2", "打开开发者选项中的“无线调试”。");
                                numbered_hint(ui, "3", "选择“使用二维码配对设备”并扫描。");
                                numbered_hint(ui, "4", "保持此窗口打开，等待连接成功。");
                                if let Some(pairing) = pairing.as_ref() {
                                    ui.add_space(8.0);
                                    Frame::new()
                                        .fill(colors.bg_soft)
                                        .stroke(Stroke::new(1.0, colors.border))
                                        .corner_radius(10)
                                        .inner_margin(Margin::symmetric(12, 9))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(format!(
                                                    "配对码  {}",
                                                    pairing.code
                                                ))
                                                .monospace()
                                                .size(12.0)
                                                .color(colors.accent),
                                            );
                                            ui.label(
                                                RichText::new(&pairing.service_name)
                                                    .monospace()
                                                    .size(9.5)
                                                    .color(colors.muted),
                                            );
                                        });
                                }
                            });
                        });
                        ui.add_space(12.0);
                        ui.horizontal_wrapped(|ui| {
                            if self.wireless.busy {
                                if ui
                                    .add(
                                        egui::Button::new("停止等待")
                                            .fill(colors.card)
                                            .stroke(Stroke::new(1.0, colors.border))
                                            .corner_radius(9),
                                    )
                                    .clicked()
                                {
                                    self.stop_wireless_pairing();
                                }
                            } else if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("重新等待扫描").strong().color(colors.bg),
                                    )
                                    .fill(colors.accent)
                                    .stroke(Stroke::NONE)
                                    .corner_radius(9),
                                )
                                .clicked()
                            {
                                self.start_qr_wireless();
                            }
                            if ui
                                .add(
                                    egui::Button::new("刷新二维码")
                                        .fill(colors.card)
                                        .stroke(Stroke::new(1.0, colors.border))
                                        .corner_radius(9),
                                )
                                .clicked()
                            {
                                self.regenerate_qr_pairing();
                            }
                        });
                    }
                }

                ui.add_space(10.0);
                ui.checkbox(
                    &mut self.wireless.confirm_adb_restart,
                    "允许重启 ADB（会断开所有工具的调试连接；请先停止等待）",
                );
                if ui
                    .add_enabled(
                        self.wireless.confirm_adb_restart && !self.wireless.busy,
                        egui::Button::new("使用当前 ADB 重启后台服务"),
                    )
                    .clicked()
                {
                    self.wireless.confirm_adb_restart = false;
                    if let Some(path) = self.config.resolved_adb() {
                        self.wireless.session = Some(WirelessSession::restart_adb(path));
                        self.wireless.busy = true;
                        self.wireless.error = None;
                        self.wireless.status = "正在重启 ADB 服务并检查 mDNS…".to_owned();
                    } else {
                        self.wireless.error = Some("未找到 ADB，请先检查工具路径。".to_owned());
                    }
                }
                ui.add_space(14.0);
                let status_color = if self.wireless.error.is_some() {
                    colors.red
                } else if self.wireless.busy {
                    colors.accent
                } else {
                    colors.muted
                };
                Frame::new()
                    .fill(theme::alpha(status_color, 0.06))
                    .stroke(Stroke::new(1.0, theme::alpha(status_color, 0.28)))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if self.wireless.busy {
                                ui.add(egui::Spinner::new().size(15.0).color(colors.accent));
                            } else {
                                paint_icon(
                                    ui.painter(),
                                    Rect::from_center_size(
                                        ui.cursor().left_center() + egui::vec2(7.0, 0.0),
                                        Vec2::splat(14.0),
                                    ),
                                    if self.wireless.error.is_some() {
                                        Icon::Warning
                                    } else {
                                        Icon::Activity
                                    },
                                    status_color,
                                );
                                ui.add_space(18.0);
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(self.wireless.error.as_ref().unwrap_or(&self.wireless.status))
                                        .size(11.0)
                                        .color(status_color),
                                )
                                .wrap(),
                            );
                        });
                    });
                close
            });

        if modal.should_close() || modal.inner {
            self.close_wireless_pairing();
        }
    }

    fn show_drop_overlay(&self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let hovering = ctx.input(|input| !input.raw.hovered_files.is_empty());
        let alpha = ctx.animate_bool_with_time(Id::new("drop-overlay"), hovering, 0.14);
        if alpha <= 0.001 {
            return;
        }
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            Order::Foreground,
            Id::new("drop-overlay-layer"),
        ));
        painter.rect_filled(rect, 0.0, theme::alpha(colors.bg, alpha * 0.90));
        let inner = rect.shrink(36.0);
        painter.rect_filled(
            inner,
            CornerRadius::same(24),
            Color32::from_rgba_unmultiplied(43, 218, 195, (alpha * 14.0) as u8),
        );
        painter.rect_stroke(
            inner,
            CornerRadius::same(24),
            Stroke::new(2.0, theme::alpha(colors.accent, alpha)),
            egui::StrokeKind::Inside,
        );
        paint_icon(
            &painter,
            Rect::from_center_size(inner.center() - egui::vec2(0.0, 34.0), Vec2::splat(48.0)),
            Icon::Package,
            theme::alpha(colors.accent, alpha),
        );
        painter.text(
            inner.center() + egui::vec2(0.0, 34.0),
            Align2::CENTER_CENTER,
            "释放文件以加入 AndroidTool",
            FontId::new(20.0, egui::FontFamily::Proportional),
            theme::alpha(colors.text, alpha),
        );
        painter.text(
            inner.center() + egui::vec2(0.0, 62.0),
            Align2::CENTER_CENTER,
            "APK / AAB / APKS 自动进入安装器，其它文件进入文件工作区",
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme::alpha(colors.muted, alpha),
        );
    }
}

impl eframe::App for AndroidToolApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.config.theme.apply(ctx);
        let colors = theme::palette(ctx);
        self.screen.mirror_rect = None;
        self.screen.canvas_rect = None;
        self.tick(ctx);
        self.paint_background(ctx);

        egui::SidePanel::left("sidebar")
            .exact_width(228.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(colors.sidebar)
                    .stroke(Stroke::new(1.0, colors.border))
                    .inner_margin(Margin::symmetric(15, 18)),
            )
            .show(ctx, |ui| self.sidebar(ui));

        egui::TopBottomPanel::top("device-navigation")
            .exact_height(82.0)
            .frame(
                Frame::new()
                    .fill(colors.sidebar)
                    .stroke(Stroke::new(1.0, colors.border))
                    .inner_margin(Margin::symmetric(22, 12)),
            )
            .show(ctx, |ui| self.topbar(ui));

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| {
                let fade = (self.page_changed_at.elapsed().as_secs_f32() / 0.16).clamp(0.0, 1.0);
                ui.set_opacity(fade);
                match self.page {
                    Page::Overview => {
                        ScrollArea::vertical()
                            .id_salt("overview-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.page_overview(ui));
                    }
                    Page::Screen => self.page_screen(ui),
                    Page::Logcat => self.page_logcat(ui),
                    Page::Installer => {
                        ScrollArea::vertical()
                            .id_salt("installer-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.page_installer(ui));
                    }
                    Page::Gamepad => {
                        ScrollArea::vertical()
                            .id_salt("gamepad-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.page_gamepad(ui));
                    }
                    Page::Files => self.page_files(ui),
                    Page::Settings => {
                        ScrollArea::vertical()
                            .id_salt("settings-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.page_settings(ui));
                    }
                }
            });

        self.show_gamepad_overlay(ctx);
        self.send_gamepad_state();
        self.show_wireless_modal(ctx);
        self.show_drop_overlay(ctx);
        self.show_toasts(ctx);
        #[cfg(windows)]
        self.sync_embedded_mirror(ctx);
    }
}

fn device_picker_card(
    ui: &mut Ui,
    device: &DeviceInfo,
    selected: bool,
    width: f32,
) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 88.0), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        12,
        if selected {
            theme::mix(colors.panel, colors.accent_dark, 0.25)
        } else if response.hovered() {
            colors.card_hover
        } else {
            colors.panel
        },
    );
    painter.rect_stroke(
        rect,
        12,
        Stroke::new(
            1.0,
            if selected {
                colors.accent
            } else {
                colors.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    let color = if device.is_online() {
        colors.green
    } else {
        colors.orange
    };
    paint_icon(
        &painter,
        Rect::from_min_size(rect.min + egui::vec2(14.0, 14.0), Vec2::splat(20.0)),
        Icon::Device,
        color,
    );
    let title_rect = Rect::from_min_max(
        rect.min + egui::vec2(44.0, 10.0),
        egui::pos2(rect.right() - 54.0, rect.top() + 34.0),
    );
    ui.painter_at(title_rect).text(
        title_rect.left_center(),
        Align2::LEFT_CENTER,
        device.display_name(),
        FontId::proportional(13.0),
        colors.text,
    );
    if selected {
        painter.text(
            rect.right_top() + egui::vec2(-12.0, 22.0),
            Align2::RIGHT_CENTER,
            "当前",
            FontId::proportional(10.0),
            colors.accent,
        );
    }
    painter.text(
        rect.min + egui::vec2(14.0, 45.0),
        Align2::LEFT_CENTER,
        format!("{} · {}", device.state_label(), device.connection),
        FontId::proportional(10.0),
        color,
    );
    let serial_rect = Rect::from_min_max(
        rect.min + egui::vec2(14.0, 58.0),
        rect.max - egui::vec2(12.0, 6.0),
    );
    ui.painter_at(serial_rect).text(
        serial_rect.left_center(),
        Align2::LEFT_CENTER,
        &device.serial,
        FontId::monospace(9.5),
        colors.muted,
    );
    response.on_hover_text(&device.serial)
}

fn gamepad_overlay_rect(area: Rect, size: Vec2, position: egui::Pos2) -> Rect {
    let travel = (area.size() - size - Vec2::splat(16.0)).max(Vec2::ZERO);
    Rect::from_min_size(
        area.min
            + egui::vec2(8.0, 8.0)
            + position.to_vec2().clamp(Vec2::ZERO, Vec2::splat(1.0)) * travel,
        size,
    )
}

fn form_label(ui: &Ui, text: &str) -> RichText {
    let colors = theme::palette(ui.ctx());
    RichText::new(text).size(11.0).strong().color(colors.muted)
}

fn numbered_hint(ui: &mut Ui, number: &str, text: &str) {
    let colors = theme::palette(ui.ctx());
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 9.0, colors.accent);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            number,
            FontId::new(9.0, egui::FontFamily::Proportional),
            colors.bg,
        );
        ui.add(
            egui::Label::new(
                RichText::new(text)
                    .size(10.5)
                    .color(colors.muted)
                    .line_height(Some(17.0)),
            )
            .wrap(),
        );
    });
}

fn draw_qr_code(ui: &mut Ui, matrix: &QrMatrix) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(218.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(14), Color32::WHITE);
    let quiet_zone = 4usize;
    let modules_with_quiet_zone = matrix.size + quiet_zone * 2;
    let cell = (rect.width() - 16.0) / modules_with_quiet_zone as f32;
    let origin = rect.left_top() + egui::vec2(8.0, 8.0) + Vec2::splat(cell * quiet_zone as f32);
    for y in 0..matrix.size {
        for x in 0..matrix.size {
            if matrix.modules[y * matrix.size + x] {
                let min = origin + egui::vec2(x as f32 * cell, y as f32 * cell);
                ui.painter().rect_filled(
                    Rect::from_min_size(min, Vec2::splat(cell + 0.15)),
                    0.0,
                    Color32::BLACK,
                );
            }
        }
    }
}

fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let center = rect.center();
    let size = rect.width().min(rect.height());
    let stroke = Stroke::new((size * 0.085).clamp(1.25, 1.8), color);
    let thin = Stroke::new((size * 0.065).clamp(1.0, 1.35), color);
    let radius = CornerRadius::same((size * 0.10).round().clamp(1.0, 4.0) as u8);

    match icon {
        Icon::Overview => {
            let cell = size * 0.28;
            let gap = size * 0.09;
            for x in [-1.0, 1.0] {
                for y in [-1.0, 1.0] {
                    let cell_center =
                        center + egui::vec2(x * (cell + gap) / 2.0, y * (cell + gap) / 2.0);
                    painter.rect_filled(
                        Rect::from_center_size(cell_center, Vec2::splat(cell)),
                        radius,
                        color,
                    );
                }
            }
        }
        Icon::Screen => {
            let screen = Rect::from_center_size(
                center - egui::vec2(0.0, size * 0.05),
                egui::vec2(size * 0.70, size * 0.48),
            );
            painter.rect_stroke(screen, radius, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    egui::pos2(center.x, screen.bottom()),
                    egui::pos2(center.x, center.y + size * 0.31),
                ],
                thin,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-size * 0.17, size * 0.31),
                    center + egui::vec2(size * 0.17, size * 0.31),
                ],
                thin,
            );
        }
        Icon::Terminal => {
            let terminal = Rect::from_center_size(center, egui::vec2(size * 0.72, size * 0.58));
            painter.rect_stroke(terminal, radius, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    terminal.left_top() + egui::vec2(size * 0.14, size * 0.18),
                    terminal.left_top() + egui::vec2(size * 0.24, size * 0.28),
                ],
                thin,
            );
            painter.line_segment(
                [
                    terminal.left_top() + egui::vec2(size * 0.24, size * 0.28),
                    terminal.left_top() + egui::vec2(size * 0.14, size * 0.38),
                ],
                thin,
            );
            painter.line_segment(
                [
                    terminal.left_top() + egui::vec2(size * 0.34, size * 0.41),
                    terminal.left_top() + egui::vec2(size * 0.52, size * 0.41),
                ],
                thin,
            );
        }
        Icon::Package => {
            let body = Rect::from_center_size(
                center + egui::vec2(0.0, size * 0.04),
                egui::vec2(size * 0.57, size * 0.51),
            );
            painter.rect_stroke(body, radius, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    body.left_top() + egui::vec2(size * 0.18, 0.0),
                    body.center_top() + egui::vec2(-size * 0.03, size * 0.14),
                ],
                thin,
            );
            painter.line_segment(
                [
                    body.right_top() + egui::vec2(-size * 0.18, 0.0),
                    body.center_top() + egui::vec2(size * 0.03, size * 0.14),
                ],
                thin,
            );
            painter.line_segment(
                [
                    body.center_top() + egui::vec2(-size * 0.03, size * 0.14),
                    body.center_top() + egui::vec2(-size * 0.03, body.height()),
                ],
                thin,
            );
            painter.line_segment(
                [
                    body.center_top() + egui::vec2(size * 0.03, size * 0.14),
                    body.center_top() + egui::vec2(size * 0.03, body.height()),
                ],
                thin,
            );
        }
        Icon::Gamepad => {
            let pad = Rect::from_center_size(center, egui::vec2(size * 0.76, size * 0.42));
            painter.rect_stroke(
                pad,
                CornerRadius::same((size * 0.17).round() as u8),
                stroke,
                egui::StrokeKind::Inside,
            );
            let left = pad.left_center() + egui::vec2(size * 0.19, 0.0);
            painter.line_segment(
                [
                    left - egui::vec2(size * 0.07, 0.0),
                    left + egui::vec2(size * 0.07, 0.0),
                ],
                thin,
            );
            painter.line_segment(
                [
                    left - egui::vec2(0.0, size * 0.07),
                    left + egui::vec2(0.0, size * 0.07),
                ],
                thin,
            );
            let right = pad.right_center() - egui::vec2(size * 0.19, 0.0);
            painter.circle_filled(
                right + egui::vec2(size * 0.06, -size * 0.04),
                size * 0.035,
                color,
            );
            painter.circle_filled(
                right + egui::vec2(size * 0.13, size * 0.04),
                size * 0.035,
                color,
            );
        }
        Icon::Folder => {
            let body = Rect::from_center_size(
                center + egui::vec2(0.0, size * 0.05),
                egui::vec2(size * 0.70, size * 0.48),
            );
            painter.rect_stroke(body, radius, stroke, egui::StrokeKind::Inside);
            let tab = Rect::from_min_size(
                body.left_top() - egui::vec2(0.0, size * 0.12),
                egui::vec2(size * 0.28, size * 0.14),
            );
            painter.line_segment([tab.left_bottom(), tab.right_bottom()], stroke);
            painter.line_segment([tab.left_top(), tab.left_bottom()], stroke);
            painter.line_segment([tab.left_top(), tab.right_top()], stroke);
            painter.line_segment([tab.right_top(), tab.right_bottom()], stroke);
        }
        Icon::Sliders => {
            for (index, offset) in [-0.20_f32, 0.0, 0.20].into_iter().enumerate() {
                let y = center.y + size * offset;
                painter.line_segment(
                    [
                        egui::pos2(center.x - size * 0.33, y),
                        egui::pos2(center.x + size * 0.33, y),
                    ],
                    thin,
                );
                let knob_x = center.x + size * [-0.14, 0.12, -0.02][index];
                painter.circle_filled(egui::pos2(knob_x, y), size * 0.075, color);
            }
        }
        Icon::Device => {
            let phone = Rect::from_center_size(center, egui::vec2(size * 0.40, size * 0.70));
            painter.rect_stroke(
                phone,
                CornerRadius::same((size * 0.09).round() as u8),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    phone.center_top() + egui::vec2(-size * 0.06, size * 0.09),
                    phone.center_top() + egui::vec2(size * 0.06, size * 0.09),
                ],
                thin,
            );
            painter.circle_filled(
                phone.center_bottom() - egui::vec2(0.0, size * 0.08),
                size * 0.035,
                color,
            );
        }
        Icon::Warning => {
            painter.add(egui::Shape::closed_line(
                vec![
                    center + egui::vec2(0.0, -size * 0.34),
                    center + egui::vec2(size * 0.36, size * 0.28),
                    center + egui::vec2(-size * 0.36, size * 0.28),
                ],
                stroke,
            ));
            painter.line_segment(
                [
                    center + egui::vec2(0.0, -size * 0.15),
                    center + egui::vec2(0.0, size * 0.10),
                ],
                thin,
            );
            painter.circle_filled(center + egui::vec2(0.0, size * 0.20), size * 0.035, color);
        }
        Icon::Link => {
            let left = center - egui::vec2(size * 0.16, 0.0);
            let right = center + egui::vec2(size * 0.16, 0.0);
            painter.circle_stroke(left, size * 0.18, stroke);
            painter.circle_stroke(right, size * 0.18, stroke);
            painter.line_segment(
                [
                    left + egui::vec2(size * 0.16, 0.0),
                    right - egui::vec2(size * 0.16, 0.0),
                ],
                thin,
            );
        }
        Icon::Activity => {
            painter.add(egui::Shape::line(
                vec![
                    center + egui::vec2(-size * 0.36, 0.0),
                    center + egui::vec2(-size * 0.16, 0.0),
                    center + egui::vec2(-size * 0.04, -size * 0.25),
                    center + egui::vec2(size * 0.10, size * 0.25),
                    center + egui::vec2(size * 0.22, 0.0),
                    center + egui::vec2(size * 0.36, 0.0),
                ],
                stroke,
            ));
        }
        Icon::Code => {
            painter.line_segment(
                [
                    center + egui::vec2(-size * 0.13, -size * 0.20),
                    center + egui::vec2(-size * 0.31, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-size * 0.31, 0.0),
                    center + egui::vec2(-size * 0.13, size * 0.20),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(size * 0.13, -size * 0.20),
                    center + egui::vec2(size * 0.31, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(size * 0.31, 0.0),
                    center + egui::vec2(size * 0.13, size * 0.20),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(size * 0.07, -size * 0.29),
                    center + egui::vec2(-size * 0.07, size * 0.29),
                ],
                thin,
            );
        }
    }
}

fn control_button(ui: &mut Ui, icon: &str, tooltip: &str) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [68.0, 36.0],
        egui::Button::new(RichText::new(icon).size(11.0))
            .fill(colors.card)
            .stroke(Stroke::new(1.0, colors.border))
            .corner_radius(9),
    )
    .on_hover_text(tooltip)
}

fn normalized_screen_point(pos: egui::Pos2, rect: Rect) -> (f32, f32) {
    (
        ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
    )
}

const LOG_COLUMNS: [f32; 5] = [0.0, 142.0, 174.0, 232.0, 414.0];

fn paint_log_cells(ui: &Ui, rect: Rect, values: [&str; 5], colors: [Color32; 5]) {
    for column in 0..5 {
        let left = rect.left() + LOG_COLUMNS[column];
        let right = if column == 4 {
            rect.right()
        } else {
            rect.left() + LOG_COLUMNS[column + 1]
        };
        let cell = Rect::from_min_max(
            egui::pos2(left + 4.0, rect.top()),
            egui::pos2(right - 6.0, rect.bottom()),
        );
        if cell.width() <= 0.0 {
            continue;
        }
        let painter = ui.painter_at(cell);
        let mut job = egui::text::LayoutJob::simple(
            values[column].to_owned(),
            FontId::monospace(10.0),
            colors[column],
            cell.width(),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(cell.width());
        let galley = painter.layout_job(job);
        painter.galley(
            cell.left_center() - egui::vec2(0.0, galley.size().y / 2.0),
            galley,
            colors[column],
        );
    }
}

fn log_header(ui: &mut Ui) {
    let colors = theme::palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
    paint_log_cells(
        ui,
        rect,
        ["时间", "级", "PID", "TAG", "消息"],
        [colors.muted; 5],
    );
}

fn log_row(ui: &mut Ui, entry: &LogEntry, row: usize) {
    let colors = theme::palette(ui.ctx());
    let color = match entry.level {
        LogLevel::Verbose => colors.muted,
        LogLevel::Debug => colors.blue,
        LogLevel::Info => colors.green,
        LogLevel::Warn => colors.orange,
        LogLevel::Error | LogLevel::Fatal | LogLevel::Assert => colors.red,
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
    if row % 2 != 0 {
        ui.painter_at(rect)
            .rect_filled(rect, 0.0, theme::alpha(colors.text, 0.025));
    }
    paint_log_cells(
        ui,
        rect,
        [
            &entry.timestamp,
            &entry.level.letter().to_string(),
            &entry.pid,
            &entry.tag,
            &entry.message,
        ],
        [colors.muted, color, colors.muted, color, colors.text],
    );
    response.on_hover_ui(|ui| {
        ui.set_max_width(700.0);
        ui.label(
            RichText::new(format!(
                "{}  PID {}  {}",
                entry.process, entry.pid, entry.tag
            ))
            .color(color),
        );
        ui.label(RichText::new(&entry.message).monospace());
    });
}

fn parse_wm_size(text: &str) -> Option<[usize; 2]> {
    for line in text.lines() {
        let (_, size) = line.split_once(':')?;
        let (width, height) = size.trim().split_once('x')?;
        if let (Ok(width), Ok(height)) = (width.trim().parse(), height.trim().parse()) {
            return Some([width, height]);
        }
    }
    None
}

fn parse_stick_values(text: &str) -> Option<(f32, f32)> {
    let (x, y) = text.split_once(':')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

fn parse_remote_entries(text: &str) -> Vec<RemoteEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with("total ") {
            continue;
        }
        let mut fields = line;
        let mut parts = Vec::new();
        for _ in 0..7 {
            fields = fields.trim_start();
            let end = fields.find(char::is_whitespace).unwrap_or(fields.len());
            parts.push(&fields[..end]);
            fields = &fields[end..];
        }
        let name = fields.trim_start();
        if parts.len() < 7 || name.is_empty() {
            continue;
        }
        let name = if parts[0].starts_with('l') {
            name.split(" -> ").next().unwrap_or(name)
        } else {
            name
        };
        let name = name.to_owned();
        if name == "." || name == ".." {
            continue;
        }
        let permission = parts[0].to_owned();
        let is_dir = permission.starts_with('d') || name.ends_with('/');
        let size = parts[4].parse().unwrap_or(0);
        let modified = format!("{} {}", parts[5], parts[6]);
        let name = name.trim_end_matches('/').to_owned();
        if name.is_empty() {
            continue;
        }
        entries.push(RemoteEntry {
            name,
            is_dir,
            size,
            permission,
            modified,
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_modules_stay_black_on_white_in_both_themes() {
        let ctx = egui::Context::default();
        theme::setup(&ctx);
        let matrix = QrMatrix {
            size: 2,
            modules: vec![true, false, false, true],
        };
        for mode in [theme::ThemeMode::Dark, theme::ThemeMode::Light] {
            mode.apply(&ctx);
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw_qr_code(ui, &matrix));
            });
            let qr_rectangles: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect) if rect.rect.width() < 300.0 => Some(rect.fill),
                    _ => None,
                })
                .collect();
            assert_eq!(
                qr_rectangles,
                [Color32::WHITE, Color32::BLACK, Color32::BLACK],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn long_log_text_is_elided_and_clipped_to_its_column() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(850.0, 300.0),
            )),
            ..Default::default()
        };
        let mut row_rect = Rect::NOTHING;
        let output = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                row_rect = ui
                    .allocate_exact_size(egui::vec2(700.0, 24.0), Sense::hover())
                    .0;
                paint_log_cells(
                    ui,
                    row_rect,
                    [
                        "11-11 14:33:57.123",
                        "I",
                        "12345",
                        "very-long-tag-name-with-no-spaces",
                        &"very long log message ".repeat(200),
                    ],
                    [theme::TEXT; 5],
                );
            });
        });
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::epaint::Shape::Text(text) = &shape.shape {
                    Some((shape.clip_rect, text))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(texts.len(), 5);
        for (clip, text) in &texts {
            assert!(clip.right() <= row_rect.right());
            assert!(text.pos.x + text.galley.size().x <= clip.right() + 1.0);
        }
        assert!(texts[4].1.galley.elided);
    }

    #[test]
    fn parses_wm_size() {
        assert_eq!(
            parse_wm_size("Physical size: 1080x2400\nOverride size: 720x1600"),
            Some([1080, 2400])
        );
    }

    #[test]
    fn parses_remote_directory_listing() {
        let text = "\
total 8
drwxrwx--- 2 root sdcard_rw 4096 2026-11-11 10:20 Download
-rw-rw---- 1 root sdcard_rw 1234 2026-11-11 10:21 app debug.log
";
        let entries = parse_remote_entries(text);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "Download");
        assert_eq!(entries[1].size, 1234);
        assert_eq!(entries[1].name, "app debug.log");
        let entries = parse_remote_entries(
            "drwxrwx--- 2 root sdcard_rw 4096 2026-11-11 10:20 spaced  folder/\n",
        );
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "spaced  folder");
    }

    #[test]
    fn parses_virtual_stick_values() {
        assert_eq!(parse_stick_values("-0.52:0.31"), Some((-0.52, 0.31)));
        assert_eq!(parse_stick_values("invalid"), None);
    }

    #[test]
    fn file_panels_fit_narrow_and_wide_content_areas() {
        for width in [800.0, 1400.0] {
            let ctx = egui::Context::default();
            let mut app = AndroidToolApp::from_config(AppConfig::default(), false);
            app.devices = vec![DeviceInfo {
                serial: "fixture".to_owned(),
                state: "device".to_owned(),
                ..Default::default()
            }];
            app.active_serial = Some("fixture".to_owned());
            app.files.entries = parse_remote_entries(
                "drwxrwx--- 2 root sdcard_rw 4096 2026-11-11 10:20 Download/\n",
            );
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 800.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let limit = ui.max_rect();
                    app.page_files(ui);
                    assert!(
                        ui.min_rect().right() <= limit.right() + 1.0,
                        "file layout exceeds width {width}: {:?}",
                        ui.min_rect()
                    );
                });
            });
        }
    }

    #[test]
    fn settings_layout_stays_within_narrow_and_wide_content_areas() {
        for width in [720.0, 900.0, 916.0, 1024.0, 1400.0] {
            let ctx = egui::Context::default();
            theme::setup(&ctx);
            let mut app = AndroidToolApp::from_config(AppConfig::default(), false);
            let long_directory = "very-long-directory-name\\".repeat(20);
            app.config.adb_path = format!("C:\\{long_directory}platform-tools\\adb.exe");
            app.config.bundletool_path = format!("C:\\{long_directory}bundletool.jar");
            app.config.java_path = format!("C:\\{long_directory}java.exe");
            app.config_status = format!("已保存 · C:\\{long_directory}androidtool.settings.json");
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 900.0),
                )),
                ..Default::default()
            };

            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let limit = ui.max_rect();
                    app.page_settings(ui);
                    assert!(
                        ui.min_rect().right() <= limit.right() + 1.0,
                        "settings layout exceeds width {width}: {:?}",
                        ui.min_rect()
                    );
                });
            });
            let browse_positions: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == "浏览" => {
                        Some(text.pos.x)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(browse_positions.len(), 3);
            assert!(
                browse_positions
                    .iter()
                    .all(|x| (x - browse_positions[0]).abs() < 1.0),
                "browse buttons are not aligned at width {width}: {browse_positions:?}"
            );
        }
    }

    #[test]
    fn embedded_video_area_fits_available_viewport() {
        for (width, height, scale) in [
            (800.0, 580.0, 1.0),
            (1180.0, 880.0, 1.0),
            (1180.0, 880.0, 1.5),
        ] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            theme::setup(&ctx);
            let mut app = AndroidToolApp::from_config(AppConfig::default(), false);
            app.config.mirror_embedded = true;
            app.screen.mirror_status = "INFO: 60 fps".to_owned();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let available = ui.max_rect();
                    app.scrcpy_panel(ui);
                    if cfg!(windows) {
                        let rect = app.screen.mirror_rect.unwrap();
                        assert!(
                            available.contains_rect(rect),
                            "{available:?} does not contain {rect:?}"
                        );
                        assert!(rect.width() > 0.0 && rect.height() > 100.0);
                    }
                });
            });
        }
    }

    #[test]
    fn floating_gamepad_position_stays_inside_video_after_resize() {
        for size in [egui::vec2(430.0, 500.0), egui::vec2(900.0, 360.0)] {
            let area = Rect::from_min_size(egui::pos2(200.0, 140.0), size);
            for position in [
                egui::pos2(0.0, 0.0),
                egui::pos2(1.0, 1.0),
                egui::pos2(-2.0, 3.0),
            ] {
                let rect = gamepad_overlay_rect(area, egui::vec2(330.0, 246.0), position);
                assert!(area.shrink(7.0).contains_rect(rect));
            }
        }
    }

    #[test]
    fn floating_gamepad_keeps_device_controls_and_video_size_fixed() {
        for (width, height) in [(800.0, 580.0), (1180.0, 880.0)] {
            let mut control_positions = Vec::new();
            let mut video_rects = Vec::new();
            for open in [false, true] {
                let ctx = egui::Context::default();
                theme::setup(&ctx);
                let mut app = AndroidToolApp::from_config(AppConfig::default(), false);
                app.screen.gamepad_overlay_open = open;
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, height),
                    )),
                    ..Default::default()
                };
                let output = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let available = ui.max_rect();
                        app.screen_with_controls(ui, true);
                        assert!(
                            ui.min_rect().right() <= available.right() + 1.0,
                            "right panel overflow: {:?} vs {available:?}",
                            ui.min_rect()
                        );
                        let video = app.screen.mirror_rect.unwrap();
                        assert!(available.contains_rect(video));
                        assert!(video.height() > 100.0);
                    });
                });
                let text_position = |label: &str| {
                    output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                                Some(text.pos)
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("missing label: {label}"))
                };
                let control = text_position("设备控制");
                let _ = text_position(if open { "隐藏手柄" } else { "显示手柄" });
                let video = app.screen.mirror_rect.unwrap();
                assert!(video.right() < control.x);
                video_rects.push(video);
                control_positions.push(control);
            }
            assert_eq!(
                control_positions[0], control_positions[1],
                "overlay must not move device controls"
            );
            assert_eq!(video_rects[0], video_rects[1]);
        }
    }

    #[test]
    fn package_filter_defaults_to_foreground_and_includes_child_processes() {
        let mut app = AndroidToolApp::from_config(AppConfig::default(), false);
        assert!(app.log.package_mode == LogPackageMode::Foreground);
        app.log.foreground = Some("com.example.app".to_owned());
        for process in [
            "com.example.app",
            "com.example.app:worker",
            "com.example.app2",
        ] {
            let mut entry = crate::logcat::parse_line("11-11 12:34:56.789 1234 5678 I Tag: hello");
            entry.process = process.to_owned();
            app.log.entries.push_back(entry);
        }
        app.ensure_log_filter();
        assert_eq!(app.log.filter_cache, [0, 1]);
        app.log.foreground = None;
        app.log.filter_dirty = true;
        app.ensure_log_filter();
        assert!(app.log.filter_cache.is_empty());
        app.log.package_mode = LogPackageMode::All;
        app.log.filter_dirty = true;
        app.ensure_log_filter();
        assert_eq!(app.log.filter_cache.len(), 3);
    }

    #[test]
    fn scanned_application_choices_deduplicate_children_and_prioritize_foreground() {
        let mut state = LogState::default();
        state.foreground = Some("com.example.front".to_owned());
        state
            .scanned_foreground
            .insert("com.example.previous".to_owned());
        state.process_names = HashMap::from([
            ("1".to_owned(), "com.example.front".to_owned()),
            ("2".to_owned(), "com.example.front:worker".to_owned()),
            ("3".to_owned(), "com.example.background".to_owned()),
            ("4".to_owned(), "/system/bin/vendor.service".to_owned()),
            ("5".to_owned(), "init".to_owned()),
        ]);
        assert_eq!(
            state.application_choices(),
            [
                "com.example.front",
                "com.example.background",
                "com.example.previous"
            ]
        );
        state.package_mode = LogPackageMode::Package;
        state.package_input = "com.example.front".to_owned();
        state.foreground = Some("com.example.background".to_owned());
        assert_eq!(state.package_input, "com.example.front");
    }
}
