//! 配置持久化:%APPDATA%\SerialTool\config.json
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::logger::LogConfig;
use crate::serial::port::SerialConfig;
use crate::serial::preset::DataFormat;

#[derive(Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub last_serial: SerialConfig,
    #[serde(default)]
    pub presets: Vec<crate::serial::preset::SendPreset>,
    #[serde(default)]
    pub display_format: DataFormat,
    #[serde(default)]
    pub log: LogConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            last_serial: SerialConfig::default(),
            presets: Vec::new(),
            display_format: DataFormat::Ascii,
            log: LogConfig::default(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("SerialTool")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> AppConfig {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}

pub fn save(cfg: &AppConfig) {
    let _ = std::fs::create_dir_all(config_dir());
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(config_path(), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serial::port::FrameMode;
    use crate::serial::preset::{DataFormat, SendPreset};

    #[test]
    fn config_roundtrip() {
        // APPDATA 在测试进程内重定向到临时目录,不污染真实环境
        let base = std::env::temp_dir().join(format!("serialtool_cfg_{}", std::process::id()));
        std::env::set_var("APPDATA", &base);
        let _ = std::fs::remove_dir_all(&base);

        let cfg = AppConfig {
            last_serial: SerialConfig {
                port_name: "COM7".into(),
                baud_rate: 921600,
                data_bits: 7,
                stop_bits: 2.0,
                parity: "Even".into(),
                flow_control: "Hardware".into(),
                frame_mode: FrameMode::Newline,
            },
            presets: vec![
                SendPreset {
                    id: 1,
                    name: "AT".into(),
                    content: "AT".into(),
                    format: DataFormat::Ascii,
                    append_crlf: true,
                    repeat_interval_ms: None,
                    enabled: false,
                    expanded: false,
                },
                SendPreset {
                    id: 2,
                    name: "心跳".into(),
                    content: "AA 55".into(),
                    format: DataFormat::Hex,
                    append_crlf: false,
                    repeat_interval_ms: Some(1000),
                    enabled: true,
                    expanded: true,
                },
            ],
            display_format: DataFormat::Hex,
            log: LogConfig::default(),
        };
        save(&cfg);

        let loaded = load();
        assert_eq!(loaded.last_serial.port_name, "COM7");
        assert_eq!(loaded.last_serial.baud_rate, 921600);
        assert_eq!(loaded.last_serial.frame_mode, FrameMode::Newline);
        assert_eq!(loaded.presets.len(), 2);
        assert_eq!(loaded.presets[1].name, "心跳");
        assert_eq!(loaded.presets[1].repeat_interval_ms, Some(1000));
        assert_eq!(loaded.presets[0].decode().unwrap(), b"AT\r\n");
        assert_eq!(loaded.presets[1].decode().unwrap(), vec![0xAA, 0x55]);

        let _ = std::fs::remove_dir_all(&base);

        // 无配置文件时回退默认值
        let base2 = std::env::temp_dir().join(format!("serialtool_cfg_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base2);
        std::env::set_var("APPDATA", &base2);
        let def = load();
        assert_eq!(def.last_serial.baud_rate, 115200);
        assert_eq!(def.last_serial.frame_mode, FrameMode::Idle);
        assert!(def.presets.is_empty());
        let _ = std::fs::remove_dir_all(&base2);
    }
}
