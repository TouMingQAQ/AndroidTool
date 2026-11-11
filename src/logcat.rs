use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use regex::Regex;

use crate::adb::Adb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Assert,
}

impl LogLevel {
    pub fn from_char(value: char) -> Self {
        match value.to_ascii_uppercase() {
            'V' => Self::Verbose,
            'D' => Self::Debug,
            'W' => Self::Warn,
            'E' => Self::Error,
            'F' => Self::Fatal,
            'A' => Self::Assert,
            _ => Self::Info,
        }
    }

    pub fn letter(self) -> char {
        match self {
            Self::Verbose => 'V',
            Self::Debug => 'D',
            Self::Info => 'I',
            Self::Warn => 'W',
            Self::Error => 'E',
            Self::Fatal => 'F',
            Self::Assert => 'A',
        }
    }

    pub fn all() -> [Self; 7] {
        [
            Self::Verbose,
            Self::Debug,
            Self::Info,
            Self::Warn,
            Self::Error,
            Self::Fatal,
            Self::Assert,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub process: String,
    pub timestamp: String,
    pub pid: String,
    pub tid: String,
    pub level: LogLevel,
    pub tag: String,
    pub message: String,
    pub raw: String,
}

impl LogEntry {
    pub fn threadtime_line(&self) -> String {
        if self.timestamp.is_empty() {
            return self.raw.clone();
        }
        format!(
            "{} {:>5} {:>5} {} {}: {}",
            self.timestamp,
            self.pid,
            self.tid,
            self.level.letter(),
            self.tag,
            self.message
        )
    }

    pub fn search_text(&self) -> String {
        format!("{} {}", self.tag, self.message).to_ascii_lowercase()
    }
}

#[derive(Debug)]
pub enum LogcatEvent {
    Batch(Vec<LogEntry>),
    Processes {
        foreground: Option<String>,
        names: HashMap<String, String>,
    },
    ProcessError(String),
    Error(String),
}

pub struct LogcatSession {
    pub serial: String,
    receiver: mpsc::Receiver<LogcatEvent>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<std::process::Child>>>,
    scan_requested: Arc<AtomicBool>,
}

impl LogcatSession {
    pub fn start(adb_path: std::path::PathBuf, serial: String) -> Self {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_child = Arc::clone(&child);
        let thread_serial = serial.clone();
        let scan_requested = Arc::new(AtomicBool::new(false));
        let monitor_scan = Arc::clone(&scan_requested);
        let monitor_adb = Adb::new(adb_path.clone());
        let monitor_serial = serial.clone();
        let monitor_sender = sender.clone();
        let monitor_stop = Arc::clone(&stop);
        thread::spawn(move || {
            while !monitor_stop.load(Ordering::Relaxed) {
                let result = (|| {
                    let activity = monitor_adb.shell_cancellable(
                        &monitor_serial,
                        &["dumpsys", "activity", "activities"],
                        &monitor_stop,
                    )?;
                    let mut foreground = parse_foreground_package(&activity);
                    if foreground.is_none() {
                        let window = monitor_adb.shell_cancellable(
                            &monitor_serial,
                            &["dumpsys", "window", "windows"],
                            &monitor_stop,
                        )?;
                        foreground = parse_foreground_package(&window);
                    }
                    let processes = monitor_adb.shell_cancellable(
                        &monitor_serial,
                        &["ps", "-A", "-o", "PID,NAME"],
                        &monitor_stop,
                    )?;
                    Ok::<_, String>(LogcatEvent::Processes {
                        foreground,
                        names: parse_process_names(&processes),
                    })
                })();
                if monitor_sender
                    .send(result.unwrap_or_else(LogcatEvent::ProcessError))
                    .is_err()
                {
                    break;
                }
                for _ in 0..20 {
                    if monitor_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if monitor_scan.swap(false, Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        });
        thread::spawn(move || {
            run_logcat(adb_path, thread_serial, sender, thread_stop, thread_child);
        });
        Self {
            serial,
            receiver,
            stop,
            child,
            scan_requested,
        }
    }

    pub fn try_recv(&self) -> Option<LogcatEvent> {
        self.receiver.try_recv().ok()
    }

    pub fn scan_now(&self) {
        self.scan_requested.store(true, Ordering::Relaxed);
    }
}

impl Drop for LogcatSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn run_logcat(
    adb_path: std::path::PathBuf,
    serial: String,
    sender: mpsc::Sender<LogcatEvent>,
    stop: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<std::process::Child>>>,
) {
    let adb = Adb::new(adb_path);
    let mut child = match adb.spawn_logcat(&serial) {
        Ok(child) => child,
        Err(error) => {
            let _ = sender.send(LogcatEvent::Error(error));
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = sender.send(LogcatEvent::Error("logcat 没有输出管道".to_owned()));
            return;
        }
    };

    if let Ok(mut slot) = child_slot.lock() {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        *slot = Some(child);
    }

    let (line_sender, lines) = mpsc::sync_channel(1024);
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if line_sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut batch = Vec::with_capacity(64);
    let mut last_flush = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if !line.trim().is_empty() => batch.push(parse_line(&line)),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ => {}
        }
        if !batch.is_empty()
            && (batch.len() >= 32 || last_flush.elapsed() >= Duration::from_millis(100))
        {
            if sender
                .send(LogcatEvent::Batch(std::mem::take(&mut batch)))
                .is_err()
            {
                break;
            }
            last_flush = std::time::Instant::now();
        }
    }
    if !batch.is_empty() {
        let _ = sender.send(LogcatEvent::Batch(batch));
    }
}

pub fn parse_line(line: &str) -> LogEntry {
    if let Some(entry) = parse_threadtime(line) {
        return entry;
    }
    if let Some(entry) = parse_android_studio(line) {
        return entry;
    }
    if let Some(entry) = parse_short_format(line) {
        return entry;
    }

    LogEntry {
        process: String::new(),
        timestamp: String::new(),
        pid: String::new(),
        tid: String::new(),
        level: LogLevel::Info,
        tag: "raw".to_owned(),
        message: line.to_owned(),
        raw: line.to_owned(),
    }
}

pub fn parse_foreground_package(text: &str) -> Option<String> {
    for marker in [
        "topResumedActivity",
        "mResumedActivity",
        "mFocusedActivity",
        "mCurrentFocus",
        "mFocusedApp",
    ] {
        for line in text.lines().filter(|line| line.contains(marker)) {
            for token in line.split_whitespace() {
                if let Some((package, _)) = token.split_once('/') {
                    let package = package.trim_start_matches('{');
                    if package.contains('.')
                        && package
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
                    {
                        return Some(package.to_owned());
                    }
                }
            }
        }
    }
    None
}

pub fn parse_process_names(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?;
            let name = fields.next()?;
            pid.parse::<u32>().ok()?;
            Some((pid.to_owned(), name.to_owned()))
        })
        .collect()
}

pub fn process_matches_package(process: &str, package: &str) -> bool {
    process == package
        || process
            .strip_prefix(package)
            .is_some_and(|suffix| suffix.starts_with(':'))
}

fn threadtime_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^(\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d+)\s+(\d+)\s+(\d+)\s+([VDIWEFA])\s+(.+?):\s?(.*)$",
        )
        .expect("valid logcat regex")
    })
}

