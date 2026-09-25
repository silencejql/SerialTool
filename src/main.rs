//! SerialTool - 体积小、功能强的串口收发与注入式监控上位机
//!
//! CLI 诊断命令(用 debug 构建运行,release 为窗口子系统无控制台):
//!   serial_tool scan                         扫描持有串口句柄的进程
//!   serial_tool inject <pid> [毫秒]          注入并打印双向数据流
//!   serial_tool loop   <COMn> [baud] [间隔ms] [持续ms]  周期收发(充当被注入目标)
//!   serial_tool send   <COMn> [baud] <text...>
//!   serial_tool recv   <COMn> [baud] [毫秒]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod injection;
mod logger;
mod serial;
mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("scan") => {
            run_cli_scan();
            return Ok(());
        }
        Some("inject") if args.len() >= 3 => {
            if let Ok(pid) = args[2].parse() {
                run_cli_inject(pid, args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10_000));
            } else {
                eprintln!("用法: serial_tool inject <pid> [毫秒]");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some("loop") if args.len() >= 3 => {
            run_cli_loop(
                &args[2],
                args.get(3).and_then(|s| s.parse().ok()).unwrap_or(115200),
                args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1000),
                args.get(5).and_then(|s| s.parse().ok()).unwrap_or(15_000),
            );
            return Ok(());
        }
        Some("send") if args.len() >= 4 => {
            run_cli_send(
                &args[2],
                args.get(3).and_then(|s| s.parse().ok()).unwrap_or(115200),
                &args[4..],
            );
            return Ok(());
        }
        Some("recv") if args.len() >= 3 => {
            run_cli_recv(
                &args[2],
                args.get(3).and_then(|s| s.parse().ok()).unwrap_or(115200),
                args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5_000),
            );
            return Ok(());
        }
        _ => {}
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([820.0, 520.0])
            .with_title(format!("SerialTool v{}", env!("APP_VERSION")))
            .with_icon(egui::IconData {
                rgba: include_bytes!("../assets/icon_64.rgba").to_vec(),
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "SerialTool",
        options,
        Box::new(|cc| Ok(Box::new(app::SerialApp::new(cc)))),
    )
}

fn hex_ascii(data: &[u8]) -> (String, String) {
    let hex: Vec<String> = data.iter().map(|b| format!("{b:02X}")).collect();
    let ascii: String = data
        .iter()
        .map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '.' })
        .collect();
    (hex.join(" "), ascii)
}

/// `scan`:列出持有串口句柄的进程
fn run_cli_scan() {
    println!("[cli] 扫描持有串口句柄的进程...");
    let procs = injection::scan::scan_serial_processes();
    if procs.is_empty() {
        println!("[cli] 未发现打开串口的进程");
        return;
    }
    println!("{:<8} {:<28} {:<12} {}", "PID", "进程", "位数", "串口");
    for p in &procs {
        println!(
            "{:<8} {:<28} {:<12} {}",
            p.pid,
            p.name,
            if p.x64 { "x64" } else { "x86" },
            p.ports.join(", ")
        );
    }
}

