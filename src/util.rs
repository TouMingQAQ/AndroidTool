use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn settings_path() -> PathBuf {
    install_dir().join("androidtool.settings.json")
}

pub fn cache_dir() -> PathBuf {
    install_dir().join("cache")
}

pub fn converted_dir() -> PathBuf {
    install_dir().join("converted")
}

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    hide_console(&mut command);
    command
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

pub fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<Output, String> {
    command(program)
        .args(args)
        .output()
        .map_err(|error| format!("无法启动命令: {error}"))
}

pub fn run_cancellable(
    command: &mut Command,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<Output, String> {
    if stop.load(Ordering::Relaxed) {
        return Err("操作已取消".to_owned());
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 ADB 命令: {error}"))?;
    // Drain both pipes concurrently so a full pipe cannot block cancellation.
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let read_pipe = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let stdout = read_pipe(Box::new(stdout));
    let stderr = read_pipe(Box::new(stderr));
    let started = Instant::now();
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Err("操作已取消".to_owned());
        }
        if started.elapsed() >= timeout {
            break Err(format!("ADB 命令超时（{} 秒）", timeout.as_secs()));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(30)),
            Err(error) => break Err(format!("无法读取 ADB 进程状态: {error}")),
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let read_result = |handle: thread::JoinHandle<std::io::Result<Vec<u8>>>| {
        handle
            .join()
            .map_err(|_| "读取 ADB 输出的线程异常退出".to_owned())?
            .map_err(|error| format!("无法读取 ADB 输出: {error}"))
    };
    let stdout = read_result(stdout);
    let stderr = read_result(stderr);
    Ok(Output {
        status: result?,
        stdout: stdout?,
        stderr: stderr?,
    })
}

pub fn output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stdout.trim(), stderr.trim()),
        (false, true) => stdout.trim().to_owned(),
        (true, false) => stderr.trim().to_owned(),
        (true, true) => String::new(),
    }
}

pub fn find_on_path(binary: &str) -> Option<PathBuf> {
    let candidate_names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{binary}.exe"),
            format!("{binary}.cmd"),
            format!("{binary}.bat"),
            binary.to_owned(),
        ]
    } else {
        vec![binary.to_owned()]
    };

    let path = std::env::var_os("PATH")?;
    for folder in std::env::split_paths(&path) {
        for name in &candidate_names {
            let candidate = folder.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn detect_adb(configured: &str) -> Option<PathBuf> {
    detect_adb_in(&install_dir(), configured).or_else(|| find_on_path("adb"))
}

fn detect_adb_in(directory: &Path, configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        let path = if path.is_absolute() {
            path
        } else {
            directory.join(path)
        };
        if path.is_file() {
            return Some(path);
        }
    }

    let exe_name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let portable_candidates = [
        directory.join("platform-tools").join(exe_name),
        directory.join(exe_name),
        directory.join("adb").join(exe_name),
    ];
    for candidate in portable_candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

pub fn detect_bundletool(configured: &str) -> Option<PathBuf> {
    detect_bundletool_in(&install_dir(), configured)
}

fn configured_file(directory: &Path, configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        let path = if path.is_absolute() {
            path
        } else {
            directory.join(path)
        };
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn detect_bundletool_in(directory: &Path, configured: &str) -> Option<PathBuf> {
    if let Some(path) = configured_file(directory, configured) {
        return Some(path);
    }
    let candidates = [
        directory.join("bundletool.jar"),
        directory.join("tools").join("bundletool.jar"),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

pub fn detect_java(configured: &str) -> Option<PathBuf> {
    if let Some(path) = configured_file(&install_dir(), configured) {
        return Some(path);
    }
    find_on_path("java")
}

pub fn portable_path(path: &Path) -> String {
    path.strip_prefix(install_dir())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut result: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    result.push('…');
    result
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package")
        .to_owned()
}

pub fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| format!("无法创建目录 {}: {error}", path.display()))
}

pub fn open_folder(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = command("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = command("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = command("xdg-open").arg(path).spawn();
    }
}

pub fn reveal_file(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = command("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = command("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(parent) = path.parent() {
            open_folder(parent);
        }
    }
}

pub fn normalize_remote_path(path: &str) -> String {
    let mut result = path.trim().replace('\\', "/");
    if result.is_empty() {
        return "/sdcard".to_owned();
    }
    if !result.starts_with('/') {
        result.insert(0, '/');
    }
    while result.len() > 1 && result.ends_with('/') {
        result.pop();
    }
    result
}

pub fn quote_remote_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn parent_remote_path(path: &str) -> String {
    let normalized = normalize_remote_path(path);
    if normalized == "/" {
        return "/".to_owned();
    }
    normalized
        .rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                "/".to_owned()
            } else {
                parent.to_owned()
            }
        })
        .unwrap_or_else(|| "/".to_owned())
}

