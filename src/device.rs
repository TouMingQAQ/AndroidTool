use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::adb::{Adb, DeviceInfo};

#[derive(Debug, Clone, Default)]
pub struct DeviceSnapshot {
    pub devices: Vec<DeviceInfo>,
    pub error: Option<String>,
}

pub struct DeviceWatcher {
    receiver: Receiver<DeviceSnapshot>,
    stop: Arc<AtomicBool>,
}

impl DeviceWatcher {
    pub fn start(adb_path: std::path::PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        thread::spawn(move || watch_loop(adb_path, sender, stop_thread));
        Self { receiver, stop }
    }

    pub fn try_recv(&self) -> Option<DeviceSnapshot> {
        let mut latest = None;
        while let Ok(snapshot) = self.receiver.try_recv() {
            latest = Some(snapshot);
        }
        latest
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch_loop(adb_path: std::path::PathBuf, sender: Sender<DeviceSnapshot>, stop: Arc<AtomicBool>) {
    let adb = Adb::new(adb_path);
    let mut last_error: Option<String> = None;
    let _ = adb.start_server();

    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let snapshot = match adb.devices() {
            Ok(devices) => {
                last_error = None;
                DeviceSnapshot {
                    devices,
                    error: None,
                }
            }
            Err(error) => {
                last_error = Some(error.clone());
                DeviceSnapshot {
                    devices: Vec::new(),
                    error: Some(error),
                }
            }
        };

        if sender.send(snapshot).is_err() {
            break;
        }

        let elapsed = started.elapsed();
        let wait = Duration::from_millis(2400).saturating_sub(elapsed);
        let sleep_step = Duration::from_millis(80);
        let mut slept = Duration::ZERO;
        while slept < wait && !stop.load(Ordering::Relaxed) {
            thread::sleep(sleep_step.min(wait - slept));
            slept += sleep_step;
        }
    }

    let _ = last_error;
}
