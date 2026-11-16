//! Local MCP transport and explicitly scoped Android automation tools.
use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::*,
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    adb,
    config::{AppConfig, SigningProfile},
    installer::{InstallEvent, InstallRequest, InstallSession, PackageKind},
    util,
};

const OUTPUT_LIMIT: usize = 256 * 1024;
const MAX_OPERATIONS: usize = 256;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn share_dir(config: &AppConfig) -> PathBuf {
    let value = config.mcp_share_dir.trim();
    if value.is_empty() {
        util::install_dir().join("mcp-share")
    } else {
        util::install_dir().join(value)
    }
}

#[derive(Clone)]
struct ToolsConfig {
    adb: Option<PathBuf>,
    java: Option<PathBuf>,
    bundletool: Option<PathBuf>,
}
impl From<&AppConfig> for ToolsConfig {
    fn from(c: &AppConfig) -> Self {
        Self {
            adb: c.resolved_adb(),
            java: c.resolved_java(),
            bundletool: c.resolved_bundletool(),
        }
    }
}

struct Operation {
    serial: String,
    progress: f32,
    stage: String,
    done: bool,
    success: bool,
    message: String,
    converted: Option<PathBuf>,
    stop: Arc<AtomicBool>,
}
struct Shared {
    root: PathBuf,
    config: Mutex<ToolsConfig>,
    signing: Mutex<Option<SigningProfile>>,
    devices: Mutex<HashMap<String, Arc<Semaphore>>>,
    operations: Mutex<HashMap<String, Operation>>,
    active: Mutex<Vec<Weak<AtomicBool>>>,
    shutdown: CancellationToken,
}

impl Shared {
    fn flag(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(self.shutdown.is_cancelled()));
        let mut active = self.active.lock();
        active.retain(|f| f.strong_count() > 0);
        active.push(Arc::downgrade(&flag));
        if self.shutdown.is_cancelled() {
            flag.store(true, Ordering::Relaxed);
        }
        flag
    }
    fn stop(&self) {
        self.shutdown.cancel();
        for flag in self.active.lock().iter().filter_map(Weak::upgrade) {
            flag.store(true, Ordering::Relaxed);
        }
        self.signing.lock().take();
    }
}

pub struct McpService {
    shared: Arc<Shared>,
    thread: Option<thread::JoinHandle<()>>,
    pub port: u16,
    pub root: PathBuf,
    pub status: Arc<Mutex<String>>,
    pub running: Arc<AtomicBool>,
}

impl McpService {
    pub fn start(config: &AppConfig) -> Result<Self, String> {
        if config.mcp_port == 0 {
            return Err("MCP 端口不能为 0".into());
        }
        let requested = share_dir(config);
        fs::create_dir_all(&requested).map_err(|e| format!("共享目录不可用: {e}"))?;
        let metadata = fs::symlink_metadata(&requested).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() || is_reparse(&metadata) {
            return Err("共享根目录不能是链接或 junction".into());
        }
        let root = fs::canonicalize(requested).map_err(|e| e.to_string())?;
        let probe = root.join(format!(
            ".mcp-write-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let probe_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .map_err(|e| format!("共享目录不可写: {e}"))?;
        drop(probe_file);
        fs::remove_file(probe).map_err(|e| e.to_string())?;
        // Fail synchronously on a busy port; never silently select another one.
        let listener =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.mcp_port))
                .map_err(|e| format!("无法监听 MCP 端口 {}: {e}", config.mcp_port))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared {
            root: root.clone(),
            config: Mutex::new(config.into()),
            signing: Mutex::new(None),
            devices: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
            active: Mutex::new(Vec::new()),
            shutdown: CancellationToken::new(),
        });
        let status = Arc::new(Mutex::new("正在启动".into()));
        let running = Arc::new(AtomicBool::new(true));
        let (state, report, live, port) = (
            shared.clone(),
            status.clone(),
            running.clone(),
            config.mcp_port,
        );
        let worker = thread::Builder::new()
            .name("androidtool-mcp".into())
            .spawn(move || {
                let result = (|| -> Result<(), String> {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())?;
                    let result = runtime.block_on(async {
                        let listener = tokio::net::TcpListener::from_std(listener)
                            .map_err(|e| e.to_string())?;
                        let mut options = StreamableHttpServerConfig::default();
                        options.cancellation_token = state.shutdown.clone();
                        options.allowed_hosts = vec![format!("127.0.0.1:{port}")];
                        options = options.enforce_origin_validation();
                        options.max_request_body_bytes = 64 * 1024;
                        let factory = state.clone();
                        let service = StreamableHttpService::new(
                            move || Ok(AndroidServer(factory.clone())),
                            Arc::new(LocalSessionManager::default()),
                            options,
                        );
                        let router = axum::Router::new().nest_service("/mcp", service);
                        *report.lock() = "运行中".into();
                        let server = axum::serve(listener, router)
                            .with_graceful_shutdown(state.shutdown.clone().cancelled_owned())
                            .into_future();
                        tokio::pin!(server);
                        tokio::select! {
                            result = &mut server => result.map_err(|e| e.to_string()),
                            _ = state.shutdown.cancelled() => {
                                // MCP clients may keep an SSE stream open indefinitely.
                                // Give pending responses time to finish, then close connections.
                                match tokio::time::timeout(Duration::from_secs(2), &mut server).await {
                                    Ok(result) => result.map_err(|e| e.to_string()),
                                    Err(_) => Ok(()),
                                }
                            }
                        }
                    });
                    state.stop();
                    runtime.shutdown_timeout(Duration::from_secs(3));
                    result
                })();
                *report.lock() = match result {
                    Ok(()) => "已停止".into(),
                    Err(e) => format!("服务错误: {e}"),
                };
                state.stop();
                live.store(false, Ordering::Relaxed);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            thread: Some(worker),
            port,
            root,
            status,
            running,
        })
    }
    pub fn update_tools(&self, config: &AppConfig) {
        *self.shared.config.lock() = config.into();
    }
    pub fn authorize(&self, profile: SigningProfile) -> Result<(), String> {
        profile.validate()?;
        *self.shared.signing.lock() = Some(profile);
        Ok(())
    }
    pub fn revoke(&self) {
        self.shared.signing.lock().take();
    }
    pub fn authorized_name(&self) -> Option<String> {
        self.shared.signing.lock().as_ref().map(|p| p.name.clone())
    }
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }
}
impl Drop for McpService {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
struct AndroidServer(Arc<Shared>);
struct CancelGuard {
    flag: Arc<AtomicBool>,
    armed: bool,
}
impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::Relaxed);
        }
    }
}

