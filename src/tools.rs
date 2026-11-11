use crate::util;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    Adb,
    Bundletool,
    Java,
}
impl ToolKind {
    pub const ALL: [Self; 3] = [Self::Adb, Self::Bundletool, Self::Java];
    pub fn index(self) -> usize {
        match self {
            Self::Adb => 0,
            Self::Bundletool => 1,
            Self::Java => 2,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Adb => "ADB",
            Self::Bundletool => "bundletool",
            Self::Java => "Java",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolVersion {
    pub name: String,
    pub url: String,
    pub checksum: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum ToolEvent {
    Progress {
        message: String,
        fraction: Option<f32>,
    },
    Versions(Vec<ToolVersion>),
    VersionsFallback {
        versions: Vec<ToolVersion>,
        message: String,
    },
    Finished {
        success: bool,
        message: String,
        path: Option<PathBuf>,
    },
}
#[derive(Serialize, Deserialize)]
struct Request {
    kind: ToolKind,
    proxy: String,
    version: Option<ToolVersion>,
    major: u8,
    directory: Option<PathBuf>,
    target: Option<PathBuf>,
    overwrite: bool,
}
pub struct ToolSession {
    receiver: Receiver<ToolEvent>,
    child: Child,
    directory: Option<PathBuf>,
    finished: bool,
}
pub fn download_directory(kind: ToolKind, version: &ToolVersion) -> PathBuf {
    let root = util::install_dir().join("tools");
    match kind {
        ToolKind::Adb => root.join("adb"),
        ToolKind::Bundletool => root
            .join("bundletool")
            .join(safe_version_name(&version.name)),
        ToolKind::Java => root.join("jdk").join(safe_version_name(&version.name)),
    }
}
fn safe_version_name(name: &str) -> String {
    let mut value: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if value.is_empty() || value == "." || value == ".." {
        value = "unknown".into();
    }
    value.chars().take(80).collect()
}
impl ToolSession {
    pub fn start(
        kind: ToolKind,
        proxy: &str,
        version: Option<ToolVersion>,
        major: u8,
        install: bool,
        overwrite: bool,
    ) -> Result<Self, String> {
        agent(proxy)?;
        let target = if install {
            Some(
                version
                    .as_ref()
                    .map(|v| download_directory(kind, v))
                    .unwrap_or_else(|| util::install_dir().join("tools").join("adb")),
            )
        } else {
            None
        };
        let directory = if let Some(root) = &target {
            let parent = root.parent().ok_or("安装路径无效")?;
            util::ensure_dir(parent)?;
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = parent.join(format!(".androidtool-staging-{id}-{}", std::process::id()));
            fs::create_dir(&path).map_err(|e| e.to_string())?;
            Some(path)
        } else {
            None
        };
        let request = Request {
            kind,
            proxy: proxy.to_owned(),
            version,
            major,
            directory: directory.clone(),
            target,
            overwrite,
        };
        let child_result = util::command(std::env::current_exe().map_err(|e| e.to_string())?)
            .arg("--tool-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child_result {
            Ok(child) => child,
            Err(error) => {
                if let Some(path) = &directory {
                    let _ = fs::remove_dir_all(path);
                }
                return Err(error.to_string());
            }
        };
        let write_result = child
            .stdin
            .take()
            .ok_or("下载进程输入不可用".to_owned())
            .and_then(|input| serde_json::to_writer(input, &request).map_err(|e| e.to_string()));
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(path) = &directory {
                let _ = fs::remove_dir_all(path);
            }
            return Err(error);
        }
        let stdout = child.stdout.take().ok_or("下载进程输出不可用")?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut ended = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<ToolEvent>(&line) {
                    ended |= matches!(
                        event,
                        ToolEvent::Finished { .. }
                            | ToolEvent::Versions(_)
                            | ToolEvent::VersionsFallback { .. }
                    );
                    if sender.send(event).is_err() {
                        return;
                    }
                }
            }
            if !ended {
                let _ = sender.send(ToolEvent::Finished {
                    success: false,
                    message: "下载进程意外退出".into(),
                    path: None,
                });
            }
        });
        Ok(Self {
            receiver,
            child,
            directory,
            finished: false,
        })
    }
    pub fn try_recv(&mut self) -> Option<ToolEvent> {
        let event = self.receiver.try_recv().ok()?;
        if matches!(&event, ToolEvent::Finished { success: true, .. }) {
            self.finished = true;
        }
        Some(event)
    }
}
impl Drop for ToolSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !self.finished {
            if let Some(path) = &self.directory {
                // Only remove this session's freshly created, canonical child directory.
                let root = path.parent().and_then(|parent| parent.canonicalize().ok());
                if let (Some(root), Ok(path)) = (root, path.canonicalize()) {
                    if path.parent() == Some(root.as_path())
                        && path.file_name().is_some_and(|n| {
                            n.to_string_lossy().starts_with(".androidtool-staging-")
                        })
                    {
                        let _ = fs::remove_dir_all(path);
                    }
                }
            }
        }
    }
}
fn emit(event: ToolEvent) {
    let mut stdout = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, &event);
    let _ = writeln!(stdout);
    let _ = stdout.flush();
}
fn finalize_install(
    staging: &Path,
    target: &Path,
    file: &Path,
    overwrite: bool,
) -> Result<PathBuf, String> {
    let relative = file.strip_prefix(staging).map_err(|_| "安装文件路径无效")?;
    let backup = staging.with_extension("backup");
    let had_target = target.exists();
    if had_target {
        if !overwrite {
            return Err("目标版本已存在，请确认覆盖后重试".into());
        }
        fs::rename(target, &backup).map_err(|e| format!("无法备份旧版本：{e}"))?;
    }
    if let Err(error) = fs::rename(staging, target) {
        if had_target {
            if let Err(restore) = fs::rename(&backup, target) {
                return Err(format!(
                    "安装失败：{error}；恢复旧版本失败：{restore}，备份位于 {}",
                    backup.display()
                ));
            }
        }
        return Err(format!("安装失败：{error}"));
    }
    if had_target {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(target.join(relative))
}
pub fn worker_main() {
    let result = (|| -> Result<(), String> {
        let request: Request =
            serde_json::from_reader(std::io::stdin()).map_err(|e| e.to_string())?;
        let client = agent(&request.proxy)?;
        if let Some(directory) = &request.directory {
            let version = match request.version.clone() {
                Some(version) => version,
                None => versions(&client, request.kind, request.major)?
                    .into_iter()
                    .next()
                    .ok_or("未找到可下载版本")?,
            };
            let path = install(&client, request.kind, &version, directory)?;
            let target = request.target.as_ref().ok_or("安装目标无效")?;
            let path = finalize_install(directory, target, &path, request.overwrite)?;
            emit(ToolEvent::Finished {
                success: true,
                message: format!("{} {} 已就绪", request.kind.label(), version.name),
                path: Some(path),
            });
        } else {
            if request.kind == ToolKind::Bundletool {
                emit(query_bundletool_versions(&client));
                return Ok(());
            }
            match versions(&client, request.kind, request.major) {
                Ok(versions) => emit(ToolEvent::Versions(versions)),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })();
    if let Err(message) = result {
        emit(ToolEvent::Finished {
            success: false,
            message,
            path: None,
        });
    }
}
fn agent(proxy: &str) -> Result<ureq::Agent, String> {
    let mut builder = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .https_only(true)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(30));
    if !proxy.trim().is_empty() {
        if !proxy.trim().starts_with("http://") {
            return Err("代理请填写 http://主机:端口（HTTP CONNECT / 混合端口）".into());
        }
        builder = builder.proxy(ureq::Proxy::new(proxy.trim()).map_err(|_| "代理地址无效")?);
    }
    Ok(builder.build())
}
fn json(client: &ureq::Agent, url: &str) -> Result<serde_json::Value, String> {
    client
        .get(url)
        .set("User-Agent", "AndroidTool")
        .call()
        .map_err(query_error)?
        .into_json()
        .map_err(|_| "版本列表响应无效".into())
}

fn query_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => {
            let limited = code == 429
                || (code == 403 && response.header("X-RateLimit-Remaining") == Some("0"));
            if limited {
                format!("官方接口限流 (HTTP {code})，请稍后重试")
            } else if code == 403 {
                "请求被拒绝 (HTTP 403)，未确认是 API 限流，请检查代理或网络访问策略".into()
            } else {
                format!("版本查询失败 (HTTP {code})")
            }
        }
        ureq::Error::Transport(error) => format!(
            "版本查询连接失败：{}；请检查代理、DNS 或 TLS 连接",
            error.kind()
        ),
    }
}

