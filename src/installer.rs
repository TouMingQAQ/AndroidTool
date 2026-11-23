use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, atomic::AtomicBool};
use std::thread;
use std::time::Duration;

use chrono::Local;
use ring::rand::{SecureRandom, SystemRandom};
use zip::ZipArchive;

use crate::config::SigningProfile;
use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Apk,
    Aab,
    Apks,
    Unknown,
}

impl PackageKind {
    pub fn detect(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "apk" => Self::Apk,
            "aab" => Self::Aab,
            "apks" => Self::Apks,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Apk => "APK",
            Self::Aab => "AAB",
            Self::Apks => "APKS",
            Self::Unknown => "未知",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub adb_path: PathBuf,
    pub java_path: PathBuf,
    pub bundletool_path: PathBuf,
    pub serial: String,
    pub package: PathBuf,
    pub convert_only: bool,
    pub output_apk: Option<PathBuf>,
    pub signing: Option<SigningProfile>,
    pub cancel: Option<Arc<AtomicBool>>,
}

#[derive(Debug)]
pub enum InstallEvent {
    Progress {
        progress: f32,
        stage: String,
    },
    Log {
        level: InstallLogLevel,
        line: String,
    },
    Converted(PathBuf),
    Finished {
        success: bool,
        message: String,
        converted: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallLogLevel {
    Step,
    Success,
    Warning,
    Error,
    Output,
}

pub struct InstallSession {
    receiver: Receiver<InstallEvent>,
}

impl InstallSession {
    pub fn start(request: InstallRequest) -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || run_install(request, sender));
        Self { receiver }
    }

    pub fn try_recv(&self) -> Option<InstallEvent> {
        self.receiver.try_recv().ok()
    }
}

pub fn converted_output_path(package: &Path) -> PathBuf {
    let stem = package
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("application");
    let timestamp = Local::now().format("%Y%m%d-%H%M%S-%f");
    util::apk_library_dir().join(format!("{stem}-{timestamp}-universal.apk"))
}

fn run_install(request: InstallRequest, sender: mpsc::Sender<InstallEvent>) {
    let result = execute(request.clone(), &sender);
    match result {
        Ok(converted) => {
            let message = if request.convert_only {
                match converted.as_ref() {
                    Some(path) => format!("转换完成：{}", path.display()),
                    None => "转换完成".to_owned(),
                }
            } else {
                "安装成功".to_owned()
            };
            let _ = sender.send(InstallEvent::Finished {
                success: true,
                message,
                converted,
            });
        }
        Err(error) => {
            let _ = sender.send(InstallEvent::Finished {
                success: false,
                message: error,
                converted: None,
            });
        }
    }
}

fn execute(
    request: InstallRequest,
    sender: &mpsc::Sender<InstallEvent>,
) -> Result<Option<PathBuf>, String> {
    if request
        .cancel
        .as_ref()
        .is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err("操作已取消".into());
    }
    if !request.package.is_file() {
        return Err(format!("安装包不存在：{}", request.package.display()));
    }
    let kind = PackageKind::detect(&request.package);
    let name = util::file_name(&request.package);
    progress(sender, 0.04, "检查安装包");
    log(sender, InstallLogLevel::Step, format!("检查安装包：{name}"));
    log(
        sender,
        InstallLogLevel::Output,
        format!("类型：{}", kind.label()),
    );

    match kind {
        PackageKind::Apk => {
            install_apk(&request, &request.package, sender)?;
            Ok(None)
        }
        PackageKind::Aab => {
            let converted = convert_aab(&request, sender)?;
            let _ = sender.send(InstallEvent::Converted(converted.clone()));
            if request.convert_only {
                Ok(Some(converted))
            } else {
                install_apk(&request, &converted, sender)?;
                Ok(Some(converted))
            }
        }
        PackageKind::Apks => {
            if request.convert_only {
                return Err("APKS 是设备相关集合，请直接安装；仅转换模式仅支持 AAB".to_owned());
            }
            install_apks(&request, sender)?;
            Ok(None)
        }
        PackageKind::Unknown => Err("无法识别安装包。支持 .apk、.aab、.apks".to_owned()),
    }
}