fn full_date_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^(\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d+)\s+(\d+)\s+(\d+)\s+([VDIWEFA])\s+(.+?):\s?(.*)$",
        )
        .expect("valid full date logcat regex")
    })
}

fn android_studio_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^(\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\.\d+)\s+(\d+)-(\d+)/(\S+)\s+([VDIWEFA])/([^:]+):\s?(.*)$",
        )
        .expect("valid Android Studio regex")
    })
}

fn short_format_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^([VDIWEFA])/([^(]+)\(\s*(\d+)\):\s?(.*)$").expect("valid logcat short regex")
    })
}

fn parse_threadtime(line: &str) -> Option<LogEntry> {
    let captures = threadtime_regex()
        .captures(line)
        .or_else(|| full_date_regex().captures(line))?;
    Some(LogEntry {
        process: String::new(),
        timestamp: captures.get(1)?.as_str().to_owned(),
        pid: captures.get(2)?.as_str().to_owned(),
        tid: captures.get(3)?.as_str().to_owned(),
        level: LogLevel::from_char(captures.get(4)?.as_str().chars().next()?),
        tag: captures.get(5)?.as_str().trim().to_owned(),
        message: captures
            .get(6)
            .map(|value| value.as_str())
            .unwrap_or("")
            .to_owned(),
        raw: line.to_owned(),
    })
}

fn parse_android_studio(line: &str) -> Option<LogEntry> {
    let captures = android_studio_regex().captures(line)?;
    Some(LogEntry {
        process: captures.get(4)?.as_str().to_owned(),
        timestamp: captures.get(1)?.as_str().to_owned(),
        pid: captures.get(2)?.as_str().to_owned(),
        tid: captures.get(3)?.as_str().to_owned(),
        level: LogLevel::from_char(captures.get(5)?.as_str().chars().next()?),
        tag: captures.get(6)?.as_str().trim().to_owned(),
        message: captures
            .get(7)
            .map(|value| value.as_str())
            .unwrap_or("")
            .to_owned(),
        raw: line.to_owned(),
    })
}