fn query_bundletool_versions(client: &ureq::Agent) -> ToolEvent {
    let api_error = match versions(client, ToolKind::Bundletool, 17) {
        Ok(versions) => return ToolEvent::Versions(versions),
        Err(error) => error,
    };
    emit(ToolEvent::Progress {
        message: "官方 API 不可用，正在尝试官方发布订阅源…".into(),
        fraction: None,
    });
    let feed_result = (|| {
        let response = client
            .get("https://github.com/google/bundletool/releases.atom")
            .set("User-Agent", "AndroidTool")
            .call()
            .map_err(query_error)?;
        let mut text = String::new();
        response
            .into_reader()
            .take(2 * 1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|_| "官方发布订阅源读取失败".to_owned())?;
        if text.len() > 2 * 1024 * 1024 {
            return Err("官方发布订阅源过大".into());
        }
        parse_bundletool_feed(&text)
    })();
    match feed_result {
        Ok(versions) => ToolEvent::VersionsFallback {
            message: format!(
                "已通过官方发布订阅源获取 {} 个在线版本（API 不可用）",
                versions.len()
            ),
            versions,
        },
        Err(feed_error) => ToolEvent::VersionsFallback {
            versions: bundled_bundletool_versions(),
            message: format!(
                "API：{api_error}；发布订阅源：{feed_error}；使用内置版本列表，下载仍需访问 GitHub"
            ),
        },
    }
}