fn install_apk(
    request: &InstallRequest,
    apk: &Path,
    sender: &mpsc::Sender<InstallEvent>,
) -> Result<(), String> {
    if request.serial.trim().is_empty() {
        return Err("没有选择目标设备".to_owned());
    }
    if !request.adb_path.is_file() {
        return Err("未找到 ADB，请先在设置中配置".to_owned());
    }
    progress(sender, 0.78, "安装到设备");
    log(
        sender,
        InstallLogLevel::Step,
        format!("安装到设备：{}", request.serial),
    );
    let mut command = util::command(&request.adb_path);
    command
        .args(["-s", &request.serial, "install", "-r", "-d"])
        .arg(apk);
    let default_stop = AtomicBool::new(false);
    let stop = request.cancel.as_deref().unwrap_or(&default_stop);
    let output = util::run_streaming_limited(
        &mut command,
        Duration::from_secs(600),
        stop,
        16 * 1024 * 1024,
        |is_stderr, line| {
            log(
                sender,
                if is_stderr {
                    InstallLogLevel::Warning
                } else {
                    InstallLogLevel::Output
                },
                line,
            );
        },
    )?;
    let text = util::output_text(&output);
    if !output.status.success() || !text.contains("Success") {
        return Err(if text.is_empty() {
            "ADB 安装失败".to_owned()
        } else {
            text
        });
    }
    progress(sender, 1.0, "安装完成");
    log(sender, InstallLogLevel::Success, "APK 安装成功");
    Ok(())
}

fn convert_aab(
    request: &InstallRequest,
    sender: &mpsc::Sender<InstallEvent>,
) -> Result<PathBuf, String> {
    let signing = request
        .signing
        .as_ref()
        .ok_or("AAB 转换需要先选择签名密钥")?;
    signing.validate()?;
    log(
        sender,
        InstallLogLevel::Step,
        format!(
            "使用签名密钥：{}（别名：{}）",
            signing.name, signing.key_alias
        ),
    );
    if !request.java_path.is_file() {
        return Err("AAB 转换需要 Java 17+，请先配置 java".to_owned());
    }
    if !request.bundletool_path.is_file() {
        return Err("AAB 转换需要 bundletool.jar，请在设置页下载或选择".to_owned());
    }
    util::ensure_dir(&util::cache_dir())?;
    let output_path = request
        .output_apk
        .clone()
        .unwrap_or_else(|| converted_output_path(&request.package));
    if output_path.exists() {
        return Err(format!(
            "{} 已存在。为避免覆盖已有 APK，请选择其他输出位置。",
            output_path.display()
        ));
    }
    let scratch = ConversionScratch::new()?;
    let temp_apks = scratch.path.join("bundle.apks");
    log(
        sender,
        InstallLogLevel::Output,
        format!("转换缓存：{}", scratch.path.display()),
    );

    progress(sender, 0.12, "生成通用 APK（bundletool）");
    log(
        sender,
        InstallLogLevel::Step,
        "开始生成通用 APK（bundletool build-apks）",
    );
    let mut command = util::command(&request.java_path);
    command
        .arg(format!(
            "-Djava.io.tmpdir={}",
            java_path_argument(&scratch.path)
        ))
        .arg("-jar")
        .arg(java_path_argument(&request.bundletool_path))
        .arg("build-apks")
        .arg(format!("--bundle={}", java_path_argument(&request.package)))
        .arg(format!("--output={}", java_path_argument(&temp_apks)))
        .arg("--mode=universal")
        .arg("--overwrite")
        .arg(format!(
            "--ks={}",
            java_path_argument(&signing.resolved_keystore())
        ))
        .arg(format!("--ks-key-alias={}", signing.key_alias.trim()));
    let passwords = SigningPasswordFiles::new(signing)?;
    passwords.configure(&mut command);
    let output = match run_install_command(&mut command, request, sender, Some(signing)) {
        Ok(output) => output,
        Err(error) => {
            let _ = fs::remove_file(&temp_apks);
            return Err(error);
        }
    };
    drop(passwords);

    if request
        .cancel
        .as_ref()
        .is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err("操作已取消".into());
    }

    let output_text = signing.redact(&util::output_text(&output));
    if !output.status.success() || !temp_apks.is_file() {
        log(sender, InstallLogLevel::Error, "bundletool 转换失败");
        if looks_like_scratch_write_failure(&output_text) {
            return Err(format!(
                "bundletool 无法写入转换缓存（{}），请检查磁盘空间与写入权限。\n{output_text}",
                scratch.path.display()
            ));
        }
        return Err(format!("bundletool 转换失败：\n{output_text}"));
    }

    log(
        sender,
        InstallLogLevel::Success,
        "bundletool 已完成通用 APK 构建",
    );
    progress(sender, 0.72, "提取 universal.apk");
    log(
        sender,
        InstallLogLevel::Step,
        "从 APKS 归档提取 universal.apk",
    );
    util::ensure_dir(&util::apk_library_dir())?;
    if let Some(parent) = output_path.parent() {
        util::ensure_dir(parent)?;
    }
    let universal_apk = scratch.path.join("universal.apk");
    extract_universal_apk(&temp_apks, &universal_apk)?;
    if let Err(error) = fs::remove_file(&temp_apks) {
        log(
            sender,
            InstallLogLevel::Warning,
            format!("无法删除临时 APKS：{error}"),
        );
    }
    progress(sender, 0.74, "调整 APK 签名兼容性");
    log(
        sender,
        InstallLogLevel::Step,
        "使用同一密钥重新签名 Universal APK，保留 v2/v3 并禁用可能导致系统整数溢出的 APK verity",
    );
    let apk = sign_compatible_apk(request, signing, &scratch.path, &universal_apk, sender)?;
    if request
        .cancel
        .as_ref()
        .is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err("操作已取消".into());
    }
    publish_apk(&apk, &output_path)?;

    log(
        sender,
        InstallLogLevel::Success,
        format!("APK 已生成：{}", output_path.display()),
    );
    progress(sender, 0.78, "AAB 转换完成");
    Ok(output_path)
}

