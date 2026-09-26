//! 配置持久化:默认与 exe 同级的 config.json(便携模式),日志在其下 logs\。
//! 保存路径可配置,优先级(高→低):
//!   1. 环境变量 SERIALTOOL_CONFIG_DIR;
//!   2. exe 同级的 config_path.txt 指针文件(内容为目录,支持相对 exe 目录);
//!   3. exe 所在目录(不可写时退回 %APPDATA%\SerialTool)。
//! 旧版 %APPDATA%\SerialTool\config.json 在新位置缺失时读取回退一次(下次保存写到新位置)。
use std::path::{Path, PathBuf};

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
    // 调用频率低(启动/保存),每次解析以支持测试与热切换
    // 1. 环境变量
    if let Some(p) = std::env::var_os("SERIALTOOL_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return p;
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    // 2. 指针文件
    if let Some(dir) = exe_dir.as_ref() {
        if let Ok(s) = std::fs::read_to_string(dir.join("config_path.txt")) {
            let custom = s.trim();
            if !custom.is_empty() {
                let p = PathBuf::from(custom);
                return if p.is_absolute() { p } else { dir.join(p) };
            }
        }
    }
    // 3. exe 同级,不可写(如 Program Files)则退回 APPDATA
    if let Some(dir) = exe_dir {
        if dir_writable(&dir) {
            return dir;
        }
    }
    appdata_dir()
}

fn appdata_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("SerialTool")
}

/// 目录可写性探测:已有配置文件视为可写,否则试建临时文件后立即删除
fn dir_writable(dir: &Path) -> bool {
    if dir.join("config.json").exists() {
        return true;
    }
    let probe = dir.join(".serial_tool_write_test");
    match std::fs::write(&probe, b"") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> AppConfig {
    let s = std::fs::read_to_string(config_path()).or_else(|_| {
        // 旧版配置在 %APPDATA%,读取回退一次实现无感迁移
        std::fs::read_to_string(appdata_dir().join("config.json"))
    });
    match s {
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

    /// 所有路径相关断言放在一个测试里串行执行,避免并行修改同一环境变量竞态
    #[test]
    fn config_paths_and_roundtrip() {
        // 把 SERIALTOOL_CONFIG_DIR 和 APPDATA 都重定向到临时目录,避免回退读到真实配置
        let base = std::env::temp_dir().join(format!("serialtool_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("SERIALTOOL_CONFIG_DIR", &base);
        let appdata_fallback = std::env::temp_dir().join(format!("serialtool_appdata_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&appdata_fallback);
        std::env::set_var("APPDATA", &appdata_fallback);

        // 1) 环境变量优先,config_dir 直接命中
        assert_eq!(config_dir(), base);

        // 2) 空目录加载回退默认值
        let def = load();
        assert_eq!(def.last_serial.baud_rate, 115200);
        assert_eq!(def.last_serial.frame_mode, FrameMode::Idle);
        assert!(def.presets.is_empty());

        // 3) 完整字段存取
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
        assert!(base.join("config.json").exists(), "config.json 应写入 SERIALTOOL_CONFIG_DIR");

        let loaded = load();
        assert_eq!(loaded.last_serial.port_name, "COM7");
        assert_eq!(loaded.last_serial.baud_rate, 921600);
        assert_eq!(loaded.last_serial.frame_mode, FrameMode::Newline);
        assert_eq!(loaded.presets.len(), 2);
        assert_eq!(loaded.presets[1].name, "心跳");
        assert_eq!(loaded.presets[1].repeat_interval_ms, Some(1000));
        assert_eq!(loaded.presets[0].decode().unwrap(), b"AT\r\n");
        assert_eq!(loaded.presets[1].decode().unwrap(), vec![0xAA, 0x55]);

        std::env::remove_var("SERIALTOOL_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&appdata_fallback);
    }
}
