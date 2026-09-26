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
/// 换行符模式下,半行(尚未遇到 \n)等待该空闲时长后也先推出,
/// 保证无换行结尾的数据不会一直滞留缓冲。
const NEWLINE_PARTIAL_MS: u64 = 100;
/// 单帧聚合上限,超过则提前成帧(应对持续数据流)
const MAX_RX_FRAME: usize = 64 * 1024;

/// 接收成帧方式
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FrameMode {
    /// 按总线空闲间隔成帧:驱动把一条消息拆成多次 read 时合并为一帧
    Idle,
    /// 按换行符 `\n` 成帧(兼容 `\r\n`,分隔符保留在帧尾)
    Newline,
}

impl Default for FrameMode {
    fn default() -> Self {
        FrameMode::Idle
    }
}

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
    /// 接收成帧方式(旧配置缺省为空闲间隔)
    #[serde(default)]
    pub frame_mode: FrameMode,
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
            frame_mode: FrameMode::Idle,
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
    // reader 用短超时:既是空闲成帧的断帧判据,也让轮询能及时响应停止。
    // (Windows 上 DuplicateHandle 共享同一文件对象的 COMMTIMEOUTS,该超时会
    //  同时作用于写句柄;写线程用 write_all 会自动补写剩余字节,数据不丢。)
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
                // 错误文案在这里带齐上下文,UI/CLI 直接显示,不再二次加前缀
                let _ = write_err_tx.send(RxEvent::Error(format!("发送失败: {e}")));
                break;
            }
        }
    });

    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = stop.clone();
    let mode = cfg.frame_mode;
    let join = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // 接收聚合缓冲:Idle 模式按空闲间隔成帧,Newline 模式按 \n 切行
        let mut acc: Vec<u8> = Vec::new();
        // 最近一次收到字节的时刻(Newline 模式半行兜底用)
        let mut last_byte = std::time::Instant::now();
        while !stop_reader.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    acc.extend_from_slice(&buf[..n]);
                    last_byte = std::time::Instant::now();
                    if mode == FrameMode::Newline {
                        // 一次 read 可能含多行:切出所有以 \n 结尾的完整帧
                        for frame in drain_newline_frames(&mut acc) {
                            if tx.send(RxEvent::Data(frame)).is_err() {
                                return;
                            }
                        }
                    }
                    // 缓冲达到上限先成帧,防止持续数据流时无限积压
                    if acc.len() >= MAX_RX_FRAME
                        && tx.send(RxEvent::Data(std::mem::take(&mut acc))).is_err()
                    {
                        return;
                    }
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    if acc.is_empty() {
                        continue;
                    }
                    // Idle:一个断帧间隔无新字节即成帧;
                    // Newline:仅在较长空闲后把无换行结尾的残余先推出,避免滞留
                    let flush = match mode {
                        FrameMode::Idle => true,
                        FrameMode::Newline => {
                            last_byte.elapsed() >= Duration::from_millis(NEWLINE_PARTIAL_MS)
                        }
                    };
                    if flush && tx.send(RxEvent::Data(std::mem::take(&mut acc))).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(RxEvent::Error(format!("接收失败: {e}")));
                    break;
                }
            }
        }
        // 退出前冲刷残余,避免关闭串口时最后一帧丢失
        if !acc.is_empty() {
            let _ = tx.send(RxEvent::Data(acc));
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

/// 从接收聚合缓冲中切出所有以 `\n` 结尾的完整帧(兼容 `\r\n`,分隔符保留在帧尾)。
/// 切出的帧按字节先后顺序返回;尚未遇到换行的半行仍留在 `acc` 中等待后续字节。
pub(crate) fn drain_newline_frames(acc: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    while let Some(rel) = acc.iter().position(|&b| b == b'\n') {
        // split_off 后 acc=本行(含 \n),返回值为剩余尾部
        let rest = acc.split_off(rel + 1);
        let frame = std::mem::replace(acc, rest);
        frames.push(frame);
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_multiple_frames_one_buffer() {
        // 一次 read 粘连多行:\r\n 与裸 \n 混用,分隔符保留帧尾
        let mut acc: Vec<u8> = b"L1\r\nL2\nL3".to_vec();
        let frames = drain_newline_frames(&mut acc);
        assert_eq!(frames, vec![b"L1\r\n".to_vec(), b"L2\n".to_vec()]);
        assert_eq!(acc, b"L3");
    }

    #[test]
    fn newline_fragmented_reads_merge() {
        // 一条消息被拆成多次 read:换行到达前应持续累积为同一帧
        let mut acc: Vec<u8> = b"AT+".to_vec();
        assert!(drain_newline_frames(&mut acc).is_empty());
        acc.extend_from_slice(b"CSQ\r\nOK\r\n");
        let frames = drain_newline_frames(&mut acc);
        assert_eq!(frames, vec![b"AT+CSQ\r\n".to_vec(), b"OK\r\n".to_vec()]);
        assert!(acc.is_empty());
    }

    #[test]
    fn newline_only_cr_is_partial() {
        // 只有 \r 不算换行,必须等到 \n
        let mut acc: Vec<u8> = b"abc\rdef".to_vec();
        assert!(drain_newline_frames(&mut acc).is_empty());
        assert_eq!(acc, b"abc\rdef");
        acc.push(b'\n');
        let frames = drain_newline_frames(&mut acc);
        assert_eq!(frames, vec![b"abc\rdef\n".to_vec()]);
        assert!(acc.is_empty());
    }

    #[test]
    fn newline_empty_and_trailing() {
        let mut acc: Vec<u8> = Vec::new();
        assert!(drain_newline_frames(&mut acc).is_empty());

        // 帧以换行结尾时全部切出,无残余
        let mut acc: Vec<u8> = b"\r\n\n".to_vec();
        let frames = drain_newline_frames(&mut acc);
        assert_eq!(frames, vec![b"\r\n".to_vec(), b"\n".to_vec()]);
        assert!(acc.is_empty());
    }

    #[test]
    fn newline_split_off_does_not_clone_bytes() {
        // 边界:换行恰好是最后一个字节(split_off(len) 返回空尾部)
        let mut acc: Vec<u8> = b"END\n".to_vec();
        let frames = drain_newline_frames(&mut acc);
        assert_eq!(frames, vec![b"END\n".to_vec()]);
        assert!(acc.is_empty());
    }
}