fn result(value: Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value.to_string())])
}
fn failure(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

impl ServerHandler for AndroidServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info.name = "AndroidTool".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.instructions = Some("Local Android automation. Always list devices and explicitly supply serial. Coordinates are device pixels. Local paths are relative to the configured shared directory. GUI actions can run concurrently. Cancellation cannot undo device modifications.".into());
        info
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let tool = tools()
            .into_iter()
            .find(|t| t.name == request.name)
            .ok_or_else(|| McpError::invalid_params("Unknown tool", None))?;
        let args = Value::Object(request.arguments.unwrap_or_default());
        validate(&args, &Value::Object((*tool.input_schema).clone()))
            .map_err(|e| McpError::invalid_params(e, None))?;
        let name = request.name.to_string();
        let state = self.0.clone();
        if name == "get_operation" || name == "cancel_operation" {
            return Ok(match operation(&state, &name, &args) {
                Ok(v) => result(v),
                Err(e) => failure(e),
            }
            .into());
        }
        let flag = state.flag();
        let mut guard = CancelGuard {
            flag: flag.clone(),
            armed: true,
        };
        let lock = if let Some(serial) = args.get("serial").and_then(Value::as_str) {
            let semaphore = state
                .devices
                .lock()
                .entry(serial.into())
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone();
            Some(tokio::select! {
                permit = semaphore.acquire_owned() => permit.map_err(|e| McpError::internal_error(e.to_string(), None))?,
                _ = context.ct.cancelled() => return Ok(failure("操作已取消").into()),
                _ = state.shutdown.cancelled() => return Ok(failure("服务已停止").into()),
            })
        } else {
            None
        };
        if name == "install_package" {
            let start_state = state.clone();
            let start_flag = flag.clone();
            let mut starting = tokio::task::spawn_blocking(move || {
                start_install(start_state, args, start_flag, lock)
            });
            let started = tokio::select! {
                started = &mut starting => started,
                _ = context.ct.cancelled() => { guard.flag.store(true, Ordering::Relaxed); let _ = starting.await; return Ok(failure("操作已取消").into()); },
                _ = state.shutdown.cancelled() => { guard.flag.store(true, Ordering::Relaxed); let _ = starting.await; return Ok(failure("服务已停止").into()); },
            };
            // Installation owns its cancellation flag after returning its operation ID.
            guard.armed = false;
            return Ok(match started {
                Ok(Ok(v)) => result(v),
                Ok(Err(e)) => failure(e),
                Err(e) => failure(e.to_string()),
            }
            .into());
        }
        let execute_state = state.clone();
        let mut work = tokio::task::spawn_blocking(move || {
            let _permit = lock;
            execute(&execute_state, &name, &args, &flag)
        });
        let value = tokio::select! {
            value = &mut work => value.map_err(|e| e.to_string()).and_then(|v| v),
            _ = context.ct.cancelled() => { guard.flag.store(true, Ordering::Relaxed); let _ = work.await; Err("操作已取消".into()) },
            _ = state.shutdown.cancelled() => { guard.flag.store(true, Ordering::Relaxed); let _ = work.await; Err("服务已停止".into()) },
        };
        Ok(match value {
            Ok(v) => v,
            Err(e) => failure(e),
        }
        .into())
    }
}

