//! 命名管道服务端:接收各目标进程内 agent 回传的帧。
//!
//! `start` 启动一个常驻 acceptor 线程:循环创建管道实例并 `ConnectNamedPipe`,
//! 每个连接派一个 reader 线程。帧通过 mpsc 通道送上层。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::sync::mpsc::Sender;

use once_cell::sync::Lazy;
use windows::core::w;
use windows::Win32::Foundation::{
    BOOL, CloseHandle, GetLastError, HANDLE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, NAMED_PIPE_MODE, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, WaitForSingleObject,
};

use super::frame::{command, Frame, FrameParser, CMD_DETACH};

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// pid -> 该进程 agent 的管道句柄(用于下发 detach)
static CONNS: Lazy<Mutex<HashMap<u32, SendHandle>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static STARTED: OnceLock<()> = OnceLock::new();

pub struct PipeServer;

impl PipeServer {
    /// 启动管道监听(幂等,程序生命周期内只起一次)
    pub fn start(tx: Sender<Frame>) {
        if STARTED.set(()).is_ok() {
            std::thread::spawn(|| acceptor_loop(tx));
        }
    }

    /// 请求某 pid 的 agent 卸载 hook 并自释放;无连接时忽略
    pub fn detach(pid: u32) {
        let sh = if let Ok(mut c) = CONNS.lock() {
            c.remove(&pid)
        } else {
            return;
        };
        let Some(sh) = sh else { return };
        let cmd = command(CMD_DETACH);
        // 同一句柄上 reader 线程有挂起的 overlapped 读;写必须也走 overlapped(各自独立
        // 的 OVERLAPPED/event),否则阻塞模式下读写会互相死锁、命令永远发不出去。
        unsafe {
            let ev = match CreateEventW(None, BOOL(1), BOOL(0), windows::core::PCWSTR::null()) {
                Ok(e) => e,
                Err(_) => return,
            };
            let mut ov = OVERLAPPED::default();
            ov.hEvent = ev;
            let mut written: u32 = 0;
            let r = WriteFile(
                sh.0,
                Some(&cmd),
                Some(&mut written as *mut u32),
                Some(&mut ov as *mut OVERLAPPED),
            );
            if r.is_err() && GetLastError() == ERROR_IO_PENDING {
                // 最多等 2s,避免异常情况下拖住调用方退出
                if WaitForSingleObject(ev, 2000) == WAIT_OBJECT_0 {
                    let _ = GetOverlappedResult(sh.0, &ov, &mut written, true);
                }
            }
            let _ = CloseHandle(ev);
        }
    }
}

fn make_pipe() -> Option<HANDLE> {
    unsafe {
        let h = CreateNamedPipeW(
            w!(r"\\.\pipe\serialtool_mon"),
            // overlapped:同句柄需并发挂读与下发写(detach),阻塞模式二者会互相死锁
            FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX.0 | 0x4000_0000),
            NAMED_PIPE_MODE(PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0),
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            None,
        );
        if h == INVALID_HANDLE_VALUE { None } else { Some(h) }
    }
}

fn acceptor_loop(tx: Sender<Frame>) {
    loop {
        let h = match make_pipe() {
            Some(h) => h,
            None => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };
        // 管道以 overlapped 创建,ConnectNamedPipe 必须提供 OVERLAPPED
        let accepted = unsafe {
            let ev = match CreateEventW(
                None,
                BOOL(1),
                BOOL(0),
                windows::core::PCWSTR::null(),
            ) {
                Ok(e) => e,
                Err(_) => {
                    let _ = CloseHandle(h);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
            };
            let mut ov = OVERLAPPED::default();
            ov.hEvent = ev;
            let r = ConnectNamedPipe(h, Some(&mut ov as *mut OVERLAPPED));
            let ok = if r.is_ok() {
                true
            } else {
                match GetLastError() {
                    ERROR_PIPE_CONNECTED => true,
                    ERROR_IO_PENDING => {
                        WaitForSingleObject(ev, u32::MAX) == WAIT_OBJECT_0
                    }
                    _ => false,
                }
            };
            let _ = CloseHandle(ev);
            ok
        };
        if !accepted {
            unsafe {
                let _ = CloseHandle(h);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        let tx2 = tx.clone();
        let sh = SendHandle(h);
        std::thread::spawn(move || reader_loop(sh, tx2));
    }
}

fn reader_loop(sh: SendHandle, tx: Sender<Frame>) {
    let h = sh.0;
    let mut parser = FrameParser::new();
    let mut buf = [0u8; 16 * 1024];
    let mut registered: Option<u32> = None;
    let ev = unsafe {
        CreateEventW(None, BOOL(1), BOOL(0), windows::core::PCWSTR::null())
    };
    let Ok(ev) = ev else {
        unsafe {
            let _ = CloseHandle(h);
        }
        return;
    };
    loop {
        let mut got: u32 = 0;
        let mut ov = OVERLAPPED::default();
        ov.hEvent = ev;
        unsafe {
            let _ = ResetEvent(ev);
        }
        let r = unsafe {
            ReadFile(
                h,
                Some(&mut buf[..]),
                Some(&mut got as *mut u32),
                Some(&mut ov as *mut OVERLAPPED),
            )
        };
        if r.is_err() {
            let code = unsafe { GetLastError() };
            if code == ERROR_IO_PENDING {
                if unsafe { WaitForSingleObject(ev, u32::MAX) } != WAIT_OBJECT_0 {
                    break;
                }
                if unsafe { GetOverlappedResult(h, &ov, &mut got, true) }.is_err() {
                    break;
                }
            } else {
                break;
            }
        }
        if got == 0 {
            break;
        }
        for frame in parser.push(&buf[..got as usize]) {
            if frame.ftype == super::frame::FT_ATTACH {
                registered = Some(frame.pid);
                // 同一句柄上本线程 overlapped 读与 detach 线程 overlapped 写可并发
                if let Ok(mut c) = CONNS.lock() {
                    c.insert(frame.pid, SendHandle(h));
                }
            }
            if tx.send(frame).is_err() {
                break;
            }
        }
    }
    if let Some(pid) = registered {
        if let Ok(mut c) = CONNS.lock() {
            if c.get(&pid).map(|x| x.0 .0 as usize) == Some(h.0 as usize) {
                c.remove(&pid);
            }
        }
    }
    unsafe {
        let _ = CloseHandle(ev);
        let _ = CloseHandle(h);
    }
}
