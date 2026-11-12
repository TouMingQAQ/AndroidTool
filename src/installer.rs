use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use chrono::Local;
use ring::rand::{SecureRandom, SystemRandom};
use zip::ZipArchive;

use crate::adb::Adb;
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
}

#[derive(Debug)]
pub enum InstallEvent {
    Progress {
        progress: f32,
        stage: String,
    },
    Log(String),
    Finished {
        success: bool,
        message: String,
        converted: Option<PathBuf>,
    },
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
    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    util::converted_dir().join(format!("{stem}-{timestamp}-universal.apk"))
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
    if !request.package.is_file() {
        return Err(format!("安装包不存在：{}", request.package.display()));
    }
    let kind = PackageKind::detect(&request.package);
    let name = util::file_name(&request.package);
    progress(sender, 0.04, "识别安装包");
    log(sender, format!("安装包：{name}"));
    log(sender, format!("类型：{}", kind.label()));

    match kind {
        PackageKind::Apk => {
            install_apk(&request, &request.package, sender)?;
            Ok(None)
        }
        PackageKind::Aab => {
            let converted = convert_aab(&request, sender)?;
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
    progress(sender, 0.72, "安装到设备");
    log(
        sender,
        format!("ADB install → {} ({})", request.serial, apk.display()),
    );
    let adb = Adb::new(&request.adb_path);
    let output = adb.install(&request.serial, apk)?;
    log(sender, output);
    progress(sender, 1.0, "安装完成");
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
    let stem = request
        .package
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("application");
    let temp_apks = util::cache_dir().join(format!(
        "{}-{}.apks",
        stem,
        Local::now().format("%Y%m%d-%H%M%S")
    ));

    progress(sender, 0.16, "bundletool 构建 Universal APK");
    log(sender, "正在调用 bundletool build-apks …".to_owned());
    let mut command = util::command(&request.java_path);
    command
        .arg("-jar")
        .arg(&request.bundletool_path)
        .arg("build-apks")
        .arg(format!("--bundle={}", request.package.display()))
        .arg(format!("--output={}", temp_apks.display()))
        .arg("--mode=universal")
        .arg("--overwrite")
        .arg(format!("--ks={}", signing.resolved_keystore().display()))
        .arg(format!("--ks-key-alias={}", signing.key_alias.trim()));
    let passwords = SigningPasswordFiles::new(signing)?;
    passwords.configure(&mut command);
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("无法启动 bundletool: {error}"))?;
    drop(passwords);

    let output_text = signing.redact(&util::output_text(&output));
    if !output.status.success() {
        let _ = fs::remove_file(&temp_apks);
        return Err(format!("bundletool 转换失败：\n{output_text}"));
    }
    if !output_text.is_empty() {
        log(sender, output_text);
    }

    progress(sender, 0.52, "提取 universal.apk");
    util::ensure_dir(&util::converted_dir())?;
    let output_path = request
        .output_apk
        .clone()
        .unwrap_or_else(|| converted_output_path(&request.package));
    if let Some(parent) = output_path.parent() {
        util::ensure_dir(parent)?;
    }
    extract_universal_apk(&temp_apks, &output_path)?;
    let _ = fs::remove_file(&temp_apks);

    log(sender, format!("Universal APK：{}", output_path.display()));
    progress(sender, 0.64, "转换完成");
    Ok(output_path)
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
            .arg(format!("--ks-pass=file:{}", self.store.path.display()))
            .arg(format!("--key-pass=file:{}", self.key.path.display()));
    }
}

fn extract_universal_apk(apks_path: &Path, output_apk: &Path) -> Result<(), String> {
    let file = File::open(apks_path)
        .map_err(|error| format!("无法打开 APKS {}: {error}", apks_path.display()))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("APKS 不是有效 ZIP: {error}"))?;
    let mut entry = archive
        .by_name("universal.apk")
        .map_err(|_| "APKS 中未找到 universal.apk，请确认已使用 universal 模式".to_owned())?;
    let mut output = File::create(output_apk)
        .map_err(|error| format!("无法创建 APK {}: {error}", output_apk.display()))?;
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
    Ok(())
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
    log(sender, "正在调用 bundletool install-apks …".to_owned());
    let adb_folder = request
        .adb_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![adb_folder];
    paths.extend(std::env::split_paths(&path_env));
    let joined_path = std::env::join_paths(paths).map_err(|error| error.to_string())?;

    let output = util::command(&request.java_path)
        .arg("-jar")
        .arg(&request.bundletool_path)
        .arg("install-apks")
        .arg(format!("--apks={}", request.package.display()))
        .arg(format!("--device-id={}", request.serial))
        .env("PATH", joined_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("无法启动 bundletool: {error}"))?;

    let text = util::output_text(&output);
    if !output.status.success() {
        return Err(format!("APKS 安装失败：\n{text}"));
    }
    log(sender, text);
    progress(sender, 1.0, "安装完成");
    Ok(())
}

fn progress(sender: &mpsc::Sender<InstallEvent>, value: f32, stage: impl Into<String>) {
    let _ = sender.send(InstallEvent::Progress {
        progress: value,
        stage: stage.into(),
    });
}

fn log(sender: &mpsc::Sender<InstallEvent>, line: impl Into<String>) {
    let _ = sender.send(InstallEvent::Log(line.into()));
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .arg(&source)
            .arg(&passwords.store.path)
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

        let _ = fs::remove_dir_all(root);
    }
}