fn tools() -> Vec<Tool> {
    let string = || json!({"type":"string","minLength":1,"maxLength":4096});
    let number = |max| json!({"type":"integer","minimum":0,"maximum":max});
    let definitions = vec![
        (
            "list_devices",
            "List ADB devices including offline and unauthorized devices.",
            true,
            json!({}),
            vec![],
        ),
        (
            "get_device_info",
            "Read Android version, model and current display size.",
            true,
            json!({}),
            vec![],
        ),
        (
            "screenshot",
            "Capture PNG image in native device pixels.",
            true,
            json!({}),
            vec![],
        ),
        (
            "get_ui_tree",
            "Return UIAutomator XML. Secure surfaces may be unavailable.",
            true,
            json!({}),
            vec![],
        ),
        (
            "tap",
            "Tap at device pixel coordinates.",
            false,
            json!({"x":number(32767),"y":number(32767)}),
            vec!["x", "y"],
        ),
        (
            "swipe",
            "Swipe between device pixel coordinates (duration_ms defaults to 300).",
            false,
            json!({"x1":number(32767),"y1":number(32767),"x2":number(32767),"y2":number(32767),"duration_ms":json!({"type":"integer","minimum":1,"maximum":10000})}),
            vec!["x1", "y1", "x2", "y2"],
        ),
        (
            "keyevent",
            "Send an Android numeric key code.",
            false,
            json!({"code":number(400)}),
            vec!["code"],
        ),
        (
            "input_text",
            "Enter printable ASCII text; non-ASCII and percent signs are unsupported.",
            false,
            json!({"text":json!({"type":"string","minLength":1,"maxLength":4096})}),
            vec!["text"],
        ),
        (
            "list_apps",
            "List installed Android package names.",
            true,
            json!({}),
            vec![],
        ),
        (
            "launch_app",
            "Launch the main launcher activity for a package.",
            false,
            json!({"package":string()}),
            vec!["package"],
        ),
        (
            "force_stop_app",
            "Force stop an application package.",
            false,
            json!({"package":string()}),
            vec!["package"],
        ),
        (
            "install_package",
            "Install APK/AAB/APKS from shared directory; returns operation_id. AAB requires GUI signing authorization. Poll get_operation.",
            false,
            json!({"path":string()}),
            vec!["path"],
        ),
        (
            "get_operation",
            "Read installation status. IDs expire when service restarts.",
            true,
            json!({"operation_id":string()}),
            vec!["operation_id"],
        ),
        (
            "cancel_operation",
            "Cancel an installation. Already completed device changes cannot be undone.",
            false,
            json!({"operation_id":string()}),
            vec!["operation_id"],
        ),
        (
            "list_files",
            "List a device directory (remote absolute path).",
            true,
            json!({"remote":string()}),
            vec!["remote"],
        ),
        (
            "push_file",
            "Upload a regular file from shared directory to a remote absolute path.",
            false,
            json!({"path":string(),"remote":string()}),
            vec!["path", "remote"],
        ),
        (
            "pull_file",
            "Download a device file into an existing shared subdirectory; never overwrites.",
            false,
            json!({"path":string(),"remote":string()}),
            vec!["path", "remote"],
        ),
        (
            "get_logcat",
            "Read finite log snapshot; limit defaults to 200, maximum 1000. Filters apply to recent snapshot; no streaming.",
            true,
            json!({"limit":json!({"type":"integer","minimum":1,"maximum":1000}),"level":json!({"type":"string","enum":["V","D","I","W","E","F"]}),"keyword":json!({"type":"string","maxLength":1024})}),
            vec![],
        ),
    ];
    definitions.into_iter().map(|(name, desc, read, mut props, mut required)| {
        if name != "list_devices" { props["serial"] = string(); required.push("serial"); }
        let schema = json!({"type":"object","properties":props,"required":required,"additionalProperties":false});
        Tool::new(name, desc, schema.as_object().unwrap().clone()).with_annotations(ToolAnnotations::new().read_only(read).destructive(!read).open_world(false))
    }).collect()
}

fn validate(args: &Value, schema: &Value) -> Result<(), String> {
    let obj = args.as_object().ok_or("arguments must be an object")?;
    for key in schema["required"].as_array().unwrap() {
        if !obj.contains_key(key.as_str().unwrap()) {
            return Err(format!("Missing parameter {key}"));
        }
    }
    for (key, value) in obj {
        let rule = &schema["properties"][key];
        if rule.is_null() {
            return Err(format!("Unknown parameter {key}"));
        }
        if rule["type"] == "string" {
            let s = value
                .as_str()
                .ok_or_else(|| format!("{key} must be a string"))?;
            let len = s.chars().count() as u64;
            if len < rule["minLength"].as_u64().unwrap_or(0)
                || len > rule["maxLength"].as_u64().unwrap_or(u64::MAX)
            {
                return Err(format!("{key} has invalid length"));
            }
        } else {
            let n = value
                .as_u64()
                .ok_or_else(|| format!("{key} must be a nonnegative integer"))?;
            if n < rule["minimum"].as_u64().unwrap_or(0)
                || n > rule["maximum"].as_u64().unwrap_or(u64::MAX)
            {
                return Err(format!("{key} is out of range"));
            }
        }
        if let Some(values) = rule["enum"].as_array() {
            if !values.contains(value) {
                return Err(format!("Invalid {key}"));
            }
        }
    }
    Ok(())
}

