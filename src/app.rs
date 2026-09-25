//! 应用顶层状态:串口收发、注入式监控、预设、日志、事件分发
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use chrono::Local;

use crate::config::{self, AppConfig};
pub use crate::logger::Dir;
use crate::logger::{ExportEntry, LogConfig, Logger};
use crate::serial::port::{list_ports, open, RxEvent, SerialConfig, SerialHandle};
use crate::serial::preset::{DataFormat, SendPreset};
use crate::injection::scan::SerialProcess;
use crate::injection::{self, frame_to_event, InjectionEvent};
use crate::injection::frame::Frame;

const MAX_LINES: usize = 50_000;

/// 发起注入后等待 agent 上线的超时;超时则恢复按钮并提示
const INJECT_ONLINE_TIMEOUT: Duration = Duration::from_secs(8);

/// 本机收发数据在日志中的进程标记
const SELF_PROC: &str = "SerialTool";

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    SendRecv,
    Monitor,
    Log,
}

#[derive(Clone)]
pub struct LogLine {
    pub ts: chrono::DateTime<Local>,
    pub dir: Dir,
    pub port: String,
    /// 产生数据的进程名(本机收发为 SerialTool,监控为目标进程)
    pub process: String,
    pub bytes: Vec<u8>,
    /// 非数据说明行(如 agent 上线、注入失败)
    pub note: String,
}

impl LogLine {
    fn data(
        ts: chrono::DateTime<Local>,
        dir: Dir,
        port: String,
        process: String,
        bytes: Vec<u8>,
    ) -> Self {
        Self { ts, dir, port, process, bytes, note: String::new() }
    }
    fn note(ts: chrono::DateTime<Local>, port: String, process: String, note: String) -> Self {
        Self { ts, dir: Dir::Rx, port, process, bytes: Vec::new(), note }
    }

    /// 是否匹配实时过滤关键词(逗号分隔多个,任一命中;大小写不敏感)
    /// 匹配范围:端口、进程名、方向(RX/TX)、HEX 数据、可见 ASCII、说明文字
    pub(crate) fn matches_filter(&self, filter: &str) -> bool {
        let kws: Vec<&str> = filter
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if kws.is_empty() {
            return true;
        }
        let dir_s = match self.dir {
            Dir::Rx => "RX",
            Dir::Tx => "TX",
        };
        let hex = crate::logger::hex_string(&self.bytes);
        let ascii = crate::logger::visible_ascii(&self.bytes);
        let hay = format!(
            "{} {} {} {} {}",
            self.port, self.process, dir_s, hex, ascii
        );
        let hay_l = hay.to_lowercase();
        let note_l = self.note.to_lowercase();
        kws.iter()
            .any(|k| hay_l.contains(&k.to_lowercase()) || note_l.contains(&k.to_lowercase()))
    }
}

/// 一个已发起注入的目标进程
pub struct MonTarget {
    pub name: String,
    /// agent 是否已装钩上线(Attach 帧到达)
    pub online: bool,
}

struct RepeatHandle {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl Drop for RepeatHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub struct SerialApp {
    pub tab: Tab,
    pub available_ports: Vec<String>,
    pub serial_cfg: SerialConfig,
    /// 波特率自由输入缓冲(与 serial_cfg.baud_rate 同步)
    pub baud_input: String,
    pub serial_handle: Option<SerialHandle>,
    pub status: String,

    pub display_format: DataFormat,
    pub lines: Vec<LogLine>,
    pub auto_scroll: bool,
    pub rx_bytes: u64,
    pub tx_bytes: u64,

    pub send_input: String,
    pub send_format: DataFormat,
    pub append_crlf: bool,

    pub presets: Vec<SendPreset>,
    pub editor_open: bool,
    pub editing: SendPreset,
    pub editing_index: Option<usize>,
    pub next_preset_id: u64,

    // ---- 注入式监控 ----
    /// 最近一次扫描命中的持串口进程
    pub scan_results: Vec<SerialProcess>,
    pub scanning: bool,
    /// pid -> 注入目标(含上线状态)
    pub targets: HashMap<u32, MonTarget>,
    /// pid -> 发起注入的时刻(用于上线超时兜底,Attach 到达后移除)
    pending_since: HashMap<u32, std::time::Instant>,
    pub monitor_lines: Vec<LogLine>,
    pub monitor_auto_scroll: bool,
    /// 收发页实时数据过滤关键词
    pub recv_filter: String,
    /// 监控页实时数据过滤关键词
    pub monitor_filter: String,