fn sign_compatible_apk(
    request: &InstallRequest,
    signing: &SigningProfile,
    scratch: &Path,
    input: &Path,
    sender: &mpsc::Sender<InstallEvent>,
) -> Result<PathBuf, String> {
    let helper = scratch.join("androidtool-signing.jar");
    let library = scratch.join("apksig-8.13.0.jar");
    fs::write(
        &helper,
        include_bytes!("../assets/signing/androidtool-signing.jar"),
    )
    .and_then(|_| {
        fs::write(
            &library,
            include_bytes!("../assets/signing/apksig-8.13.0.jar"),
        )
    })
    .map_err(|error| format!("无法准备 APK 签名组件：{error}"))?;
    let classpath = std::env::join_paths([
        PathBuf::from(java_path_argument(&helper)),
        PathBuf::from(java_path_argument(&library)),
    ])
    .map_err(|error| format!("APK 签名组件路径无效：{error}"))?;
    let output_apk = scratch.join("universal-compatible.apk");
    let passwords = SigningPasswordFiles::new(signing)?;
    let mut command = util::command(&request.java_path);
    command
        .arg(format!("-Djava.io.tmpdir={}", java_path_argument(scratch)))
        .arg("-cp")
        .arg(classpath)
        .arg("AndroidToolSigner")
        .arg(java_path_argument(input))
        .arg(java_path_argument(&output_apk))
        .arg(java_path_argument(&signing.resolved_keystore()))
        .arg(signing.key_alias.trim())
        .arg(java_path_argument(&passwords.store.path))
        .arg(java_path_argument(&passwords.key.path));
    let output = run_install_command(&mut command, request, sender, Some(signing))?;
    if !output.status.success() || !output_apk.is_file() {
        return Err(format!(
            "APK 兼容签名或校验失败：\n{}",
            signing.redact(&util::output_text(&output))
        ));
    }
    log(
        sender,
        InstallLogLevel::Success,
        "APK v2/v3 签名及签名证书校验通过（APK verity 已关闭）",
    );
    Ok(output_apk)
}

fn publish_apk(source: &Path, destination: &Path) -> Result<(), String> {
    // Reserve the destination exclusively, including when it appeared during conversion.
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| format!("无法创建 APK {}: {error}", destination.display()))?;
    let result = (|| -> std::io::Result<()> {
        std::io::copy(&mut File::open(source)?, &mut output)?;
        output.flush()
    })();
    drop(output);
    if let Err(error) = result {
        let _ = fs::remove_file(destination);
        return Err(format!("保存 APK 失败：{error}"));
    }
    Ok(())
}

// Rust canonicalize returns verbatim Windows paths, which Java's WindowsPathParser rejects.
// Convert only at the Java boundary; retain canonical paths for Rust I/O and cleanup checks.
fn java_path_argument(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(dos) = path.strip_prefix(r"\\?\") {
            let bytes = dos.as_bytes();
            if bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'\\'
            {
                return dos.to_owned();
            }
        }
    }
    path.into_owned()
}