fn text<'a>(args: &'a Value, key: &str) -> &'a str {
    args[key].as_str().unwrap_or("")
}
fn remote(path: &str) -> Result<String, String> {
    if !path.starts_with('/') || path.contains(['\0', '\r', '\n']) {
        return Err("remote 必须是设备绝对路径且不含控制符".into());
    }
    Ok(util::quote_remote_arg(path))
}
fn package(value: &str) -> Result<&str, String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
        || !value.as_bytes()[0].is_ascii_alphabetic()
    {
        return Err("无效包名".into());
    }
    Ok(value)
}

// Reject links/reparse points before canonicalizing; checking just a lexical prefix is insufficient.
fn shared_path(root: &Path, value: &str, writing: bool) -> Result<PathBuf, String> {
    let relative = Path::new(value);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || value.contains([':', '\0'])
    {
        return Err("path 必须是共享目录内的相对路径，不允许 .. 或绝对路径".into());
    }
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for (i, part) in relative.components().enumerate() {
        current.push(part);
        if writing && i + 1 == count {
            if fs::symlink_metadata(&current).is_ok() {
                return Err("下载目标已存在".into());
            }
            break;
        }
        let meta = fs::symlink_metadata(&current).map_err(|e| format!("共享文件不可用: {e}"))?;
        if meta.file_type().is_symlink() || is_reparse(&meta) {
            return Err("共享路径不允许链接或 junction".into());
        }
        let real = fs::canonicalize(&current).map_err(|e| e.to_string())?;
        if !real.starts_with(root) {
            return Err("路径超出共享目录".into());
        }
    }
    let parent =
        fs::canonicalize(current.parent().ok_or("无效父目录")?).map_err(|e| e.to_string())?;
    if !parent.starts_with(root) {
        return Err("路径超出共享目录".into());
    }
    Ok(parent.join(current.file_name().ok_or("无效文件名")?))
}
#[cfg(windows)]
fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &fs::Metadata) -> bool {
    false
}