    pub log_cfg: LogConfig,
    pub logger: Logger,
    /// 日志查询关键词
    pub log_query: String,
    /// 日志查询选中的文件(None=全部)
    pub log_query_file: Option<String>,
    /// 日志查询结果:(文件名, 行内容)
    pub log_results: Vec<(String, String)>,

    cfg: AppConfig,
    serial_tx: mpsc::Sender<RxEvent>,
    serial_rx: mpsc::Receiver<RxEvent>,
    inj_rx: mpsc::Receiver<Frame>,
    scan_tx: mpsc::Sender<Vec<SerialProcess>>,
    scan_rx: mpsc::Receiver<Vec<SerialProcess>>,
    repeat_tx: mpsc::Sender<u64>,
    repeat_rx: mpsc::Receiver<u64>,
    repeat_handles: Vec<RepeatHandle>,
}

impl SerialApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        crate::ui::theme::setup_fonts(&cc.egui_ctx);
        crate::ui::theme::apply(&cc.egui_ctx);

        let cfg: AppConfig = config::load();
        let log_cfg = cfg.log.clone();
        let logger = Logger::start(log_cfg.clone());

        let (serial_tx, serial_rx) = mpsc::channel::<RxEvent>();
        let (inj_tx, inj_rx) = mpsc::channel::<Frame>();
        let (scan_tx, scan_rx) = mpsc::channel::<Vec<SerialProcess>>();
        let (repeat_tx, repeat_rx) = mpsc::channel::<u64>();

        // 管道服务端幂等启动,等待 agent 回连
        crate::injection::agent_pipe::PipeServer::start(inj_tx);

        let available = list_ports();
        let mut serial_cfg = cfg.last_serial.clone();
        if serial_cfg.port_name.is_empty() || !available.contains(&serial_cfg.port_name) {
            if let Some(p) = available.first() {
                serial_cfg.port_name = p.clone();
            }
        }
        let baud_input = serial_cfg.baud_rate.to_string();

        let presets = cfg.presets.clone();
        let next_preset_id = presets.iter().map(|p| p.id).max().unwrap_or(0) + 1;