fn parse_short_format(line: &str) -> Option<LogEntry> {
    let captures = short_format_regex().captures(line)?;
    Some(LogEntry {
        process: String::new(),
        timestamp: String::new(),
        pid: captures.get(3)?.as_str().to_owned(),
        tid: String::new(),
        level: LogLevel::from_char(captures.get(1)?.as_str().chars().next()?),
        tag: captures.get(2)?.as_str().trim().to_owned(),
        message: captures
            .get(4)
            .map(|value| value.as_str())
            .unwrap_or("")
            .to_owned(),
        raw: line.to_owned(),
    })
}

pub fn import_file(path: &Path) -> Result<Vec<LogEntry>, String> {
    let file = File::open(path)
        .map_err(|error| format!("无法打开日志文件 {}: {error}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|error| format!("读取日志失败: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(parse_line(&line));
    }
    Ok(entries)
}

pub fn export_file(path: &Path, entries: &[&LogEntry]) -> Result<(), String> {
    let file = File::create(path)
        .map_err(|error| format!("无法创建日志文件 {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "--------- beginning of exported logcat (Android Studio threadtime) ---------"
    )
    .map_err(|error| error.to_string())?;
    for entry in entries {
        writeln!(writer, "{}", entry.threadtime_line()).map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_threadtime() {
        let entry = parse_line("11-11 12:34:56.789  1234  5678 E AndroidRuntime: crash");
        assert_eq!(entry.pid, "1234");
        assert_eq!(entry.tid, "5678");
        assert_eq!(entry.level, LogLevel::Error);
        assert_eq!(entry.tag, "AndroidRuntime");
        assert_eq!(entry.message, "crash");
    }

    #[test]
    fn parses_android_studio_export() {
        let entry =
            parse_line("2026-11-11 12:34:56.789 1234-5678/com.example.app I/MyTag: hello studio");
        assert_eq!(entry.timestamp, "2026-11-11 12:34:56.789");
        assert_eq!(entry.pid, "1234");
        assert_eq!(entry.process, "com.example.app");
        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.tag, "MyTag");
        assert_eq!(entry.message, "hello studio");
    }

    #[test]
    fn identifies_foreground_and_package_processes() {
        assert_eq!(
            parse_foreground_package(
                "topResumedActivity=ActivityRecord{108 u0 com.example.app/.Main t2}"
            ),
            Some("com.example.app".to_owned())
        );
        assert_eq!(
            parse_foreground_package(
                "mCurrentFocus=Window{75 u0 com.android.launcher/com.android.launcher.Launcher}"
            ),
            Some("com.android.launcher".to_owned())
        );
        assert_eq!(parse_foreground_package("no focused activity"), None);
        let names =
            parse_process_names("PID NAME\n123 com.example.app\n456 com.example.app:worker\n");
        assert!(process_matches_package(&names["123"], "com.example.app"));
        assert!(process_matches_package(&names["456"], "com.example.app"));
        assert!(!process_matches_package(
            "com.example.app2",
            "com.example.app"
        ));
    }

    #[test]
    fn falls_back_to_raw_line() {
        let entry = parse_line("continuation of a multiline stack trace");
        assert_eq!(entry.tag, "raw");
        assert_eq!(entry.message, "continuation of a multiline stack trace");
    }

    #[test]
    #[ignore = "requires a connected Android device"]
    fn live_device_reports_foreground_process_and_log_batches() {
        let path = crate::util::find_on_path("adb").expect("ADB on PATH");
        let adb = Adb::new(&path);
        let serial = adb
            .devices()
            .unwrap()
            .into_iter()
            .find(|device| device.is_online())
            .expect("online device")
            .serial;
        let session = LogcatSession::start(path, serial);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut processes = false;
        let mut logs = false;
        while std::time::Instant::now() < deadline {
            match session.try_recv() {
                Some(LogcatEvent::Processes {
                    foreground: Some(package),
                    names,
                }) => {
                    assert!(
                        names
                            .values()
                            .any(|name| process_matches_package(name, &package))
                    );
                    processes = true;
                }
                Some(LogcatEvent::Batch(batch)) => logs |= !batch.is_empty(),
                Some(LogcatEvent::Error(error) | LogcatEvent::ProcessError(error)) => {
                    panic!("{error}")
                }
                _ => {}
            }
            if processes && logs {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("process metadata: {processes}, log batches: {logs}");
    }
}
