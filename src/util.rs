use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
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

pub fn apk_library_dir() -> PathBuf {
    install_dir().join("apks")
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
    run_cancellable_limited(command, timeout, stop, usize::MAX)
}

pub fn run_cancellable_limited(
    command: &mut Command,
    timeout: Duration,
    stop: &AtomicBool,
    limit: usize,
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
        thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            let mut overflow = false;
            loop {
                let count = pipe.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                let kept = count.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&buffer[..kept]);
                overflow |= kept < count;
            }
            if overflow {
                return Err(std::io::Error::other("命令输出超过容量限制"));
            }
            Ok(bytes)
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
        #[cfg(windows)]
        {
            // bundletool may have spawned ADB children that inherit the output pipes.
            let _ = self::command("taskkill.exe")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
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

pub fn run_streaming_limited<F>(
    command: &mut Command,
    timeout: Duration,
    stop: &AtomicBool,
    limit: usize,
    mut on_line: F,
) -> Result<Output, String>
where
    F: FnMut(bool, String),
{
    if stop.load(Ordering::Relaxed) {
        return Err("操作已取消".to_owned());
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动命令: {error}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel::<(bool, std::io::Result<Vec<u8>>)>();
    let spawn_reader = |mut pipe: Box<dyn Read + Send>, is_stderr, tx: mpsc::Sender<_>| {
        thread::spawn(move || {
            let mut reader = std::io::BufReader::new(&mut pipe);
            loop {
                let mut line = Vec::new();
                match std::io::BufRead::read_until(&mut reader, b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send((is_stderr, Ok(line))).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send((is_stderr, Err(error)));
                        break;
                    }
                }
            }
        })
    };
    let stdout_reader = spawn_reader(Box::new(stdout), false, tx.clone());
    let stderr_reader = spawn_reader(Box::new(stderr), true, tx.clone());
    drop(tx);

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let started = Instant::now();
    let mut reader_done = [false; 2];
    let mut exited = None;
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Err("操作已取消".to_owned());
        }
        if started.elapsed() >= timeout {
            break Err(format!("命令超时（{} 秒）", timeout.as_secs()));
        }
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok((is_stderr, Ok(mut bytes))) => {
                while bytes
                    .last()
                    .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
                {
                    bytes.pop();
                }
                let bytes_count = stdout_bytes.len().saturating_add(stderr_bytes.len());
                if bytes.len().saturating_add(bytes_count).saturating_add(1) > limit {
                    break Err("命令输出超过容量限制".to_owned());
                }
                let destination = if is_stderr {
                    &mut stderr_bytes
                } else {
                    &mut stdout_bytes
                };
                destination.extend_from_slice(&bytes);
                destination.push(b'\n');
                let line = String::from_utf8_lossy(&bytes).into_owned();
                if !line.trim().is_empty() {
                    on_line(is_stderr, line);
                }
            }
            Ok((_, Err(error))) => break Err(format!("读取命令输出失败: {error}")),
            Err(RecvTimeoutError::Disconnected) => {
                reader_done = [true; 2];
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        match child.try_wait() {
            Ok(Some(status)) => exited = Some(status),
            Ok(None) => {}
            Err(error) => break Err(format!("无法读取命令进程状态: {error}")),
        }
        if reader_done == [true; 2] {
            if let Some(status) = exited {
                break Ok(Output {
                    status,
                    stdout: stdout_bytes,
                    stderr: stderr_bytes,
                });
            }
        }
    };
    if result.is_err() {
        #[cfg(windows)]
        {
            let _ = self::command("taskkill.exe")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    result
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
            Ok("lines") => {
                println!("stdout one");
                eprintln!("stderr one");
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

    #[test]
    fn streaming_command_delivers_stdout_and_stderr_lines() {
        let mut lines = Vec::new();
        let output = run_streaming_limited(
            &mut fixture_command("lines"),
            Duration::from_secs(5),
            &AtomicBool::new(false),
            1024,
            |stderr, line| lines.push((stderr, line)),
        )
        .unwrap();
        assert!(output.status.success());
        assert!(
            lines
                .iter()
                .any(|(stderr, line)| !stderr && line.contains("stdout one"))
        );
        assert!(
            lines
                .iter()
                .any(|(stderr, line)| *stderr && line.contains("stderr one"))
        );
    }

    #[test]
    fn streaming_command_keeps_output_larger_than_individual_lines_under_limit() {
        let output = run_streaming_limited(
            &mut fixture_command("output"),
            Duration::from_secs(5),
            &AtomicBool::new(false),
            300_000,
            |_, _| {},
        )
        .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() >= 131072);
        assert!(output.stderr.len() >= 131072);
    }
}