fn command(
    path: &Path,
    args: &[String],
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let output = util::run_cancellable_limited(
        util::command(path).args(args),
        timeout,
        stop,
        16 * 1024 * 1024,
    )?;
    if !output.status.success() {
        return Err(util::truncate_chars(&util::output_text(&output), 8192));
    }
    Ok(output.stdout)
}
fn online(state: &Shared, serial: &str, stop: &AtomicBool) -> Result<PathBuf, String> {
    let path = state
        .config
        .lock()
        .adb
        .clone()
        .ok_or("未找到 ADB，请在设置中配置")?;
    let bytes = command(
        &path,
        &["devices".into(), "-l".into()],
        Duration::from_secs(5),
        stop,
    )?;
    if !adb::parse_devices(&String::from_utf8_lossy(&bytes))
        .iter()
        .any(|d| d.serial == serial && d.is_online())
    {
        return Err(format!("设备未在线或未授权: {serial}"));
    }
    Ok(path)
}
fn shell(adb: &Path, serial: &str, args: &[&str], stop: &AtomicBool) -> Result<String, String> {
    let mut command_args = vec!["-s".into(), serial.into(), "shell".into()];
    command_args.extend(args.iter().map(|s| s.to_string()));
    command(adb, &command_args, Duration::from_secs(30), stop)
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

fn execute(
    state: &Shared,
    name: &str,
    args: &Value,
    stop: &AtomicBool,
) -> Result<CallToolResult, String> {
    if name == "list_devices" {
        let path = state.config.lock().adb.clone().ok_or("未找到 ADB")?;
        let bytes = command(
            &path,
            &["devices".into(), "-l".into()],
            Duration::from_secs(5),
            stop,
        )?;
        let devices: Vec<_> = adb::parse_devices(&String::from_utf8_lossy(&bytes)).iter().map(|d| json!({"serial":d.serial,"state":d.state,"model":d.model,"product":d.product,"connection":d.connection})).collect();
        return Ok(result(json!({"devices":devices})));
    }
    let serial = text(args, "serial");
    let adb = online(state, serial, stop)?;
    if name == "screenshot" {
        let bytes = command(
            &adb,
            &[
                "-s".into(),
                serial.into(),
                "exec-out".into(),
                "screencap".into(),
                "-p".into(),
            ],
            Duration::from_secs(30),
            stop,
        )?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("设备未返回 PNG 截图".into());
        }
        return Ok(CallToolResult::success(vec![
            ContentBlock::text(json!({"serial":serial}).to_string()),
            ContentBlock::image(STANDARD.encode(bytes), "image/png"),
        ]));
    }
    let output = match name {
        "get_device_info" => {
            let model = shell(&adb, serial, &["getprop", "ro.product.model"], stop)?;
            let android = shell(&adb, serial, &["getprop", "ro.build.version.release"], stop)?;
            let sdk = shell(&adb, serial, &["getprop", "ro.build.version.sdk"], stop)?;
            let size = shell(&adb, serial, &["wm", "size"], stop)?;
            return Ok(result(
                json!({"serial":serial,"model":model,"android":android,"sdk":sdk,"display":size}),
            ));
        }
        "get_ui_tree" => {
            let path = format!(
                "/data/local/tmp/androidtool-mcp-ui-{}-{}.xml",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            );
            let dumped = shell(&adb, serial, &["uiautomator", "dump", &path], stop);
            let xml = dumped.and_then(|_| shell(&adb, serial, &["cat", &path], stop));
            let _ = shell(&adb, serial, &["rm", "-f", &path], &AtomicBool::new(false));
            let xml = xml?;
            if !xml.contains("<hierarchy") {
                return Err("无法获取 UI XML".into());
            }
            xml
        }
        "tap" => shell(
            &adb,
            serial,
            &[
                "input",
                "tap",
                &args["x"].to_string(),
                &args["y"].to_string(),
            ],
            stop,
        )?,
        "swipe" => shell(
            &adb,
            serial,
            &[
                "input",
                "swipe",
                &args["x1"].to_string(),
                &args["y1"].to_string(),
                &args["x2"].to_string(),
                &args["y2"].to_string(),
                &args["duration_ms"].as_u64().unwrap_or(300).to_string(),
            ],
            stop,
        )?,
        "keyevent" => shell(
            &adb,
            serial,
            &["input", "keyevent", &args["code"].to_string()],
            stop,
        )?,
        "input_text" => {
            let input = text(args, "text");
            if !input.bytes().all(|b| (32..=126).contains(&b) && b != b'%') {
                return Err("仅支持可打印 ASCII 文本，不支持百分号".into());
            }
            shell(
                &adb,
                serial,
                &[
                    "input",
                    "text",
                    &util::quote_remote_arg(&input.replace(' ', "%s")),
                ],
                stop,
            )?
        }
        "list_apps" => shell(&adb, serial, &["pm", "list", "packages"], stop)?,
        "launch_app" => shell(
            &adb,
            serial,
            &[
                "monkey",
                "-p",
                package(text(args, "package"))?,
                "-c",
                "android.intent.category.LAUNCHER",
                "1",
            ],
            stop,
        )?,
        "force_stop_app" => shell(
            &adb,
            serial,
            &["am", "force-stop", package(text(args, "package"))?],
            stop,
        )?,
        "list_files" => shell(
            &adb,
            serial,
            &["ls", "-lap", "--", &remote(text(args, "remote"))?],
            stop,
        )?,
        "push_file" | "pull_file" => {
            let downloading = name == "pull_file";
            let path = shared_path(&state.root, text(args, "path"), downloading)?;
            let remote_path = text(args, "remote");
            remote(remote_path)?;
            let reservation = if downloading {
                let mut options = fs::OpenOptions::new();
                options.read(true).write(true).create_new(true);
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.share_mode(3);
                }
                Some(options.open(&path).map_err(|e| e.to_string())?)
            } else {
                if !path.is_file() {
                    return Err("只能上传普通文件".into());
                }
                None
            };
            let local = path.to_string_lossy().to_string();
            // ADB sync paths are arguments, not shell expressions; do not shell-quote them.
            let operands = if downloading {
                vec![
                    "-s".into(),
                    serial.into(),
                    "pull".into(),
                    remote_path.into(),
                    local,
                ]
            } else {
                vec![
                    "-s".into(),
                    serial.into(),
                    "push".into(),
                    local,
                    remote_path.into(),
                ]
            };
            let transferred = command(&adb, &operands, Duration::from_secs(120), stop);
            drop(reservation);
            if downloading && transferred.is_err() {
                let _ = fs::remove_file(&path);
            }
            String::from_utf8_lossy(&transferred?).to_string()
        }
        "get_logcat" => {
            let level = args["level"].as_str().unwrap_or("V");
            let limit = args["limit"].as_u64().unwrap_or(200);
            let bytes = command(
                &adb,
                &[
                    "-s".into(),
                    serial.into(),
                    "logcat".into(),
                    "-d".into(),
                    "-v".into(),
                    "threadtime".into(),
                    "-t".into(),
                    limit.to_string(),
                    format!("*:{level}"),
                ],
                Duration::from_secs(30),
                stop,
            )?;
            let raw = String::from_utf8_lossy(&bytes);
            let keyword = args["keyword"].as_str().unwrap_or("");
            raw.lines()
                .filter(|line| line.contains(keyword))
                .take(limit as usize)
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return Err("未知工具".into()),
    };
    let truncated = output.len() > OUTPUT_LIMIT;
    Ok(result(
        json!({"serial":serial,"output":util::truncate_chars(&output, OUTPUT_LIMIT),"truncated":truncated}),
    ))
}