// Adapted from AAB2APK's per-conversion Java scratch directory and write failure diagnosis.
fn looks_like_scratch_write_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("unable to sign apk")
        && [
            "filedispatcherimpl.write0",
            "filechannelimpl.write",
            "randomaccessdatasink",
        ]
        .iter()
        .any(|frame| lower.contains(frame))
}

struct ConversionScratch {
    path: PathBuf,
    root: PathBuf,
}

impl ConversionScratch {
    fn new() -> Result<Self, String> {
        util::ensure_dir(&util::cache_dir())?;
        let root = fs::canonicalize(util::cache_dir()).map_err(|error| error.to_string())?;
        let mut random = [0; 16];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "无法生成转换缓存目录名")?;
        let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = root.join(format!("aab-{name}"));
        fs::create_dir(&path).map_err(|error| format!("无法创建转换缓存：{error}"))?;
        Ok(Self { path, root })
    }
}

impl Drop for ConversionScratch {
    fn drop(&mut self) {
        // Resolve the final target before recursive removal and require our cache directory.
        if let Ok(resolved) = fs::canonicalize(&self.path) {
            if resolved.parent() == Some(self.root.as_path())
                && resolved
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("aab-"))
            {
                let _ = fs::remove_dir_all(resolved);
            }
        }
    }
}

struct PasswordFile {
    path: PathBuf,
    _file: File,
}

impl PasswordFile {
    fn new(password: &str) -> Result<Self, String> {
        if password.contains(['\r', '\n']) {
            return Err("bundletool 密码文件不支持换行符".into());
        }
        let mut random = [0u8; 16];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| "无法生成密码文件名")?;
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("androidtool-signing-{suffix}.tmp"));
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Delete on handle close also covers an application crash.
            options.custom_flags(0x04000000).share_mode(0x1 | 0x4);
        }
        let file = options
            .open(&path)
            .map_err(|error| format!("无法创建临时密码文件: {error}"))?;
        let mut secret = Self { path, _file: file };
        secret
            ._file
            .write_all(password.as_bytes())
            .map_err(|_| "无法写入临时密码文件")?;
        secret._file.flush().map_err(|_| "无法刷新临时密码文件")?;
        Ok(secret)
    }
}

impl Drop for PasswordFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct SigningPasswordFiles {
    store: PasswordFile,
    key: PasswordFile,
}

impl SigningPasswordFiles {
    fn new(signing: &SigningProfile) -> Result<Self, String> {
        Ok(Self {
            store: PasswordFile::new(&signing.store_password)?,
            key: PasswordFile::new(if signing.key_password.is_empty() {
                &signing.store_password
            } else {
                &signing.key_password
            })?,
        })
    }

    fn configure(&self, command: &mut std::process::Command) {
        command
            .arg(format!(
                "--ks-pass=file:{}",
                java_path_argument(&self.store.path)
            ))
            .arg(format!(
                "--key-pass=file:{}",
                java_path_argument(&self.key.path)
            ));
    }
}

fn extract_universal_apk(apks_path: &Path, output_apk: &Path) -> Result<(), String> {
    let file = File::open(apks_path)
        .map_err(|error| format!("无法打开 APKS {}: {error}", apks_path.display()))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("APKS 不是有效 ZIP: {error}"))?;
    let entry_name = archive
        .file_names()
        .find(|name| name.eq_ignore_ascii_case("universal.apk"))
        .or_else(|| {
            archive
                .file_names()
                .find(|name| name.to_ascii_lowercase().ends_with("/universal.apk"))
        })
        .ok_or("APKS 中未找到 universal.apk，请确认已使用 universal 模式")?
        .to_owned();
    let mut entry = archive
        .by_name(&entry_name)
        .map_err(|error| error.to_string())?;
    if entry.size() == 0 {
        return Err("APKS 中的 universal.apk 为空".into());
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_apk)
        .map_err(|error| format!("无法创建 APK {}: {error}", output_apk.display()))?;
    let result = (|| {
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = entry
                .read(&mut buffer)
                .map_err(|error| format!("读取 universal.apk 失败: {error}"))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|error| format!("写入 APK 失败: {error}"))?;
        }
        output
            .flush()
            .map_err(|error| format!("刷新 APK 失败: {error}"))
    })();
    drop(output);
    if result.is_err() {
        let _ = fs::remove_file(output_apk);
    }
    result
}

