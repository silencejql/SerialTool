//! 串口收发:基于 serialport 打开串口,独立线程读取,主线程发送
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

/// 接收断帧间隔(ms):总线空闲超过该时长才把缓冲作为一帧,
/// 用于把一次消息被拆成的多次 read 合并回一条。
const RX_GAP_MS: u64 = 10;
/// 单帧聚合上限,超过则提前成帧(应对持续数据流)
const MAX_RX_FRAME: usize = 64 * 1024;

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
    /// 后台写线程的发送队列(UI 线程只入队,绝不阻塞)
    write_tx: Option<mpsc::Sender<Vec<u8>>>,
    write_join: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl SerialHandle {
    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        let tx = self.write_tx.as_ref().ok_or("串口未连接")?;
        tx.send(data.to_vec())
            .map_err(|_| "串口已关闭".to_string())
    }

    /// 复制底层串口句柄(dup,共享同一设备句柄),供额外的写线程使用
    pub fn try_clone_writer(&self) -> Result<Box<dyn SerialPort>, String> {
        self.writer.try_clone().map_err(|e| e.to_string())
    }

    pub fn close(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // 关闭写队列并等待后台写线程退出
        self.write_tx.take();
        if let Some(j) = self.write_join.take() {
            let _ = j.join();
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for SerialHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.write_tx.take();
        if let Some(j) = self.write_join.take() {
            let _ = j.join();
        }
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
    // reader 用短超时作为"断帧间隔":一次消息可能被驱动拆成多次 read,
    // 读到数据后继续在该窗口内合并后续字节,总线空闲超过该时长才成帧。
    reader
        .set_timeout(Duration::from_millis(RX_GAP_MS))
        .map_err(|e| format!("设置串口超时失败: {e}"))?;

    // 再 clone 一份给后台写线程:UI 线程只把数据放入队列立即返回,
    // 避免 write_all 阻塞界面(表现为点发送后卡顿、日志延迟出现)。
    let mut bg_writer = port
        .try_clone()
        .map_err(|e| format!("克隆串口句柄失败: {e}"))?;
    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
    let write_err_tx = tx.clone();
    let write_join = thread::spawn(move || {
        while let Ok(data) = write_rx.recv() {
            if let Err(e) = bg_writer.write_all(&data) {
                let _ = write_err_tx.send(RxEvent::Error(format!("发送失败: {e}")));
                break;
            }
        }
    });

    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = stop.clone();
    let join = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // 接收帧聚合缓冲:把断帧间隔内连续到达的字节合并为一帧
        let mut frame: Vec<u8> = Vec::new();
        while !stop_reader.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    frame.extend_from_slice(&buf[..n]);
                    // 缓冲达到上限先成帧,防止连续流时无限积压
                    if frame.len() >= MAX_RX_FRAME {
                        if tx.send(RxEvent::Data(std::mem::take(&mut frame))).is_err() {
                            break;
                        }
                    }
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    // 一个断帧间隔内无新字节:已攒到的数据作为一帧上报
                    if !frame.is_empty()
                        && tx.send(RxEvent::Data(std::mem::take(&mut frame))).is_err()
                    {
                        break;
                    }
                }
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
        write_tx: Some(write_tx),
        write_join: Some(write_join),
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