fn start_install(
    state: Arc<Shared>,
    args: Value,
    stop: Arc<AtomicBool>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<Value, String> {
    let serial = text(&args, "serial").to_string();
    let adb = online(&state, &serial, &stop)?;
    let path = shared_path(&state.root, text(&args, "path"), false)?;
    if !path.is_file() {
        return Err("安装包必须是普通文件".into());
    }
    let kind = PackageKind::detect(&path);
    if kind == PackageKind::Unknown {
        return Err("仅支持 APK / AAB / APKS".into());
    }
    let config = state.config.lock().clone();
    if kind != PackageKind::Apk && (config.java.is_none() || config.bundletool.is_none()) {
        return Err("AAB/APKS 需要配置 Java 与 bundletool".into());
    }
    let signing = if kind == PackageKind::Aab {
        Some(
            state
                .signing
                .lock()
                .clone()
                .ok_or("请先在设置页授权 MCP 使用 AAB 签名")?,
        )
    } else {
        None
    };
    let id = format!("op-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let output = if kind == PackageKind::Aab {
        let folder = state.root.join("converted");
        if !folder.exists() {
            fs::create_dir(&folder).map_err(|e| e.to_string())?;
        }
        Some(shared_path(
            &state.root,
            &format!("converted/{id}.apk"),
            true,
        )?)
    } else {
        None
    };
    {
        let mut operations = state.operations.lock();
        if operations.len() >= MAX_OPERATIONS {
            return Err("任务数量已达上限，请重启 MCP 服务清理历史任务".into());
        }
        operations.insert(
            id.clone(),
            Operation {
                serial: serial.clone(),
                progress: 0.0,
                stage: "准备安装".into(),
                done: false,
                success: false,
                message: String::new(),
                converted: None,
                stop: stop.clone(),
            },
        );
    }
    let session = InstallSession::start(InstallRequest {
        adb_path: adb,
        java_path: config.java.unwrap_or_default(),
        bundletool_path: config.bundletool.unwrap_or_default(),
        serial: serial.clone(),
        package: path,
        convert_only: false,
        output_apk: output,
        signing: signing.clone(),
        cancel: Some(stop.clone()),
    });
    let task_id = id.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let started = Instant::now();
        loop {
            if state.shutdown.is_cancelled() || started.elapsed() > Duration::from_secs(600) {
                stop.store(true, Ordering::Relaxed);
            }
            if let Some(event) = session.try_recv() {
                let mut operations = state.operations.lock();
                let op = operations.get_mut(&task_id).unwrap();
                match event {
                    InstallEvent::Progress { progress, stage } => {
                        op.progress = progress;
                        op.stage = stage;
                    }
                    InstallEvent::Log { .. } => {}
                    InstallEvent::Converted(path) => op.converted = Some(path),
                    InstallEvent::Finished {
                        success,
                        message,
                        converted,
                    } => {
                        op.done = true;
                        op.success = success;
                        op.message = signing
                            .as_ref()
                            .map(|s| s.redact(&message))
                            .unwrap_or(message);
                        if converted.is_some() {
                            op.converted = converted;
                        }
                        op.stage = if success { "完成" } else { "失败或取消" }.into();
                        break;
                    }
                }
            } else {
                thread::sleep(Duration::from_millis(30));
            }
        }
    });
    Ok(json!({"serial":serial,"operation_id":id}))
}