fn install_apks(
    request: &InstallRequest,
    sender: &mpsc::Sender<InstallEvent>,
) -> Result<(), String> {
    if request.serial.trim().is_empty() {
        return Err("没有选择目标设备".to_owned());
    }
    if !request.java_path.is_file() || !request.bundletool_path.is_file() {
        return Err("安装 APKS 需要 Java 与 bundletool".to_owned());
    }
    progress(sender, 0.35, "安装 APKS");
    log(
        sender,
        InstallLogLevel::Step,
        "开始安装 APKS（bundletool install-apks）",
    );
    let adb_folder = request
        .adb_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![adb_folder];
    paths.extend(std::env::split_paths(&path_env));
    let joined_path = std::env::join_paths(paths).map_err(|error| error.to_string())?;

    let mut command = util::command(&request.java_path);
    command
        .arg("-jar")
        .arg(java_path_argument(&request.bundletool_path))
        .arg("install-apks")
        .arg(format!("--apks={}", java_path_argument(&request.package)))
        .arg(format!("--device-id={}", request.serial))
        .env("PATH", joined_path);
    let output = run_install_command(&mut command, request, sender, None)?;

    let text = util::output_text(&output);
    if !output.status.success() {
        return Err(format!("APKS 安装失败：\n{text}"));
    }
    progress(sender, 1.0, "安装完成");
    log(sender, InstallLogLevel::Success, "APKS 安装成功");
    Ok(())
}

fn run_install_command(
    command: &mut std::process::Command,
    request: &InstallRequest,
    sender: &mpsc::Sender<InstallEvent>,
    signing: Option<&SigningProfile>,
) -> Result<std::process::Output, String> {
    let default_stop = AtomicBool::new(false);
    let stop = request.cancel.as_deref().unwrap_or(&default_stop);
    util::run_streaming_limited(
        command,
        Duration::from_secs(1800),
        stop,
        16 * 1024 * 1024,
        |is_stderr, line| {
            let line = signing.map_or(line.clone(), |profile| profile.redact(&line));
            log(
                sender,
                if is_stderr {
                    InstallLogLevel::Warning
                } else {
                    InstallLogLevel::Output
                },
                line,
            );
        },
    )
}

fn progress(sender: &mpsc::Sender<InstallEvent>, value: f32, stage: impl Into<String>) {
    let _ = sender.send(InstallEvent::Progress {
        progress: value,
        stage: stage.into(),
    });
}