/// `inject <pid> [ms]`:注入 agent 并打印双向数据流
fn run_cli_inject(pid: u32, duration_ms: u64) {
    let (tx, rx) = mpsc::channel::<injection::frame::Frame>();
    injection::agent_pipe::PipeServer::start(tx);
    // 给 acceptor 一点时间挂出管道实例
    std::thread::sleep(Duration::from_millis(100));

    println!("[cli] 注入 pid {pid} ...");
    if let Err(e) = injection::inject::inject(pid) {
        eprintln!("[cli] 注入失败: {e}");
        std::process::exit(2);
    }
    println!("[cli] 已注入,监听 {duration_ms} ms(等待 agent 上线)...");

    let start = Instant::now();
    while start.elapsed().as_millis() < duration_ms as u128 {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(f) => {
                let ev = injection::frame_to_event(f);
                use injection::InjectionEvent::*;
                match ev {
                    Attach { pid } => println!("[cli] agent 上线 pid={pid}"),
                    Info { pid, text } => println!("[cli] info pid={pid}: {text}"),
                    Data {
                        pid,
                        port,
                        dir,
                        data,
                        ts,
                    } => {
                        let (hex, ascii) = hex_ascii(&data);
                        let d = match dir {
                            injection::Dir::Rx => "RX",
                            injection::Dir::Tx => "TX",
                        };
                        println!(
                            "{} pid={pid} [{port}] {d} {:>4}B: {} |{ascii}|",
                            ts.format("%H:%M:%S%.3f"),
                            data.len(),
                            hex
                        );
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    injection::agent_pipe::PipeServer::detach(pid);
    println!("[cli] done");
}

/// `loop <COMn> [baud] [interval_ms] [duration_ms]`:周期收发,作为注入测试目标
fn run_cli_loop(port: &str, baud: u32, interval_ms: u64, duration_ms: u64) {
    use serial::port::{open, RxEvent, SerialConfig};

    let cfg = SerialConfig {
        port_name: port.into(),
        baud_rate: baud,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel::<RxEvent>();
    let handle = match open(&cfg, tx) {
        Ok(h) => {
            println!("[cli] loop {port} @ {baud}, interval {interval_ms}ms, {duration_ms}ms");
            h
        }
        Err(e) => {
            eprintln!("[cli] OPEN FAILED: {e}");
            std::process::exit(2);
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_w = stop.clone();
    let mut writer = match handle.try_clone_writer() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[cli] clone writer failed: {e}");
            std::process::exit(2);
        }
    };
    use std::io::Write as _;
    std::thread::spawn(move || {
        let mut n = 0u32;
        while !stop_w.load(Ordering::Relaxed) {
            n += 1;
            let line = format!("PING {n}\r\n");
            if writer.write_all(line.as_bytes()).is_err() {
                return;
            }
            // 分片睡眠以便及时停止
            let mut slept = 0u64;
            while slept < interval_ms && !stop_w.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
                slept += 20;
            }
        }
    });

    let start = Instant::now();
    while start.elapsed().as_millis() < duration_ms as u128 {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(RxEvent::Data(d)) => {
                let (hex, ascii) = hex_ascii(&d);
                println!("[{port}] RX {:>4}B: {hex} |{ascii}|", d.len());
            }
            Ok(RxEvent::Error(e)) => {
                eprintln!("[{port}] READ ERROR: {e}");
                break;
            }
            Ok(RxEvent::Closed) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop.store(true, Ordering::Relaxed);
    handle.close();
    println!("[cli] done");
}

/// `send <COMn> [baud] <text...>`
fn run_cli_send(port: &str, baud: u32, words: &[String]) {
    use serial::port::{open, RxEvent, SerialConfig};

    let cfg = SerialConfig {
        port_name: port.into(),
        baud_rate: baud,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel::<RxEvent>();
    let mut handle = match open(&cfg, tx) {
        Ok(h) => {
            println!("[cli] opened {port} @ {baud}");
            h
        }
        Err(e) => {
            eprintln!("[cli] OPEN FAILED: {e}");
            std::process::exit(2);
        }
    };
    let mut payload = words.join(" ").into_bytes();
    payload.extend_from_slice(b"\r\n");
    match handle.send(&payload) {
        Ok(()) => println!("[cli] sent {} bytes", payload.len()),
        Err(e) => {
            eprintln!("[cli] SEND FAILED: {e}");
            std::process::exit(3);
        }
    }
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        if let Ok(RxEvent::Data(d)) = rx.recv_timeout(Duration::from_millis(100)) {
            println!("[{port}] echo {} bytes", d.len());
        }
    }
    handle.close();
    println!("[cli] done");
}

/// `recv <COMn> [baud] [毫秒]`
fn run_cli_recv(port: &str, baud: u32, duration_ms: u64) {
    use serial::port::{open, RxEvent, SerialConfig};

    let cfg = SerialConfig {
        port_name: port.into(),
        baud_rate: baud,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel::<RxEvent>();
    let handle = match open(&cfg, tx) {
        Ok(h) => {
            println!("[cli] opened {port} @ {baud}, receiving {duration_ms} ms");
            h
        }
        Err(e) => {
            eprintln!("[cli] OPEN FAILED: {e}");
            std::process::exit(2);
        }
    };

    let start = Instant::now();
    while start.elapsed().as_millis() < duration_ms as u128 {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(RxEvent::Data(d)) => {
                let (hex, ascii) = hex_ascii(&d);
                println!("[{port}] RX {:>4}B: {hex} |{ascii}|", d.len());
            }
            Ok(RxEvent::Error(e)) => {
                eprintln!("[{port}] READ ERROR: {e}");
                handle.close();
                return;
            }
            Ok(RxEvent::Closed) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
    handle.close();
    println!("[cli] done");
}
