//! 通讯日志:后台线程写文件,按端口 / 小时归档
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Rx,
    Tx,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct LogConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_dir")]
    pub dir: String,
    #[serde(default)]
    pub split_by_port: bool,
    #[serde(default = "default_true")]
    pub split_by_hour: bool,
}

fn default_enabled() -> bool {
    true
}
fn default_true() -> bool {
    true
}
fn default_dir() -> String {
    crate::config::config_dir()
        .join("logs")
        .to_string_lossy()
        .into_owned()
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: default_dir(),
            split_by_port: false,
            split_by_hour: true,
        }
    }
}

pub struct ExportEntry {
    pub ts: DateTime<Local>,
    pub port: String,
    /// 产生该数据的进程名(本机收发为本软件,注入监控为目标进程)
    pub process: String,
    pub dir: Dir,
    pub data: Vec<u8>,
}

enum LogMsg {
    Entry {
        ts: DateTime<Local>,
        port: String,
        process: String,
        dir: Dir,
        data: Vec<u8>,
    },
    UpdateCfg(LogConfig),
    Shut,
}

pub struct Logger {
    tx: mpsc::Sender<LogMsg>,
    join: Option<JoinHandle<()>>,
}

impl Logger {
    pub fn start(cfg: LogConfig) -> Self {
        let (tx, rx) = mpsc::channel::<LogMsg>();
        let join = thread::spawn(move || writer_loop(cfg, rx));
        Self { tx, join: Some(join) }
    }

    pub fn log(&self, port: &str, process: &str, dir: Dir, data: &[u8]) {
        let _ = self.tx.send(LogMsg::Entry {
            ts: Local::now(),
            port: port.to_string(),
            process: process.to_string(),
            dir,
            data: data.to_vec(),
        });
    }

    pub fn update_cfg(&self, cfg: LogConfig) {
        let _ = self.tx.send(LogMsg::UpdateCfg(cfg));
    }

    /// 立即把一批记录导出到指定文件(供"保存日志"按钮使用)
    pub fn export_lines(path: &Path, entries: &[ExportEntry]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        for e in entries {
            writeln!(
                f,
                "{} [{}] [{}] {:2} ({}): {} |{}|",
                e.ts.format("%Y-%m-%d %H:%M:%S%.3f"),
                e.port,
                e.process,
                match e.dir {
                    Dir::Rx => "RX",
                    Dir::Tx => "TX",
                },
                e.data.len(),
                hex_string(&e.data),
                visible_ascii(&e.data)
            )?;
        }
        f.flush()
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        let _ = self.tx.send(LogMsg::Shut);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn writer_loop(cfg: LogConfig, rx: mpsc::Receiver<LogMsg>) {
    let mut cfg = cfg;
    let mut current_key = String::new();
    let mut file: Option<File> = None;

    while let Ok(msg) = rx.recv() {
        match msg {
            LogMsg::Shut => break,
            LogMsg::UpdateCfg(c) => {
                cfg = c;
                file = None;
                current_key.clear();
            }
            LogMsg::Entry { ts, port, process, dir, data } => {
                if !cfg.enabled {
                    continue;
                }
                let mut key = String::from("serial");
                if cfg.split_by_port {
                    key.push('_');
                    key.push_str(&sanitize(&port));
                }
                key.push('_');
                key.push_str(
                    &ts.format(if cfg.split_by_hour {
                        "%Y%m%d_%H"
                    } else {
                        "%Y%m%d"
                    })
                    .to_string(),
                );

                if key != current_key {
                    if std::fs::create_dir_all(&cfg.dir).is_ok() {
                        let path = Path::new(&cfg.dir).join(format!("{key}.log"));
                        file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                            .ok();
                        current_key = key;
                    } else {
                        file = None;
                    }
                }

                if let Some(f) = file.as_mut() {
                    let _ = writeln!(
                        f,
                        "{} [{}] [{}] {:2} ({}): {} |{}|",
                        ts.format("%Y-%m-%d %H:%M:%S%.3f"),
                        port,
                        process,
                        match dir {
                            Dir::Rx => "RX",
                            Dir::Tx => "TX",
                        },
                        data.len(),
                        hex_string(&data),
                        visible_ascii(&data)
                    );
                    let _ = f.flush();
                }
            }
        }
    }

    if let Some(mut f) = file {
        let _ = f.flush();
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn hex_string(data: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(data.len() * 3);
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{b:02X}");
    }
    s
}

pub fn visible_ascii(data: &[u8]) -> String {
    data.iter()
        .map(|&b| {
            if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_writes_entries() {
        let dir = std::env::temp_dir().join(format!("serialtool_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = LogConfig {
            enabled: true,
            dir: dir.to_string_lossy().into_owned(),
            split_by_port: true,
            split_by_hour: false,
        };
        let logger = Logger::start(cfg);
        logger.log("COM9", "serial_tool.exe", Dir::Rx, b"hello");
        logger.log("COM9", "serial_tool.exe", Dir::Tx, &[0xAA, 0x55]);
        drop(logger);

        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "按端口+天应只产生一个文件");
        let content = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(content.contains("[COM9] [serial_tool.exe] RX (5): 68 65 6C 6C 6F"));
        assert!(content.contains("[COM9] [serial_tool.exe] TX (2): AA 55"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