fn log(sender: &mpsc::Sender<InstallEvent>, level: InstallLogLevel, line: impl Into<String>) {
    let _ = sender.send(InstallEvent::Log {
        level,
        line: line.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_apk_without_overwriting_and_cleans_failed_output() {
        let scratch = ConversionScratch::new().unwrap();
        let source = scratch.path.join("source.apk");
        let output = scratch.path.join("published.apk");
        fs::write(&source, b"verified-apk").unwrap();
        publish_apk(&source, &output).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"verified-apk");

        fs::write(&source, b"replacement").unwrap();
        assert!(publish_apk(&source, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"verified-apk");

        let failed_output = scratch.path.join("failed.apk");
        assert!(publish_apk(&scratch.path.join("missing.apk"), &failed_output).is_err());
        assert!(!failed_output.exists());
    }

    #[test]
    fn java_paths_preserve_normal_paths() {
        for path in [
            r"E:\AndroidTool\cache\bundle.apks",
            "relative/中文 path.aab",
        ] {
            assert_eq!(java_path_argument(Path::new(path)), path);
        }
    }

    #[cfg(windows)]
    #[test]
    fn java_paths_remove_verbatim_disk_and_unc_prefixes() {
        for (input, expected) in [
            (
                r"\\?\E:\AndroidTool\cache\aab-test\bundle.apks",
                r"E:\AndroidTool\cache\aab-test\bundle.apks",
            ),
            (r"\\?\C:\中文 目录\signing.jks", r"C:\中文 目录\signing.jks"),
            (
                r"\\?\UNC\server\share\中文 path\bundle.apks",
                r"\\server\share\中文 path\bundle.apks",
            ),
        ] {
            assert_eq!(java_path_argument(Path::new(input)), expected);
        }
    }

    #[test]
    fn signing_password_files_are_private_ephemeral_and_not_in_arguments() {
        let signing = SigningProfile {
            store_password: "store-secret-密碼".into(),
            ..Default::default()
        };
        let paths;
        {
            let passwords = SigningPasswordFiles::new(&signing).unwrap();
            paths = [passwords.store.path.clone(), passwords.key.path.clone()];
            assert_eq!(
                fs::read_to_string(&paths[0]).unwrap(),
                signing.store_password
            );
            assert_eq!(
                fs::read_to_string(&paths[1]).unwrap(),
                signing.store_password
            );
            let mut command = util::command("java");
            passwords.configure(&mut command);
            let args: Vec<_> = command.get_args().map(|v| v.to_string_lossy()).collect();
            assert_eq!(args.len(), 2);
            assert!(args[0].starts_with("--ks-pass=file:"));
            assert!(args[1].starts_with("--key-pass=file:"));
            assert!(!format!("{command:?}").contains(&signing.store_password));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&paths[0]).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
        assert!(paths.iter().all(|p| !p.exists()));
    }

    #[test]
    fn signing_password_files_support_separate_passwords_and_reject_newlines() {
        let signing = SigningProfile {
            store_password: "store".into(),
            key_password: "private".into(),
            ..Default::default()
        };
        let passwords = SigningPasswordFiles::new(&signing).unwrap();
        assert_eq!(fs::read_to_string(&passwords.key.path).unwrap(), "private");
        assert!(PasswordFile::new("line\nbreak").is_err());
    }

    #[test]
    #[ignore = "requires Java 17+ source launcher"]
    fn java_can_read_bundletool_password_files() {
        let signing = SigningProfile {
            store_password: "java-read-test-密碼".into(),
            ..Default::default()
        };
        let passwords = SigningPasswordFiles::new(&signing).unwrap();
        let source = passwords.store.path.with_extension("java");
        fs::write(&source, "import java.nio.file.*; class ReadPassword { public static void main(String[] args) throws Exception { System.out.write(Files.readAllBytes(Path.of(args[0]))); } }").unwrap();
        let output = util::command("java")
            .arg(format!(
                "-Djava.io.tmpdir={}",
                java_path_argument(&fs::canonicalize(source.parent().unwrap()).unwrap())
            ))
            .arg(java_path_argument(&fs::canonicalize(&source).unwrap()))
            .arg(java_path_argument(
                &fs::canonicalize(&passwords.store.path).unwrap(),
            ))
            .output()
            .unwrap();
        let _ = fs::remove_file(source);
        assert!(output.status.success(), "{}", util::output_text(&output));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            signing.store_password
        );
    }

    #[test]
    fn detects_supported_package_kinds() {
        assert_eq!(PackageKind::detect(Path::new("app.apk")), PackageKind::Apk);
        assert_eq!(PackageKind::detect(Path::new("app.AAB")), PackageKind::Aab);
        assert_eq!(
            PackageKind::detect(Path::new("app.apks")),
            PackageKind::Apks
        );
        assert_eq!(
            PackageKind::detect(Path::new("notes.txt")),
            PackageKind::Unknown
        );
    }

    #[test]
    fn extracts_universal_apk_from_apks() {
        let root =
            std::env::temp_dir().join(format!("androidtool-installer-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let apks_path = root.join("bundle.apks");
        let output_path = root.join("universal.apk");

        let file = File::create(&apks_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("universal.apk", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"test-apk").unwrap();
        archive.finish().unwrap();

        extract_universal_apk(&apks_path, &output_path).unwrap();
        assert_eq!(fs::read(&output_path).unwrap(), b"test-apk");
        assert!(extract_universal_apk(&apks_path, &output_path).is_err());
        assert_eq!(fs::read(&output_path).unwrap(), b"test-apk");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extracts_nested_universal_and_detects_scratch_write_failure() {
        let root =
            std::env::temp_dir().join(format!("androidtool-nested-apks-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("bundle.apks");
        let output_path = root.join("universal.apk");
        let mut archive = zip::ZipWriter::new(File::create(&archive_path).unwrap());
        archive
            .start_file(
                "output/universal.apk",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(b"apk-data").unwrap();
        archive.finish().unwrap();
        extract_universal_apk(&archive_path, &output_path).unwrap();
        assert_eq!(fs::read(&output_path).unwrap(), b"apk-data");
        assert!(looks_like_scratch_write_failure(
            "Unable to sign APK\nFileDispatcherImpl.write0"
        ));
        assert!(!looks_like_scratch_write_failure(
            "Unable to sign APK: wrong password"
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
