use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant};

use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorOptions {
    pub fps: u8,
    pub max_size: u16,
    pub bit_rate_mbps: u8,
}

impl MirrorOptions {
    fn arguments(self, serial: &str) -> Vec<String> {
        vec![
            format!("--serial={serial}"),
            format!("--max-fps={}", self.fps.clamp(1, 60)),
            format!("--max-size={}", self.max_size.clamp(640, 2560)),
            format!("--video-bit-rate={}M", self.bit_rate_mbps.clamp(1, 32)),
            "--video-codec=h264".to_owned(),
            "--render-fit=letterbox".to_owned(),
            "--no-audio".to_owned(),
            "--print-fps".to_owned(),
            "--window-title=AndroidTool - scrcpy".to_owned(),
        ]
    }
}

pub fn detect_scrcpy() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "scrcpy.exe"
    } else {
        "scrcpy"
    };
    let bundled = util::install_dir().join("scrcpy").join(name);
    bundled
        .is_file()
        .then_some(bundled)
        .or_else(|| util::find_on_path("scrcpy"))
}

pub struct MirrorSession {
    pub serial: String,
    pub options: MirrorOptions,
    pub embedded: bool,
    #[cfg(windows)]
    started: Instant,
    #[cfg(windows)]
    window: crate::mirror_window::EmbeddedWindow,
    child: Child,
    receiver: Receiver<String>,
    lines: VecDeque<String>,
    drainers: Vec<thread::JoinHandle<()>>,
}

impl MirrorSession {
    #[cfg(test)]
    pub fn start(adb: &Path, serial: String, options: MirrorOptions) -> Result<Self, String> {
        Self::start_with_mode(adb, serial, options, false)
    }