        let mut app = Self {
            tab: Tab::SendRecv,
            available_ports: available,
            serial_cfg,
            baud_input,
            serial_handle: None,
            status: "就绪".into(),
            display_format: cfg.display_format,
            lines: Vec::new(),
            auto_scroll: true,
            rx_bytes: 0,
            tx_bytes: 0,
            send_input: String::new(),
            send_format: DataFormat::Ascii,
            append_crlf: true,
            presets,
            editor_open: false,
            editing: SendPreset::new(next_preset_id),
            editing_index: None,
            next_preset_id,
            scan_results: Vec::new(),
            scanning: false,
            targets: HashMap::new(),
            pending_since: HashMap::new(),
            monitor_lines: Vec::new(),
            monitor_auto_scroll: true,
            recv_filter: String::new(),
            monitor_filter: String::new(),
            log_cfg,
            logger,
            log_query: String::new(),
            log_query_file: None,
            log_results: Vec::new(),
            cfg,
            serial_tx,
            serial_rx,
            inj_rx,
            scan_tx,
            scan_rx,
            repeat_tx,
            repeat_rx,
            repeat_handles: Vec::new(),
        };
        app.sync_repeat_threads();
        app
    }

    /// 每帧调用:取出工作线程事件并更新状态
    pub fn poll_events(&mut self) {
        while let Ok(ev) = self.serial_rx.try_recv() {
            match ev {
                RxEvent::Data(data) => {
                    let port = self.serial_cfg.port_name.clone();
                    self.rx_bytes = self.rx_bytes.saturating_add(data.len() as u64);
                    self.logger.log(&port, SELF_PROC, Dir::Rx, &data);
                    self.push_line(LogLine::data(
                        Local::now(),
                        Dir::Rx,
                        port,
                        SELF_PROC.into(),
                        data,
                    ));
                }
                RxEvent::Error(e) => self.status = format!("串口错误: {e}"),
                RxEvent::Closed => {
                    self.serial_handle = None;
                    self.status = "串口已关闭".into();
                }
            }
        }

        // 扫描完成
        while let Ok(results) = self.scan_rx.try_recv() {
            self.scanning = false;
            self.scan_results = results;
        }

        // 注入 agent 回传事件
        while let Ok(f) = self.inj_rx.try_recv() {
            match frame_to_event(f) {
                InjectionEvent::Attach { pid } => {
                    self.pending_since.remove(&pid);
                    let name = self
                        .targets
                        .get(&pid)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| format!("pid {pid}"));
                    self.targets.insert(pid, MonTarget { name: name.clone(), online: true });
                    self.status = format!("监控中:{name} (pid {pid}),双向数据流已接管");
                    self.push_monitor_line(LogLine::note(
                        Local::now(),
                        String::new(),
                        name,
                        format!("已注入 pid {pid},hook 上线,开始双向监控"),
                    ));
                }
                InjectionEvent::Info { pid, text } => {
                    let name = self.target_name(pid);
                    self.push_monitor_line(LogLine::note(
                        Local::now(),
                        String::new(),
                        name,
                        text,
                    ));
                }
                InjectionEvent::Data { pid, port, dir, data, ts } => {
                    let name = self.target_name(pid);
                    let dir = to_log_dir(dir);
                    self.logger.log(&port, &name, dir, &data);
                    self.push_monitor_line(LogLine::data(
                        ts,
                        dir,
                        port,
                        name,
                        data,
                    ));
                }
            }
        }

        while let Ok(id) = self.repeat_rx.try_recv() {
            if let Some(p) = self.presets.iter().find(|p| p.id == id && p.enabled) {
                if let Ok(data) = p.decode() {
                    let _ = self.send_bytes(&data);
                }
            }
        }

        // 注入上线超时兜底:agent 始终不上报 ATTACH 时,恢复为可重新注入,避免永久"注入中"
        let now = std::time::Instant::now();
        let timed_out: Vec<u32> = self
            .pending_since
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= INJECT_ONLINE_TIMEOUT)
            .map(|(pid, _)| *pid)
            .collect();
        for pid in timed_out {
            self.pending_since.remove(&pid);
            if let Some(t) = self.targets.get(&pid) {
                if !t.online {
                    let name = t.name.clone();
                    self.targets.remove(&pid);
                    self.status = format!("注入 {name} (pid {pid}) 后 agent 未上线");
                    self.push_monitor_line(LogLine::note(
                        Local::now(),
                        String::new(),
                        name,
                        format!(
                            "注入后 {:.0} 秒内未收到 agent 上线信号,已取消等待,可重新点击注入;\
                             若仍失败(目标可能驻留过旧版本),请重启该程序后再试",
                            INJECT_ONLINE_TIMEOUT.as_secs_f64()
                        ),
                    ));
                }
            }
        }
    }

    fn target_name(&self, pid: u32) -> String {
        if let Some(t) = self.targets.get(&pid) {
            return t.name.clone();
        }
        if let Some(p) = self.scan_results.iter().find(|p| p.pid == pid) {
            return p.name.clone();
        }
        format!("pid {pid}")
    }

    /// 后台扫描持有串口句柄的进程
    pub fn start_scan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let tx = self.scan_tx.clone();
        thread::spawn(move || {
            let results = injection::scan::scan_serial_processes();
            let _ = tx.send(results);
        });
    }

    /// 注入指定 pid
    pub fn inject_pid(&mut self, pid: u32, name: String) {
        match injection::inject::inject(pid) {
            Ok(()) => {
                self.status = format!("已向 {name} (pid {pid}) 注入,等待 agent 上线…");
                self.targets
                    .entry(pid)
                    .or_insert_with(|| MonTarget { name: name.clone(), online: false });
                self.pending_since.insert(pid, std::time::Instant::now());
                self.push_monitor_line(LogLine::note(
                    Local::now(),
                    String::new(),
                    name,
                    format!("正在注入 pid {pid} …"),
                ));
            }
            Err(e) => {
                self.status = format!("注入失败: {e}");
                self.push_monitor_line(LogLine::note(
                    Local::now(),
                    String::new(),
                    name,
                    format!("注入失败 pid {pid}: {e}"),
                ));
            }
        }
    }

    /// 请求某 pid 的 agent 卸载 hook
    pub fn detach_pid(&mut self, pid: u32) {
        crate::injection::agent_pipe::PipeServer::detach(pid);
        self.pending_since.remove(&pid);
        if let Some(t) = self.targets.remove(&pid) {
            self.push_monitor_line(LogLine::note(
                Local::now(),
                String::new(),
                t.name,
                format!("已停止监控 pid {pid}"),
            ));
        }
    }

    /// 停止全部在线监控
    pub fn detach_all(&mut self) {
        let pids: Vec<u32> = self
            .targets
            .iter()
            .filter(|(_, t)| t.online)
            .map(|(pid, _)| *pid)
            .collect();
        for pid in pids {
            self.detach_pid(pid);
        }
    }

    pub fn refresh_ports(&mut self) {
        self.available_ports = list_ports();
        if self.serial_cfg.port_name.is_empty() {
            if let Some(p) = self.available_ports.first() {
                self.serial_cfg.port_name = p.clone();
            }
        }
    }

    /// 自由输入波特率:合法时同步到串口配置
    pub fn sync_baud_input(&mut self) {
        if let Ok(b) = self.baud_input.trim().parse::<u32>() {
            if b > 0 {
                self.serial_cfg.baud_rate = b;
            }
        }
    }

    pub fn set_baud(&mut self, b: u32) {
        self.serial_cfg.baud_rate = b;
        self.baud_input = b.to_string();
    }

    pub fn open_serial(&mut self) {
        if self.serial_handle.is_some() {
            return;
        }
        self.sync_baud_input();
        let cfg = self.serial_cfg.clone();
        match open(&cfg, self.serial_tx.clone()) {
            Ok(h) => {
                self.status = format!("已打开 {} @ {} bps", cfg.port_name, cfg.baud_rate);
                self.serial_handle = Some(h);
            }
            Err(e) => self.status = e,
        }
    }

    pub fn close_serial(&mut self) {
        if let Some(h) = self.serial_handle.take() {
            h.close();
        }
    }

    pub fn send_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        let handle = self.serial_handle.as_mut().ok_or("串口未打开")?;
        handle.send(data)?;
        let port = self.serial_cfg.port_name.clone();
        self.tx_bytes = self.tx_bytes.saturating_add(data.len() as u64);
        self.logger.log(&port, SELF_PROC, Dir::Tx, data);
        self.push_line(LogLine::data(
            Local::now(),
            Dir::Tx,
            port,
            SELF_PROC.into(),
            data.to_vec(),
        ));
        Ok(())
    }

    /// 根据启用的定时预设重建重发线程
    pub fn sync_repeat_threads(&mut self) {
        self.repeat_handles.clear();
        for p in &self.presets {
            if !p.enabled {
                continue;
            }
            let Some(ms) = p.repeat_interval_ms else { continue };
            if ms == 0 {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let stop_t = stop.clone();
            let tx = self.repeat_tx.clone();
            let id = p.id;
            let join = thread::spawn(move || loop {
                let mut left = ms as u64;
                while left > 0 {
                    if stop_t.load(Ordering::Relaxed) {
                        return;
                    }
                    let step = left.min(20);
                    thread::sleep(Duration::from_millis(step));
                    left -= step;
                }
                if tx.send(id).is_err() {
                    return;
                }
            });
            self.repeat_handles.push(RepeatHandle {
                stop,
                join: Some(join),
            });
        }
    }

    /// 日志目录下的 .log 文件名(按修改时间新→旧)
    pub fn list_log_files(&self) -> Vec<String> {
        let dir = Path::new(&self.log_cfg.dir);
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        e.path()
                            .extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("log"))
                    })
                    .filter_map(|e| {
                        let m = e.metadata().and_then(|m| m.modified()).ok()?;
                        Some((e.file_name().to_string_lossy().into_owned(), m))
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort_by(|a, b| b.1.cmp(&a.1));
        files.into_iter().map(|(n, _)| n).collect()
    }

    /// 按关键词(可含逗号分隔多个,任一命中)在日志目录检索,大小写不敏感
    pub fn search_logs(&mut self) {
        const MAX_RESULTS: usize = 2000;
        self.log_results.clear();
        let kws: Vec<String> = self
            .log_query
            .split([',', '，'])
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        let mut names = self.list_log_files();
        if let Some(sel) = &self.log_query_file {
            names.retain(|n| n == sel);
        }

        let mut truncated = false;
        'outer: for name in names {
            let path = Path::new(&self.log_cfg.dir).join(&name);
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in content.lines() {
                let hit = kws.is_empty()
                    || kws.iter().any(|k| line.to_lowercase().contains(k));
                if hit {
                    self.log_results.push((name.clone(), line.to_string()));
                    if self.log_results.len() >= MAX_RESULTS {
                        truncated = true;
                        break 'outer;
                    }
                }
            }
        }
        self.status = if truncated {
            format!("日志查询:命中达到上限 {MAX_RESULTS} 行,请缩小范围")
        } else {
            format!("日志查询:命中 {} 行", self.log_results.len())
        };
    }

    pub fn export_current_log(&mut self) {
        let fname = format!(
            "serial_export_{}.log",
            Local::now().format("%Y%m%d_%H%M%S")
        );
        let path = Path::new(&self.log_cfg.dir).join(fname);
        // 合并本机收发与注入监控两条流,按时间排序导出
        let mut entries: Vec<ExportEntry> = self
            .lines
            .iter()
            .chain(self.monitor_lines.iter())
            .filter(|l| l.note.is_empty())
            .map(|l| ExportEntry {
                ts: l.ts,
                port: l.port.clone(),
                process: l.process.clone(),
                dir: l.dir,
                data: l.bytes.clone(),
            })
            .collect();
        entries.sort_by_key(|e| e.ts);
        match Logger::export_lines(&path, &entries) {
            Ok(()) => self.status = format!("已保存: {}", path.display()),
            Err(e) => self.status = format!("保存失败: {e}"),
        }
    }

    fn push_line(&mut self, line: LogLine) {
        if self.lines.len() >= MAX_LINES {
            self.lines.drain(0..1000);
        }
        self.lines.push(line);
    }

    fn push_monitor_line(&mut self, line: LogLine) {
        if self.monitor_lines.len() >= MAX_LINES {
            self.monitor_lines.drain(0..1000);
        }
        self.monitor_lines.push(line);
    }

    fn save_config(&mut self) {
        self.sync_baud_input();
        self.cfg.last_serial = self.serial_cfg.clone();
        self.cfg.presets = self.presets.clone();
        self.cfg.display_format = self.display_format;
        self.cfg.log = self.log_cfg.clone();
        config::save(&self.cfg);
    }
}

