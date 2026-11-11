use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::util;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub theme: crate::theme::ThemeMode,
    pub adb_path: String,
    pub bundletool_path: String,
    pub java_path: String,
    pub download_proxy: String,
    pub bundletool_version: String,
    pub java_version: String,
    pub java_major: u8,
    pub screenshot_fps: u8,
    pub scrcpy_enabled: bool,
    pub mirror_embedded: bool,
    pub mirror_fps: u8,
    pub mirror_max_size: u16,
    pub mirror_bit_rate_mbps: u8,
    pub max_log_lines: usize,
    pub log_auto_scroll: bool,
    pub auto_install_drops: bool,
    pub gamepad: GamepadConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: crate::theme::ThemeMode::System,
            adb_path: String::new(),
            bundletool_path: String::new(),
            java_path: String::new(),
            download_proxy: String::new(),
            bundletool_version: String::new(),
            java_version: String::new(),
            java_major: 17,
            screenshot_fps: 3,
            scrcpy_enabled: true,
            mirror_embedded: cfg!(windows),
            mirror_fps: 60,
            mirror_max_size: 1920,
            mirror_bit_rate_mbps: 8,
            max_log_lines: 20_000,
            log_auto_scroll: true,
            auto_install_drops: true,
            gamepad: GamepadConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load() -> Self {
        Self::load_from_dir(&util::install_dir())
    }

    fn load_from_dir(directory: &std::path::Path) -> Self {
        // Keep existing portable settings when upgrading from the previous name.
        let current = directory.join("androidtool.settings.json");
        let path = if current.exists() {
            current
        } else {
            directory.join("droiddeck.settings.json")
        };
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut config: Self = serde_json::from_str(&text).unwrap_or_default();
        config.screenshot_fps = config.screenshot_fps.clamp(1, 60);
        config.mirror_fps = config.mirror_fps.clamp(1, 60);
        config.mirror_max_size = config.mirror_max_size.clamp(640, 2560);
        config.mirror_bit_rate_mbps = config.mirror_bit_rate_mbps.clamp(1, 32);
        config
    }

    pub fn save(&self) -> Result<(), String> {
        let path = util::settings_path();
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| format!("无法序列化配置: {error}"))?;
        fs::write(&path, text).map_err(|error| format!("无法保存配置 {}: {error}", path.display()))
    }

    pub fn resolved_adb(&self) -> Option<PathBuf> {
        util::detect_adb(&self.adb_path)
    }

    pub fn resolved_bundletool(&self) -> Option<PathBuf> {
        util::detect_bundletool(&self.bundletool_path)
    }

    pub fn resolved_java(&self) -> Option<PathBuf> {
        util::detect_java(&self.java_path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GamepadConfig {
    pub deadzone: f32,
}
impl Default for GamepadConfig {
    fn default() -> Self {
        Self { deadzone: 0.16 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_preferences_round_trip_and_old_configs_follow_system() {
        let old: AppConfig = serde_json::from_str(r#"{"mirror_fps":30}"#).unwrap();
        assert_eq!(old.theme, crate::theme::ThemeMode::System);
        for theme in crate::theme::ThemeMode::ALL {
            let config = AppConfig {
                theme,
                ..Default::default()
            };
            let json = serde_json::to_string(&config).unwrap();
            let restored: AppConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.theme, theme);
        }
    }

    #[test]
    fn renamed_settings_take_precedence_with_legacy_fallback() {
        let directory =
            std::env::temp_dir().join(format!("androidtool-config-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let legacy = directory.join("droiddeck.settings.json");
        let current = directory.join("androidtool.settings.json");
        fs::write(&legacy, r#"{"mirror_fps":30}"#).unwrap();
        assert_eq!(AppConfig::load_from_dir(&directory).mirror_fps, 30);
        fs::write(&current, r#"{"mirror_fps":60}"#).unwrap();
        assert_eq!(AppConfig::load_from_dir(&directory).mirror_fps, 60);
        fs::remove_file(current).unwrap();
        fs::remove_file(legacy).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn legacy_settings_enable_video_stream_with_independent_fps() {
        let config: AppConfig = serde_json::from_str(r#"{"screenshot_fps":3}"#).unwrap();
        assert!(config.scrcpy_enabled);
        assert_eq!(config.mirror_embedded, cfg!(windows));
        assert_eq!(config.mirror_fps, 60);
        assert_eq!(config.mirror_max_size, 1920);
        assert_eq!(config.screenshot_fps, 3);
        assert_eq!(config.java_major, 17);
        assert!(config.download_proxy.is_empty());
    }

    #[test]
    fn download_preferences_round_trip() {
        let mut config = AppConfig::default();
        config.java_major = 21;
        config.java_version = "jdk-21.0.9+10".into();
        config.bundletool_version = "1.18.2".into();
        config.download_proxy = "http://127.0.0.1:7890".into();
        let restored: AppConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(restored.java_major, 21);
        assert_eq!(restored.java_version, config.java_version);
        assert_eq!(restored.bundletool_version, config.bundletool_version);
        assert_eq!(restored.download_proxy, config.download_proxy);
    }
}