    pub fn start_with_mode(
        adb: &Path,
        serial: String,
        options: MirrorOptions,
        embedded: bool,
    ) -> Result<Self, String> {
        let embedded = embedded && cfg!(windows);
        let path = detect_scrcpy()
            .ok_or("未找到 scrcpy，请使用包含 scrcpy 的完整便携包，或将 scrcpy 加入 PATH。")?;
        let mut command = util::command(&path);
        command
            .args(options.arguments(&serial))
            .env("ADB", adb)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if embedded {
            // Keep startup offscreen until the renderer is attached to the page.
            command.args([
                "--window-borderless",
                "--no-window-aspect-ratio-lock",
                "--window-x=-16000",
                "--window-y=-16000",
                "--window-width=640",
                "--window-height=480",
            ]);
        }
        let server = path
            .parent()
            .unwrap_or(Path::new("."))
            .join("scrcpy-server");
        if server.is_file() {
            command.env("SCRCPY_SERVER_PATH", server);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 scrcpy：{error}"))?;
        let (sender, receiver) = mpsc::channel();
        let mut drainers = Vec::new();
        for pipe in [
            Box::new(child.stdout.take().expect("stdout")) as Box<dyn std::io::Read + Send>,
            Box::new(child.stderr.take().expect("stderr")),
        ] {
            let sender = sender.clone();
            drainers.push(thread::spawn(move || {
                for line in BufReader::new(pipe).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }));
        }
        Ok(Self {
            serial,
            options,
            embedded,
            #[cfg(windows)]
            started: Instant::now(),
            #[cfg(windows)]
            window: Default::default(),
            child,
            receiver,
            lines: VecDeque::new(),
            drainers,
        })
    }

    pub fn poll(&mut self) -> Result<Option<String>, String> {
        self.drain_output();
        match self.child.try_wait().map_err(|error| error.to_string())? {
            None => Ok(None),
            Some(status) => {
                for handle in self.drainers.drain(..) {
                    let _ = handle.join();
                }
                self.drain_output();
                if status.success() {
                    Ok(Some("scrcpy 投屏已结束。".to_owned()))
                } else {
                    Err(format!(
                        "scrcpy 已退出（{status}）：\n{}",
                        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
                    ))
                }
            }
        }
    }

    fn drain_output(&mut self) {
        while let Ok(line) = self.receiver.try_recv() {
            self.lines.push_back(line);
            while self.lines.len() > 40 {
                self.lines.pop_front();
            }
        }
    }

    pub fn latest_status(&self) -> &str {
        self.lines
            .back()
            .map(String::as_str)
            .unwrap_or("正在启动视频流…")
    }

    pub fn is_attached(&self) -> bool {
        #[cfg(windows)]
        {
            self.embedded && self.window.is_attached()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    #[cfg(windows)]
    pub fn sync_embedded(
        &mut self,
        parent: windows_sys::Win32::Foundation::HWND,
        bounds: [i32; 4],
        visible: bool,
    ) -> Result<(), String> {
        if self.embedded
            && !self.window.sync(self.child.id(), parent, bounds, visible)?
            && self.started.elapsed() > Duration::from_secs(20)
        {
            return Err("等待 scrcpy 视频窗口超时，请检查连接或切换独立窗口模式。".to_owned());
        }
        Ok(())
    }
}

impl Drop for MirrorSession {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a connected Android device, bundled scrcpy and an interactive desktop"]
    fn live_embedded_scrcpy_decodes_resizes_hides_and_stops() {
        use std::ptr::null_mut;
        use windows_sys::Win32::UI::WindowsAndMessaging::*;

        struct Host(windows_sys::Win32::Foundation::HWND);
        impl Drop for Host {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let title: Vec<u16> = "AndroidTool embedded integration test\0"
            .encode_utf16()
            .collect();
        let host = Host(unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                50,
                50,
                900,
                700,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        });
        assert!(!host.0.is_null());
        let adb = util::detect_adb("").unwrap();
        let serial = crate::adb::Adb::new(&adb)
            .devices()
            .unwrap()
            .into_iter()
            .find(|device| device.is_online())
            .expect("online phone")
            .serial;
        let mut session = MirrorSession::start_with_mode(
            &adb,
            serial,
            MirrorOptions {
                fps: 60,
                max_size: 1280,
                bit_rate_mbps: 4,
            },
            true,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut attached = false;
        while Instant::now() < deadline {
            unsafe {
                let mut message = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            assert!(
                session.poll().unwrap().is_none(),
                "scrcpy exited: {:?}",
                session.lines
            );
            attached = session
                .window
                .sync(session.child.id(), host.0, [20, 30, 600, 500], true)
                .unwrap();
            if attached && session.lines.iter().any(|line| line.contains("Texture")) {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
        assert!(
            attached && session.lines.iter().any(|line| line.contains("Texture")),
            "{:?}",
            session.lines
        );
        // Pump past the initial texture: scrcpy may resize itself after attachment.
        let settle = Instant::now() + Duration::from_millis(500);
        while Instant::now() < settle {
            unsafe {
                let mut message = std::mem::zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            session.poll().unwrap();
            session
                .sync_embedded(host.0, [20, 30, 600, 500], true)
                .unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        session.window.verify(host.0, [20, 30, 600, 500], true);
        // Reproduce native auto-resizes with unchanged host bounds, both orientations.
        for drift in [[0, 0, 400, 1000], [0, 0, 1000, 400]] {
            session.window.simulate_scrcpy_resize(drift);
            session
                .sync_embedded(host.0, [20, 30, 600, 500], true)
                .unwrap();
            session.window.verify(host.0, [20, 30, 600, 500], true);
        }
        session
            .sync_embedded(host.0, [40, 20, 420, 360], true)
            .unwrap();
        session.window.verify(host.0, [40, 20, 420, 360], true);
        session
            .sync_embedded(host.0, [40, 20, 420, 360], false)
            .unwrap();
        session.window.verify(host.0, [40, 20, 420, 360], false);
        session
            .sync_embedded(host.0, [40, 20, 420, 360], true)
            .unwrap();
        session.window.verify(host.0, [40, 20, 420, 360], true);
        let id = session.child.id();
        drop(session);
        let output =
            util::run("tasklist", &["/FI", &format!("PID eq {id}"), "/FO", "CSV"]).unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout).contains("scrcpy.exe"));
    }

    #[test]
    fn constructs_device_specific_low_latency_video_options() {
        let args = MirrorOptions {
            fps: 60,
            max_size: 1920,
            bit_rate_mbps: 8,
        }
        .arguments("phone with spaces");
        assert!(args.contains(&"--serial=phone with spaces".to_owned()));
        assert!(args.contains(&"--max-fps=60".to_owned()));
        assert!(args.contains(&"--video-bit-rate=8M".to_owned()));
        assert!(args.contains(&"--video-codec=h264".to_owned()));
        assert!(args.contains(&"--render-fit=letterbox".to_owned()));
        assert!(!args.iter().any(|arg| arg.contains("video-buffer")));
    }

    #[test]
    #[ignore = "requires a connected Android device and bundled scrcpy"]
    fn live_scrcpy_session_starts_decodes_and_stops() {
        let adb = util::find_on_path("adb").unwrap();
        let serial = crate::adb::Adb::new(&adb)
            .devices()
            .unwrap()
            .into_iter()
            .find(|device| device.is_online())
            .expect("online phone")
            .serial;
        let mut session = MirrorSession::start(
            &adb,
            serial,
            MirrorOptions {
                fps: 60,
                max_size: 1280,
                bit_rate_mbps: 4,
            },
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut decoded = false;
        while std::time::Instant::now() < deadline {
            assert!(
                session.poll().unwrap().is_none(),
                "scrcpy exited unexpectedly"
            );
            if session.lines.iter().any(|line| line.contains("Texture")) {
                decoded = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
        let id = session.child.id();
        assert!(decoded, "video did not start: {:?}", session.lines);
        drop(session);
        #[cfg(windows)]
        {
            let output =
                util::run("tasklist", &["/FI", &format!("PID eq {id}"), "/FO", "CSV"]).unwrap();
            assert!(!String::from_utf8_lossy(&output.stdout).contains("scrcpy.exe"));
        }
    }
}