fn to_log_dir(d: injection::Dir) -> Dir {
    match d {
        injection::Dir::Rx => Dir::Rx,
        injection::Dir::Tx => Dir::Tx,
    }
}

impl eframe::App for SerialApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        crate::ui::build_ui(self, ctx);
        // 定时重发线程触发后需要持续重绘
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl Drop for SerialApp {
    fn drop(&mut self) {
        self.close_serial();
        // 卸载所有已注入的 hook
        let pids: Vec<u32> = self.targets.keys().copied().collect();
        for pid in pids {
            crate::injection::agent_pipe::PipeServer::detach(pid);
        }
        self.repeat_handles.clear();
        self.save_config();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rx_line(port: &str, proc: &str, bytes: &[u8]) -> LogLine {
        LogLine::data(
            chrono::Local::now(),
            Dir::Rx,
            port.to_string(),
            proc.to_string(),
            bytes.to_vec(),
        )
    }

    fn tx_line(port: &str, proc: &str, bytes: &[u8]) -> LogLine {
        LogLine::data(
            chrono::Local::now(),
            Dir::Tx,
            port.to_string(),
            proc.to_string(),
            bytes.to_vec(),
        )
    }

    #[test]
    fn empty_filter_matches_all() {
        let l = rx_line("COM10", "sscom32.exe", b"Hello");
        assert!(l.matches_filter(""));
        assert!(l.matches_filter("   "));
    }

    #[test]
    fn filter_by_direction() {
        let rx = rx_line("COM10", "p.exe", b"Hi");
        let tx = tx_line("COM10", "p.exe", b"Hi");
        assert!(rx.matches_filter("RX"));
        assert!(!rx.matches_filter("TX"));
        assert!(tx.matches_filter("tx")); // 大小写不敏感
        assert!(!tx.matches_filter("rx"));
    }

    #[test]
    fn filter_by_port_and_process() {
        let l = rx_line("COM10", "sscom32.exe", b"Hi");
        assert!(l.matches_filter("COM10"));
        assert!(l.matches_filter("com10"));
        assert!(!l.matches_filter("COM11"));
        assert!(l.matches_filter("sscom"));
    }

    #[test]
    fn filter_by_ascii_and_hex() {
        let l = rx_line("COM1", "p.exe", b"Hello"); // 48 65 6C 6C 6F
        assert!(l.matches_filter("hello")); // ascii 大小写不敏感
        assert!(l.matches_filter("HELLO"));
        assert!(l.matches_filter("48")); // hex
        assert!(l.matches_filter("48 65")); // hex 前缀
        assert!(!l.matches_filter("zzzz"));
    }

    #[test]
    fn multiple_keywords_any_match() {
        let l = tx_line("COM11", "p.exe", b"abc");
        assert!(l.matches_filter("nomatch TX")); // 空格分隔, TX 命中
        assert!(l.matches_filter("com9,com11")); // 逗号分隔, 任一命中
        assert!(!l.matches_filter("com9 nomatch"));
    }

    #[test]
    fn filter_note_line() {
        let n = LogLine::note(
            chrono::Local::now(),
            "COM5".into(),
            "p.exe".into(),
            "agent 已上线".into(),
        );
        assert!(n.matches_filter("上线"));
        assert!(n.matches_filter("AGENT"));
        assert!(!n.matches_filter("断开"));
    }
}