fn parse_bundletool_feed(text: &str) -> Result<Vec<ToolVersion>, String> {
    let entries = regex::Regex::new(r"(?s)<entry(?:\s[^>]*)?>(.*?)</entry>").unwrap();
    let tags = regex::Regex::new(
        r#"https://github\.com/google/bundletool/releases/tag/([0-9]+\.[0-9]+\.[0-9]+)["<\s]"#,
    )
    .unwrap();
    let pinned = bundled_bundletool_versions();
    let mut versions: Vec<ToolVersion> = Vec::new();
    for entry in entries.captures_iter(text) {
        let Some(tag) = tags.captures(&entry[1]) else {
            continue;
        };
        let name = &tag[1];
        if versions.iter().any(|version| version.name == name) {
            continue;
        }
        versions.push(ToolVersion {
            name: name.into(),
            url: format!("https://github.com/google/bundletool/releases/download/{name}/bundletool-all-{name}.jar"),
            checksum: pinned.iter().find(|version| version.name == name).and_then(|version| version.checksum.clone()),
        });
    }
    if versions.is_empty() {
        return Err("官方发布订阅源未包含可识别的稳定版本".into());
    }
    Ok(versions)
}

fn bundled_bundletool_versions() -> Vec<ToolVersion> {
    [
        ("1.18.3", "a099cfa1543f55593bc2ed16a70a7c67fe54b1747bb7301f37fdfd6d91028e29"),
        ("1.18.2", "378b5434cd1378bef6b2bc527b8c7f0ff2584b273830335bce54d6d0813c8584"),
    ].into_iter().map(|(name, checksum)| ToolVersion {
        name: name.to_owned(),
        url: format!("https://github.com/google/bundletool/releases/download/{name}/bundletool-all-{name}.jar"),
        checksum: Some(checksum.to_owned()),
    }).collect()
}
fn platform() -> Result<(&'static str, &'static str), String> {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return Err("不支持当前 Java 下载架构".into());
    };
    Ok((os, arch))
}
fn versions(client: &ureq::Agent, kind: ToolKind, major: u8) -> Result<Vec<ToolVersion>, String> {
    emit(ToolEvent::Progress {
        message: "正在查询官方版本列表…".into(),
        fraction: None,
    });
    let mut result = Vec::new();
    match kind {
        ToolKind::Adb => {
            let os = if cfg!(windows) {
                "windows"
            } else if cfg!(target_os = "macos") {
                "darwin"
            } else {
                "linux"
            };
            result.push(ToolVersion {
                name: "最新稳定版".into(),
                url: format!(
                    "https://dl.google.com/android/repository/platform-tools-latest-{os}.zip"
                ),
                checksum: None,
            });
        }
        ToolKind::Bundletool => {
            let releases = json(
                client,
                "https://api.github.com/repos/google/bundletool/releases?per_page=50",
            )?;
            for release in releases.as_array().ok_or("bundletool 版本响应无效")? {
                if release["draft"] == true || release["prerelease"] == true {
                    continue;
                }
                if let Some(assets) = release["assets"].as_array() {
                    for asset in assets {
                        let name = asset["name"].as_str().unwrap_or_default();
                        if name.starts_with("bundletool-all-") && name.ends_with(".jar") {
                            if let Some(url) = asset["browser_download_url"].as_str() {
                                result.push(ToolVersion {
                                    name: release["tag_name"].as_str().unwrap_or(name).into(),
                                    url: url.into(),
                                    checksum: asset["digest"]
                                        .as_str()
                                        .and_then(|v| v.strip_prefix("sha256:"))
                                        .map(str::to_owned),
                                });
                            }
                        }
                    }
                }
            }
        }
        ToolKind::Java => {
            if ![17, 21, 25].contains(&major) {
                return Err("请选择 Java 17 / 21 / 25 LTS".into());
            }
            let (os, arch) = platform()?;
            let url = format!(
                "https://api.adoptium.net/v3/assets/feature_releases/{major}/ga?architecture={arch}&heap_size=normal&image_type=jre&jvm_impl=hotspot&os={os}&page_size=20&project=jdk&vendor=eclipse"
            );
            let releases = json(client, &url)?;
            for release in releases.as_array().ok_or("Java 版本响应无效")? {
                if let Some(binaries) = release["binaries"].as_array() {
                    for binary in binaries {
                        let package = &binary["package"];
                        if let (Some(url), Some(checksum)) =
                            (package["link"].as_str(), package["checksum"].as_str())
                        {
                            result.push(ToolVersion {
                                name: release["release_name"]
                                    .as_str()
                                    .unwrap_or("Temurin JRE")
                                    .into(),
                                url: url.into(),
                                checksum: Some(checksum.into()),
                            });
                        }
                    }
                }
            }
        }
    }
    if result.is_empty() {
        return Err("官方没有提供当前平台的可用版本".into());
    }
    Ok(result)
}
fn download(client: &ureq::Agent, version: &ToolVersion, destination: &Path) -> Result<(), String> {
    if !version.url.starts_with("https://") {
        return Err("拒绝非 HTTPS 下载地址".into());
    }
    let response = client
        .get(&version.url)
        .set("User-Agent", "AndroidTool")
        .call()
        .map_err(|_| "下载失败，请检查网络和代理".to_owned())?;
    let length = response
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok());
    let mut reader = response.into_reader();
    let mut file = File::create(destination).map_err(|e| e.to_string())?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0; 65536];
    let mut total = 0u64;
    let mut last = std::time::Instant::now() - Duration::from_secs(1);
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| "下载连接中断或超时".to_owned())?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        hash.update(&buffer[..count]);
        total += count as u64;
        if total > 1024 * 1024 * 1024 {
            return Err("下载超过 1 GiB 安全限制".into());
        }
        if last.elapsed() > Duration::from_millis(150) {
            emit(ToolEvent::Progress {
                message: format!(
                    "已下载 {}{}",
                    util::format_bytes(total),
                    length
                        .map(|n| format!(" / {}", util::format_bytes(n)))
                        .unwrap_or_default()
                ),
                fraction: length.filter(|n| *n > 0).map(|n| total as f32 / n as f32),
            });
            last = std::time::Instant::now();
        }
    }
    if length.is_some_and(|n| total != n) || total == 0 {
        return Err("下载文件不完整".into());
    }
    let digest: String = hash
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if version
        .checksum
        .as_ref()
        .is_some_and(|expected| !digest.eq_ignore_ascii_case(expected))
    {
        return Err("SHA-256 校验失败，文件未启用".into());
    }
    Ok(())
}
fn install(
    client: &ureq::Agent,
    kind: ToolKind,
    version: &ToolVersion,
    directory: &Path,
) -> Result<PathBuf, String> {
    let archive = directory.join(if version.url.ends_with(".tar.gz") {
        "download.tar.gz"
    } else {
        "download.zip"
    });
    download(client, version, &archive)?;
    let output = directory.join("content");
    util::ensure_dir(&output)?;
    emit(ToolEvent::Progress {
        message: "下载完成，正在校验并解压…".into(),
        fraction: None,
    });
    let path = if kind == ToolKind::Bundletool {
        let file = File::open(&archive).map_err(|e| e.to_string())?;
        let mut jar =
            zip::ZipArchive::new(file).map_err(|_| "bundletool 不是有效 JAR".to_owned())?;
        jar.by_name("META-INF/MANIFEST.MF")
            .map_err(|_| "bundletool JAR 缺少清单".to_owned())?;
        let path = output.join("bundletool.jar");
        fs::copy(&archive, &path).map_err(|e| e.to_string())?;
        path
    } else {
        if version.url.ends_with(".tar.gz") {
            return Err("当前仅支持下载 ZIP 格式的 Java；此平台请手动安装 Java".into());
        }
        extract_zip(&archive, &output)?;
        let name = if kind == ToolKind::Adb {
            if cfg!(windows) { "adb.exe" } else { "adb" }
        } else if cfg!(windows) {
            "java.exe"
        } else {
            "java"
        };
        let path = find_file(&output, name, 6).ok_or("解压完成，但未找到可执行文件")?;
        let expected_parent = if kind == ToolKind::Adb {
            "platform-tools"
        } else {
            "bin"
        };
        if path.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new(expected_parent)) {
            return Err("下载的工具目录结构不正确".into());
        }
        path
    };
    if kind != ToolKind::Bundletool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
        let arg = if kind == ToolKind::Java {
            "-version"
        } else {
            "version"
        };
        let result = util::command(&path)
            .arg(arg)
            .output()
            .map_err(|_| "下载的工具无法启动".to_owned())?;
        if !result.status.success() {
            return Err("工具版本验证失败，未启用".into());
        }
    }
    let _ = fs::remove_file(archive);
    Ok(path)
}
fn find_file(directory: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    for entry in fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();
        if entry.file_type().ok()?.is_dir() {
            if let Some(found) = find_file(&path, name, depth - 1) {
                return Some(found);
            }
        } else if entry.file_name() == name {
            return Some(path);
        }
    }
    None
}
fn extract_zip(path: &Path, destination: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(File::open(path).map_err(|e| e.to_string())?)
        .map_err(|_| "ZIP 文件无效".to_owned())?;
    let mut size = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let relative = entry.enclosed_name().ok_or("ZIP 包含越界路径")?;
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err("ZIP 包含不支持的符号链接".into());
        }
        size = size.saturating_add(entry.size());
        if size > 2 * 1024 * 1024 * 1024 {
            return Err("ZIP 解压超过安全限制".into());
        }
        let path = destination.join(relative);
        if entry.is_dir() {
            util::ensure_dir(&path)?;
        } else {
            util::ensure_dir(path.parent().ok_or("解压路径无效")?)?;
            let mut file = File::create(&path).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o777))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_validation() {
        assert!(agent("http://127.0.0.1:7890").is_ok());
        assert!(agent("").is_ok());
        assert!(agent("socks5://localhost:1080").is_err());
    }
    #[test]
    fn kinds_are_independent() {
        assert_eq!(ToolKind::ALL.map(ToolKind::index), [0, 1, 2]);
    }

    #[test]
    fn version_directory_names_cannot_escape() {
        assert_eq!(safe_version_name("jdk-21.0.9+10"), "jdk-21.0.9+10");
        assert_eq!(safe_version_name(".."), "unknown");
        assert!(!safe_version_name("../other").contains('/'));
    }

    #[test]
    fn replace_installed_version_only_after_confirmation() {
        let root =
            std::env::temp_dir().join(format!("androidtool-replace-test-{}", std::process::id()));
        let target = root.join("version");
        let staging = root.join(".androidtool-staging-test");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(target.join("bundletool.jar"), b"old").unwrap();
        fs::write(staging.join("bundletool.jar"), b"new").unwrap();
        assert!(
            finalize_install(&staging, &target, &staging.join("bundletool.jar"), false).is_err()
        );
        assert_eq!(fs::read(target.join("bundletool.jar")).unwrap(), b"old");
        let file =
            finalize_install(&staging, &target, &staging.join("bundletool.jar"), true).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"new");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fallback_versions_use_pinned_official_assets_and_checksums() {
        let versions = bundled_bundletool_versions();
        assert!(versions.len() >= 2);
        for version in versions {
            assert_eq!(
                version.url,
                format!(
                    "https://github.com/google/bundletool/releases/download/{0}/bundletool-all-{0}.jar",
                    version.name
                )
            );
            let checksum = version.checksum.unwrap();
            assert_eq!(checksum.len(), 64);
            assert!(checksum.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn release_feed_selects_only_stable_bundletool_tags() {
        let feed = r#"<feed>
          <entry><link href="https://github.com/google/bundletool/releases/tag/1.18.3"/></entry>
          <entry><link href="https://github.com/google/bundletool/releases/tag/1.18.3"/></entry>
          <entry><link href="https://github.com/google/bundletool/releases/tag/1.19.0-rc1"/></entry>
          <entry><link href="https://github.com/google/bundletool/releases/tag/1.17.2"/></entry>
          <entry><link href="https://example.com/google/bundletool/releases/tag/1.17.1"/></entry>
        </feed>"#;
        let versions = parse_bundletool_feed(feed).unwrap();
        assert_eq!(
            versions.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            ["1.18.3", "1.17.2"]
        );
        assert!(versions[0].checksum.is_some());
        assert!(versions[1].checksum.is_none());
        assert!(parse_bundletool_feed("<feed></feed>").is_err());
    }

    #[test]
    fn extraction_rejects_traversal_and_extracts_nested_files() {
        let root =
            std::env::temp_dir().join(format!("androidtool-zip-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let archive = root.join("archive.zip");
        let output = root.join("output");
        for (name, valid) in [("../escaped", false), ("jdk/bin/java.exe", true)] {
            let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"fixture").unwrap();
            writer.finish().unwrap();
            assert_eq!(extract_zip(&archive, &output).is_ok(), valid);
            assert!(!root.join("escaped").exists());
        }
        assert_eq!(
            fs::read(output.join("jdk/bin/java.exe")).unwrap(),
            b"fixture"
        );
        let target = root.canonicalize().unwrap();
        assert_eq!(
            target.parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        fs::remove_dir_all(target).unwrap();
    }
}
