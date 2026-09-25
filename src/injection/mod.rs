//! 注入式串口监控:扫描持有串口句柄的进程 → 注入 hook DLL → 管道接收双向数据流。

pub mod agent_pipe;
pub mod frame;
pub mod inject;
pub mod scan;

use chrono::Local;

use frame::{Frame, FT_ATTACH, FT_INFO, FT_RX, FT_TX};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Rx,
    Tx,
}

#[derive(Debug, Clone)]
pub enum InjectionEvent {
    /// 某 pid 的 agent 已装钩上线
    Attach { pid: u32 },
    /// 抓到一帧串口数据
    Data {
        pid: u32,
        port: String,
        dir: Dir,
        data: Vec<u8>,
        ts: chrono::DateTime<Local>,
    },
    /// agent 上报的文本信息
    Info { pid: u32, text: String },
}

/// 将底层帧翻译为 UI 事件
pub fn frame_to_event(f: Frame) -> InjectionEvent {
    match f.ftype {
        FT_ATTACH => InjectionEvent::Attach { pid: f.pid },
        FT_RX => InjectionEvent::Data {
            pid: f.pid,
            port: f.port,
            dir: Dir::Rx,
            data: f.data,
            ts: Local::now(),
        },
        FT_TX => InjectionEvent::Data {
            pid: f.pid,
            port: f.port,
            dir: Dir::Tx,
            data: f.data,
            ts: Local::now(),
        },
        FT_INFO => InjectionEvent::Info {
            pid: f.pid,
            text: String::from_utf8_lossy(&f.data).into_owned(),
        },
        _ => InjectionEvent::Info {
            pid: f.pid,
            text: format!("未知帧类型 {}", f.ftype),
        },
    }
}