pub fn join_remote_path(base: &str, name: &str) -> String {
    format!(
        "{}/{}",
        normalize_remote_path(base),
        name.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_relative_paths_resolve_from_install_directory() {
        let root =
            std::env::temp_dir().join(format!("androidtool-relative-test-{}", std::process::id()));
        let first = root.join("first");
        let second = root.join("moved");
        let relative = Path::new("tools/bundletool/1.18.3/content/bundletool.jar");
        let java = Path::new("tools/jdk/jdk-21/content/bin/java.exe");
        fs::create_dir_all(first.join(relative).parent().unwrap()).unwrap();
        fs::create_dir_all(first.join(java).parent().unwrap()).unwrap();
        fs::write(first.join(relative), b"jar").unwrap();
        fs::write(first.join(java), b"java").unwrap();
        fs::rename(&first, &second).unwrap();
        assert_eq!(
            detect_bundletool_in(&second, &relative.to_string_lossy()),
            Some(second.join(relative))
        );
        assert_eq!(
            configured_file(&second, &java.to_string_lossy()),
            Some(second.join(java))
        );
        assert_eq!(
            configured_file(&first, &second.join(java).to_string_lossy()),
            Some(second.join(java))
        );
        fs::remove_dir_all(root).unwrap();
    }
    use std::io::Write;
    use std::sync::Arc;

    #[test]
    fn adb_resolution_is_portable_and_respects_manual_override() {
        let directory = std::env::temp_dir().join(format!(
            "androidtool-adb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let tools = directory.join("platform-tools");
        fs::create_dir_all(&tools).unwrap();
        let name = if cfg!(windows) { "adb.exe" } else { "adb" };
        let bundled = tools.join(name);
        let manual = directory.join(name);
        fs::write(&bundled, []).unwrap();
        fs::write(&manual, []).unwrap();
        assert_eq!(detect_adb_in(&directory, ""), Some(bundled.clone()));
        assert_eq!(detect_adb_in(&directory, name), Some(manual.clone()));
        assert_eq!(
            detect_adb_in(&directory, &manual.to_string_lossy()),
            Some(manual.clone())
        );
        assert_eq!(
            detect_adb_in(&directory, "missing-adb"),
            Some(bundled.clone())
        );
        fs::remove_file(bundled).unwrap();
        fs::remove_file(manual).unwrap();
        fs::remove_dir(tools).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn quotes_remote_shell_arguments() {
        assert_eq!(
            quote_remote_arg("/sdcard/my folder/a'b;echo x"),
            "'/sdcard/my folder/a'\\''b;echo x'"
        );
    }

    // Run this test function in a child process to exercise actual pipe and process handling.
    #[test]
    fn command_fixture() {
        match std::env::var("ANDROIDTOOL_COMMAND_FIXTURE").as_deref() {
            Ok("sleep") => thread::sleep(Duration::from_secs(30)),
            Ok("output") => {
                std::io::stdout().write_all(&vec![b'x'; 131072]).unwrap();
                std::io::stderr().write_all(&vec![b'y'; 131072]).unwrap();
            }
            _ => {}
        }
    }

    fn fixture_command(mode: &str) -> Command {
        let mut command = command(std::env::current_exe().unwrap());
        command
            .args(["--exact", "util::tests::command_fixture", "--nocapture"])
            .env("ANDROIDTOOL_COMMAND_FIXTURE", mode);
        command
    }

    #[test]
    fn command_timeout_terminates_the_child() {
        let started = Instant::now();
        let result = run_cancellable(
            &mut fixture_command("sleep"),
            Duration::from_millis(300),
            &AtomicBool::new(false),
        );
        assert!(result.unwrap_err().contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn cancellation_terminates_a_running_child() {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let cancellation = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            thread_stop.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = run_cancellable(
            &mut fixture_command("sleep"),
            Duration::from_secs(30),
            &stop,
        );
        cancellation.join().unwrap();
        assert!(result.unwrap_err().contains("取消"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn command_drains_both_output_pipes_without_deadlocking() {
        let output = run_cancellable(
            &mut fixture_command("output"),
            Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() >= 131072);
        assert!(output.stderr.len() >= 131072);
    }
}