fn operation(state: &Shared, name: &str, args: &Value) -> Result<Value, String> {
    let mut operations = state.operations.lock();
    let op = operations
        .get_mut(text(args, "operation_id"))
        .ok_or("任务不存在或服务已重启")?;
    if op.serial != text(args, "serial") {
        return Err("任务的 serial 不匹配".into());
    }
    if name == "cancel_operation" && !op.done {
        op.stop.store(true, Ordering::Relaxed);
    }
    Ok(
        json!({"serial":op.serial,"done":op.done,"success":op.success,"progress":op.progress,"stage":op.stage,"message":op.message,"cancel_requested":op.stop.load(Ordering::Relaxed),"converted":op.converted.as_ref().and_then(|p|p.strip_prefix(&state.root).ok()).map(|p|p.to_string_lossy())}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{ServiceExt, transport::StreamableHttpClientTransport};
    use std::io::{Read, Write};

    fn temporary_root() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("androidtool-mcp-test-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    fn config(root: &Path) -> AppConfig {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        AppConfig {
            mcp_port: port,
            mcp_share_dir: root.to_string_lossy().into_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn shared_paths_reject_escape_and_existing_destinations() {
        let root = temporary_root();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/input.apk"), b"apk").unwrap();
        assert!(shared_path(&root, "sub/input.apk", false).is_ok());
        assert!(shared_path(&root, "../outside", false).is_err());
        assert!(shared_path(&root, "C:/outside", false).is_err());
        assert!(shared_path(&root, "sub/input.apk", true).is_err());
        assert!(shared_path(&root, "sub/new.apk", true).is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::env::temp_dir(), root.join("link")).unwrap();
            assert!(shared_path(&root, "link/test", true).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn schemas_validate_serial_types_and_injection_inputs() {
        let tool = tools().into_iter().find(|t| t.name == "tap").unwrap();
        let schema = Value::Object((*tool.input_schema).clone());
        assert!(validate(&json!({"serial":"abc","x":1,"y":2}), &schema).is_ok());
        assert!(validate(&json!({"x":1,"y":2}), &schema).is_err());
        assert!(validate(&json!({"serial":"abc","x":"1;rm","y":2}), &schema).is_err());
        assert!(package("com.example;rm").is_err());
        assert_eq!(remote("/sdcard/a'b").unwrap(), "'/sdcard/a'\\''b'");
        let config: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.mcp_enabled);
        assert_eq!(config.mcp_port, 18765);
    }

    fn http_status(port: u16, host: &str, origin: Option<&str>) -> u16 {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let origin = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        write!(stream, "POST /mcp HTTP/1.1\r\nHost: {host}\r\n{origin}Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
        let mut output = String::new();
        stream.read_to_string(&mut output).unwrap();
        output
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn streamable_http_client_and_request_guards() {
        let root = temporary_root();
        let service = McpService::start(&config(&root)).unwrap();
        let port = service.port;
        assert_eq!(http_status(port, "attacker.example", None), 403);
        assert_eq!(
            http_status(
                port,
                &format!("127.0.0.1:{port}"),
                Some("https://attacker.example")
            ),
            403
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let transport = StreamableHttpClientTransport::from_uri(service.url());
            let client = ().serve(transport).await.expect("initialize MCP client");
            let listed = client.list_all_tools().await.expect("list MCP tools");
            assert!(listed.iter().any(|t| t.name == "screenshot"));
            assert!(listed.iter().any(|t| t.name == "install_package"));
            let response = client
                .call_tool(
                    CallToolRequestParams::new("tap")
                        .with_arguments(json!({"x":1,"y":2}).as_object().unwrap().clone()),
                )
                .await;
            assert!(response.is_err());
            client.cancel().await.unwrap();
        });
        drop(runtime);
        drop(service);
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn busy_port_fails_without_changing_port() {
        let root = temporary_root();
        let config = config(&root);
        let service = McpService::start(&config).unwrap();
        assert!(McpService::start(&config).is_err());
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn signing_authorization_is_session_only_and_revocable() {
        let root = temporary_root();
        fs::write(root.join("key.jks"), b"keystore").unwrap();
        let mut config = config(&root);
        config.mcp_enabled = true;
        config.signing_profiles.push(SigningProfile {
            id: "one".into(),
            name: "Release".into(),
            keystore_path: root.join("key.jks").to_string_lossy().into_owned(),
            key_alias: "release".into(),
            store_password: "session-secret".into(),
            ..Default::default()
        });
        let serialized = serde_json::to_string(&config).unwrap();
        assert!(!serialized.contains("session-secret"));
        assert!(
            serde_json::from_str::<AppConfig>(&serialized)
                .unwrap()
                .mcp_enabled
        );
        let service = McpService::start(&config).unwrap();
        service
            .authorize(config.signing_profiles[0].clone())
            .unwrap();
        assert_eq!(service.authorized_name().as_deref(), Some("Release"));
        service.revoke();
        assert!(service.authorized_name().is_none());
        service
            .authorize(config.signing_profiles[0].clone())
            .unwrap();
        drop(service);
        let restarted = McpService::start(&config).unwrap();
        assert!(restarted.authorized_name().is_none());
        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    fn params(name: &'static str, args: Value) -> CallToolRequestParams {
        CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone())
    }
    fn payload(value: &CallToolResult) -> Value {
        serde_json::from_str(&value.content[0].as_text().unwrap().text).unwrap()
    }

    #[test]
    fn mock_adb_end_to_end_images_files_installs_and_cancellation() {
        let root = temporary_root();
        let executable = root.join(if cfg!(windows) {
            "mock-adb.exe"
        } else {
            "mock-adb"
        });
        let status = util::command("rustc")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_adb.rs"))
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap();
        assert!(status.success());
        image::RgbaImage::from_pixel(2, 3, image::Rgba([0, 128, 255, 255]))
            .save(root.join("screen.png"))
            .unwrap();
        fs::write(root.join("app.apk"), b"mock").unwrap();
        fs::write(root.join("app.aab"), b"mock").unwrap();
        let mut config = config(&root);
        config.adb_path = executable.to_string_lossy().into_owned();
        let service = McpService::start(&config).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let client =
                ().serve(StreamableHttpClientTransport::from_uri(service.url()))
                    .await
                    .unwrap();
            let devices = client
                .call_tool(params("list_devices", json!({})))
                .await
                .unwrap();
            assert_eq!(payload(&devices)["devices"].as_array().unwrap().len(), 4);
            let screenshot = client
                .call_tool(params("screenshot", json!({"serial":"device-two"})))
                .await
                .unwrap();
            let image = screenshot.content[1].as_image().unwrap();
            let bytes = STANDARD.decode(&image.data).unwrap();
            assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 2);
            assert_eq!(payload(&screenshot)["serial"], "device-two");
            let tapped = client
                .call_tool(params("tap", json!({"serial":"device-one","x":20,"y":30})))
                .await
                .unwrap();
            assert_ne!(tapped.is_error, Some(true));
            let xml = client
                .call_tool(params("get_ui_tree", json!({"serial":"device-one"})))
                .await
                .unwrap();
            assert!(
                payload(&xml)["output"]
                    .as_str()
                    .unwrap()
                    .contains("<hierarchy")
            );
            let offline = client
                .call_tool(params("tap", json!({"serial":"locked","x":20,"y":30})))
                .await
                .unwrap();
            assert_eq!(offline.is_error, Some(true));
            let injection = client
                .call_tool(params(
                    "launch_app",
                    json!({"serial":"device-one","package":"com.test;reboot"}),
                ))
                .await
                .unwrap();
            assert_eq!(injection.is_error, Some(true));
            let logs = client
                .call_tool(params(
                    "get_logcat",
                    json!({"serial":"device-one","keyword":"needle"}),
                ))
                .await
                .unwrap();
            assert!(!payload(&logs)["output"].as_str().unwrap().contains("other"));
            let large = client
                .call_tool(params(
                    "list_files",
                    json!({"serial":"device-one","remote":"/huge"}),
                ))
                .await
                .unwrap();
            assert_eq!(payload(&large)["truncated"], true);
            let pulled = client
                .call_tool(params(
                    "pull_file",
                    json!({"serial":"device-one","remote":"/sdcard/mock","path":"download.txt"}),
                ))
                .await
                .unwrap();
            assert_ne!(pulled.is_error, Some(true));
            assert_eq!(fs::read(root.join("download.txt")).unwrap(), b"downloaded");
            let overwrite = client
                .call_tool(params(
                    "pull_file",
                    json!({"serial":"device-one","remote":"/sdcard/mock","path":"download.txt"}),
                ))
                .await
                .unwrap();
            assert_eq!(overwrite.is_error, Some(true));
            let aab = client
                .call_tool(params(
                    "install_package",
                    json!({"serial":"device-one","path":"app.aab"}),
                ))
                .await
                .unwrap();
            assert_eq!(aab.is_error, Some(true));
            let installed = client
                .call_tool(params(
                    "install_package",
                    json!({"serial":"device-one","path":"app.apk"}),
                ))
                .await
                .unwrap();
            let id = payload(&installed)["operation_id"]
                .as_str()
                .unwrap()
                .to_string();
            let mut finished = false;
            for _ in 0..100 {
                let op = client
                    .call_tool(params(
                        "get_operation",
                        json!({"serial":"device-one","operation_id":id}),
                    ))
                    .await
                    .unwrap();
                if payload(&op)["done"] == true {
                    assert_eq!(payload(&op)["success"], true);
                    finished = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(finished);
            fs::write(root.join("slow"), b"").unwrap();
            let installed = client
                .call_tool(params(
                    "install_package",
                    json!({"serial":"device-one","path":"app.apk"}),
                ))
                .await
                .unwrap();
            let id = payload(&installed)["operation_id"]
                .as_str()
                .unwrap()
                .to_string();
            tokio::time::sleep(Duration::from_millis(200)).await;
            client
                .call_tool(params(
                    "cancel_operation",
                    json!({"serial":"device-one","operation_id":id}),
                ))
                .await
                .unwrap();
            let mut cancelled = false;
            for _ in 0..100 {
                let op = client
                    .call_tool(params(
                        "get_operation",
                        json!({"serial":"device-one","operation_id":id}),
                    ))
                    .await
                    .unwrap();
                if payload(&op)["done"] == true {
                    assert_eq!(payload(&op)["success"], false);
                    cancelled = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            assert!(cancelled);
            fs::remove_file(root.join("slow")).unwrap();
            fs::write(root.join("empty"), b"").unwrap();
            let devices = client
                .call_tool(params("list_devices", json!({})))
                .await
                .unwrap();
            assert!(payload(&devices)["devices"].as_array().unwrap().is_empty());
            client.cancel().await.unwrap();
        });
        drop(runtime);
        drop(service);
        let commands = fs::read_to_string(root.join("commands.log")).unwrap();
        assert!(!commands.contains("com.test;reboot"));
        assert!(
            commands
                .lines()
                .all(|l| l.starts_with("[\"devices\"") || l.starts_with("[\"-s\", \"device-"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shutdown_terminates_inflight_device_command() {
        let root = temporary_root();
        let executable = root.join(if cfg!(windows) {
            "mock-adb.exe"
        } else {
            "mock-adb"
        });
        assert!(
            util::command("rustc")
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_adb.rs"))
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join("slow"), b"").unwrap();
        let mut config = config(&root);
        config.adb_path = executable.to_string_lossy().into_owned();
        let service = McpService::start(&config).unwrap();
        let url = service.url();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let client_thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let client = ().serve(StreamableHttpClientTransport::from_uri(url)).await.unwrap();
                ready_tx.send(()).unwrap();
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    client.call_tool(params("tap", json!({"serial":"device-one","x":1,"y":1}))),
                )
                .await;
            });
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for _ in 0..100 {
            if fs::read_to_string(root.join("commands.log"))
                .unwrap_or_default()
                .contains("\"shell\"")
            {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
        assert!(
            fs::read_to_string(root.join("commands.log"))
                .unwrap()
                .contains("\"shell\"")
        );
        let start = Instant::now();
        drop(service);
        client_thread.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(8));
        fs::remove_dir_all(root).unwrap();
    }
}
