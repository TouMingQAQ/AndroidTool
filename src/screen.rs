use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use crate::adb::Adb;

#[derive(Debug)]
pub enum ScreenEvent {
    Frame {
        width: usize,
        height: usize,
        screen_width: usize,
        screen_height: usize,
        rgba: Vec<u8>,
    },
    Error(String),
}

pub struct ScreenSession {
    pub serial: String,
    pub fps: u8,
    receiver: Receiver<ScreenEvent>,
    stop: Arc<AtomicBool>,
}

impl ScreenSession {
    pub fn start(adb_path: std::path::PathBuf, serial: String, fps: u8) -> Self {
        let fps = fps.clamp(1, 60);
        let (sender, receiver) = mpsc::sync_channel(2);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_serial = serial.clone();
        thread::spawn(move || {
            streaming_loop(adb_path, thread_serial, fps, sender, thread_stop);
        });
        Self {
            serial,
            fps,
            receiver,
            stop,
        }
    }

    pub fn try_recv(&self) -> Option<ScreenEvent> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for ScreenSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn streaming_loop(
    adb_path: std::path::PathBuf,
    serial: String,
    fps: u8,
    sender: mpsc::SyncSender<ScreenEvent>,
    stop: Arc<AtomicBool>,
) {
    let adb = Adb::new(adb_path);
    let frame_time = Duration::from_secs_f32(1.0 / fps as f32);
    let mut last_error = String::new();
    let mut last_error_at = Instant::now() - Duration::from_secs(10);

    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        match adb.screenshot(&serial) {
            Ok(bytes) => {
                let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                    .or_else(|_| image::load_from_memory(&bytes));
                match decoded {
                    Ok(image) => {
                        let screen_width = image.width() as usize;
                        let screen_height = image.height() as usize;
                        let image = if image.width() > 1440 || image.height() > 1440 {
                            image.thumbnail(1440, 1440)
                        } else {
                            image
                        };
                        let rgba = image.to_rgba8();
                        let event = ScreenEvent::Frame {
                            width: rgba.width() as usize,
                            height: rgba.height() as usize,
                            screen_width,
                            screen_height,
                            rgba: rgba.into_raw(),
                        };
                        match sender.try_send(event) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                        last_error.clear();
                    }
                    Err(error) => {
                        let message = format!("屏幕帧解析失败: {error}");
                        send_error(&sender, &mut last_error, &mut last_error_at, message, &stop);
                    }
                }
            }
            Err(error) => {
                let message = format!("无法获取屏幕画面: {error}");
                send_error(&sender, &mut last_error, &mut last_error_at, message, &stop);
            }
        }

        let elapsed = started.elapsed();
        if elapsed < frame_time {
            let mut remaining = frame_time - elapsed;
            while remaining > Duration::ZERO && !stop.load(Ordering::Relaxed) {
                let step = remaining.min(Duration::from_millis(50));
                thread::sleep(step);
                remaining = remaining.saturating_sub(step);
            }
        }
    }
}

fn send_error(
    sender: &mpsc::SyncSender<ScreenEvent>,
    last_error: &mut String,
    last_error_at: &mut Instant,
    message: String,
    stop: &AtomicBool,
) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    if last_error != &message || last_error_at.elapsed() > Duration::from_secs(4) {
        let _ = sender.try_send(ScreenEvent::Error(message.clone()));
        *last_error = message;
        *last_error_at = Instant::now();
    }
}
