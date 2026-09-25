//! 串口收发:基于 serialport 打开串口,独立线程读取,主线程发送
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

#[derive(Clone, Serialize, Deserialize)]
pub struct SerialConfig {
    pub port_name: String,
    #[serde(default = "default_baud")]
    pub baud_rate: u32,
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default = "default_stop_bits")]
    pub stop_bits: f32,
    #[serde(default)]
    pub parity: String,
    #[serde(default)]
    pub flow_control: String,
}

fn default_baud() -> u32 {
    115200

}
fn default_data_bits() -> u8 {
    8
}
fn default_stop_bits() -> f32 {
    1.0
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port_name: String::new(),
            baud_rate: 115200,
            data_bits: 8,
            stop_bits: 1.0,
            parity: "None".into(),
            flow_control: "None".into(),
        }
    }
}

pub enum RxEvent {
    Data(Vec<u8>),
    Error(String),
    Closed,
}

pub struct SerialHandle {
    writer: Box<dyn SerialPort>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl SerialHandle {
    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer.write_all(data).map_err(|e| e.to_string())
    }

    /// 复制底层串口句柄(dup,共享同一设备句柄),供额外的写线程使用
    pub fn try_clone_writer(&self) -> Result<Box<dyn SerialPort>, String> {
        self.writer.try_clone().map_err(|e| e.to_string())
    }

    pub fn close(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for SerialHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 枚举系统可用串口
pub fn list_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

/// 以指定配置打开串口,启动 reader 线程
pub fn open(
    cfg: &SerialConfig,
    tx: mpsc::Sender<RxEvent>,
) -> Result<SerialHandle, String> {
    if cfg.port_name.is_empty() {
        return Err("未选择串口".into());
    }

    let port = serialport::new(&cfg.port_name, cfg.baud_rate)
        .data_bits(data_bits_of(cfg.data_bits))
        .stop_bits(stop_bits_of(cfg.stop_bits))
        .parity(parity_of(&cfg.parity))
        .flow_control(flow_of(&cfg.flow_control))
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| format!("打开 {} 失败: {e}", cfg.port_name))?;

    // 监控端绝不做的事情这里是主控端:try_clone 一份给 reader 线程
    let mut reader = port
        .try_clone()
        .map_err(|e| format!("克隆串口句柄失败: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = stop.clone();
    let join = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while !stop_reader.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    if tx.send(RxEvent::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    let _ = tx.send(RxEvent::Error(e.to_string()));
                    break;
                }
            }
        }
        let _ = tx.send(RxEvent::Closed);
    });

    Ok(SerialHandle {
        writer: port,
        stop,
        join: Some(join),
    })
}

fn data_bits_of(v: u8) -> DataBits {
    match v {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    }
}

fn stop_bits_of(v: f32) -> StopBits {
    if (v - 2.0).abs() < 0.1 {
        StopBits::Two
    } else {
        StopBits::One
    }
}

fn parity_of(s: &str) -> Parity {
    match s {
        "Odd" => Parity::Odd,
        "Even" => Parity::Even,
        _ => Parity::None,
    }
}

fn flow_of(s: &str) -> FlowControl {
    match s {
        "Software" => FlowControl::Software,
        "Hardware" => FlowControl::Hardware,
        _ => FlowControl::None,
    }
}
