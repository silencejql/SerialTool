//! 自动扫描持有串口句柄的进程。
//!
//! 纯用户态:
//! 1. `QueryDosDeviceW` 建立 `COMn -> \Device\...` 映射
//! 2. `NtQuerySystemInformation(SystemExtendedHandleInformation)` 枚举系统句柄表
//! 3. `DuplicateHandle` 复制到自身,`NtQueryObject` 取名,匹配串口设备路径
//! 4. `QueryFullProcessImageNameW` 取进程名,`IsWow64Process2` 判位数

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    BOOL, CloseHandle, DuplicateHandle, HANDLE, LPARAM, DUPLICATE_SAME_ACCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, QueryDosDeviceW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_UNKNOWN;
use windows::Win32::System::Threading::{
    GetCurrentProcess, IsWow64Process2, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_DUP_HANDLE, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Debug, Clone)]
pub struct SerialProcess {
    pub pid: u32,
    pub name: String,
    pub ports: Vec<String>,
    /// true = 原生 64 位 x64 进程(可注入);false = 32 位/ARM64 等
    pub x64: bool,
}

// ---- NT 原生结构/调用 ----

const SYSTEM_EXTENDED_HANDLE_INFORMATION: i32 = 64;
const OBJECT_NAME_INFORMATION: u32 = 1;
const STATUS_INFO_LENGTH_MISMATCH: i32 = -1073741820; // 0xC0000004u32 as i32

#[derive(Clone, Copy)]
struct SharedH(HANDLE);
unsafe impl Send for SharedH {}
unsafe impl Sync for SharedH {}

#[repr(C)]
struct SystemHandleInfoEx {
    number_of_handles: usize,
    reserved: usize,
}

#[repr(C)]
struct SystemHandleEntryEx {
    object: *mut c_void,
    unique_process_id: usize,
    handle_value: usize,
    granted_access: u32,
    creator_back_trace_index: u16,
    object_type_index: u16,
    handle_attributes: u32,
    reserved: u32,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectNameInfo {
    name: UnicodeString,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        class: i32,
        info: *mut c_void,
        len: u32,
        retlen: *mut u32,
    ) -> i32;
    fn NtQueryObject(
        h: HANDLE,
        info_class: u32,
        info: *mut c_void,
        len: u32,
        retlen: *mut u32,
    ) -> i32;
}

/// 打开当前 exe(普通磁盘文件)得到一个 File 内核对象句柄,用作类型探针
fn open_file_probe() -> Option<HANDLE> {
    let path = std::env::current_exe().ok()?;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0x80, // FILE_READ_ATTRIBUTES
            FILE_SHARE_MODE(7), // READ|WRITE|DELETE
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            HANDLE(std::ptr::null_mut()),
        )
        .ok()
    }
}

/// 返回 `COMn -> 大写设备路径`,如 COM10 -> \DEVICE\COM0COM-A
fn com_device_map() -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();

    // 1. 取全部 DOS 设备名(缓冲按需倍增,ERROR_MORE_DATA 时返回 0)
    let mut cap: usize = 16384;
    let names_buf = loop {
        let mut b = vec![0u16; cap];
        let n = unsafe { QueryDosDeviceW(None, Some(&mut b)) } as usize;
        if n > 0 {
            b.truncate(n);
            break b;
        }
        cap *= 4;
        if cap > 1 << 20 {
            return result;
        }
    };

    // 双 \0 结尾的名字列表
    let flat: String = String::from_utf16_lossy(&names_buf);
    for raw in flat.split('\0') {
        if raw.is_empty() {
            continue;
        }
        let up = raw.to_ascii_uppercase();
        match up.strip_prefix("COM") {
            Some(d) if !d.is_empty() && d.len() <= 3 && d.bytes().all(|b| b.is_ascii_digit()) => {}
            _ => continue,
        }

        // 2. 查该 DOS 名指向的设备路径
        let mut target = vec![0u16; 1024];
        let wide: Vec<u16> = raw.encode_utf16().chain(std::iter::once(0)).collect();
        let tn = unsafe {
            QueryDosDeviceW(windows::core::PCWSTR(wide.as_ptr()), Some(&mut target))
        } as usize;
        if tn > 0 {
            let t = String::from_utf16_lossy(&target[..tn])
                .trim_end_matches('\0')
                .to_ascii_uppercase();
            // 个别设备返回多路径(分号分隔),取第一段
            let first = t.split(';').next().unwrap_or(&t).trim_end_matches('\0');
            if !first.is_empty() {
                result.insert(raw.to_string(), first.to_string());
            }
        }
    }
    result
}

fn query_object_name(h: HANDLE, buf: &mut [u8]) -> Option<String> {
    let status = unsafe {
        NtQueryObject(
            h,
            OBJECT_NAME_INFORMATION,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
            std::ptr::null_mut(),
        )
    };
    if status < 0 {
        return None;
    }
    let info = unsafe { &*(buf.as_ptr() as *const ObjectNameInfo) };
    if info.name.buffer.is_null() || info.name.length == 0 {
        return None;
    }
    let chars = (info.name.length / 2) as usize;
    unsafe {
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(
            info.name.buffer,
            chars,
        )))
    }
}

fn process_file_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(h);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(
            Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(path),
        )
    }
}

/// 判定目标进程是否为原生 64 位 x64(false = WOW64 32 位 / ARM64 等)。
/// 注入器据此选择同位数的 agent DLL 与跨位数注入路径。
pub(crate) fn is_x64_process(pid: u32) -> bool {
    unsafe {
        let h = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return false,
        };
        let mut machine = IMAGE_FILE_MACHINE_UNKNOWN;
        let mut native = IMAGE_FILE_MACHINE_UNKNOWN;
        let ok = IsWow64Process2(h, &mut machine, Some(&mut native));
        let _ = CloseHandle(h);
        match ok {
            Ok(()) => machine == IMAGE_FILE_MACHINE_UNKNOWN,
            Err(_) => false,
        }
    }
}

/// 枚举拥有可见顶层窗口的进程 pid(串口上位机几乎都是 GUI 交互程序)
fn visible_window_pids() -> HashSet<u32> {
    let mut set: HashSet<u32> = HashSet::new();
    let ptr = &mut set as *mut HashSet<u32> as isize;
    let _ = unsafe { EnumWindows(Some(enum_windows_proc), LPARAM(ptr)) };
    set
}

unsafe extern "system" fn enum_windows_proc(hwnd: windows::Win32::Foundation::HWND, lparam: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd).as_bool() {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            (*(lparam.0 as *mut HashSet<u32>)).insert(pid);
        }
    }
    BOOL(1)
}

/// 常见串口/调试工具进程名关键词(小写),覆盖无窗口的命令行工具
const SERIAL_NAME_HINTS: &[&str] = &[
    "serial", "putty", "sscom", "tera", "modbus", "uart", "accessport", "xcom", "qcom", "vspd",
    "comdbg", "串口",
];

/// 扫描:返回持有串口句柄的进程(端口去重排序)
pub fn scan_serial_processes() -> Vec<SerialProcess> {
    let device_map = com_device_map();
    if device_map.is_empty() {
        return Vec::new();
    }
    let targets: BTreeSet<String> = device_map.values().cloned().collect();

    let my_pid = unsafe { windows::Win32::System::Threading::GetCurrentProcessId() };

    let started = std::time::Instant::now();
    // 打开一个已知 File 句柄作为"类型探针",用于在句柄表中识别 File 对象类型编号
    let probe = open_file_probe();

    // 句柄表缓冲,按需倍增
    let mut buf_len: u32 = 1 << 20;
    let mut buf: Vec<u8>;
    let entries_ptr = loop {
        buf = vec![0u8; buf_len as usize];
        let mut ret: u32 = 0;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_EXTENDED_HANDLE_INFORMATION,
                buf.as_mut_ptr() as *mut c_void,
                buf_len,
                &mut ret,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            buf_len = (ret * 2).max(buf_len * 2);
            if buf_len > 1 << 28 {
                if let Some(h) = probe {
                    unsafe {
                        let _ = CloseHandle(h);
                    }
                }
                return Vec::new();
            }
            continue;
        }
        if status < 0 {
            if let Some(h) = probe {
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
            return Vec::new();
        }
        break unsafe {
            let info = &*(buf.as_ptr() as *const SystemHandleInfoEx);
            std::slice::from_raw_parts(
                buf.as_ptr().add(size_of::<SystemHandleInfoEx>()) as *const SystemHandleEntryEx,
                info.number_of_handles,
            )
        };
    };

    // 从探针句柄取 File 类型编号
    let file_type: Option<u16> = probe.and_then(|ph| {
        let want = ph.0 as usize;
        let t = entries_ptr
            .iter()
            .find(|e| e.unique_process_id as u32 == my_pid && e.handle_value == want)
            .map(|e| e.object_type_index);
        unsafe {
            let _ = CloseHandle(ph);
        }
        t
    });

    // 候选进程:当前会话内,有可见顶层窗口,或进程名命中串口工具关键词。
    // NtQueryObject 对个别句柄会长时间阻塞,预筛把任务量从 ~2 万降到数千。
    let mut my_session = 0u32;
    let _ = unsafe { ProcessIdToSessionId(my_pid, &mut my_session) };
    let mut candidates: HashSet<u32> = visible_window_pids();
    for pid in entries_ptr
        .iter()
        .map(|e| e.unique_process_id as u32)
        .collect::<BTreeSet<_>>()
    {
        if pid == 0 || pid == my_pid {
            continue;
        }
        let mut s = 0u32;
        if unsafe { ProcessIdToSessionId(pid, &mut s).is_ok() } && s == my_session {
            if let Some(name) = process_file_name(pid) {
                if SERIAL_NAME_HINTS.iter().any(|k| name.to_ascii_lowercase().contains(k)) {
                    candidates.insert(pid);
                }
            }
        }
    }

    // 待查任务:(pid, 句柄值, 访问权限)。File 类型 + 候选进程
    let mut all: Vec<(u32, usize, u32)> = entries_ptr
        .iter()
        .filter(|e| {
            let pid = e.unique_process_id as u32;
            pid != my_pid
                && candidates.contains(&pid)
                && file_type.is_none_or(|t| e.object_type_index == t)
        })
        .map(|e| (e.unique_process_id as u32, e.handle_value, e.granted_access))
        .collect();
    // 高优先在前:同时具备 FILE_READ_DATA|FILE_WRITE_DATA(0x3) 的句柄。
    // 句柄表 GrantedAccess 存的是 generic 映射后的具体权限,串口读写句柄必带 0x3。
    all.sort_by_key(|t| if t.2 & 0x3 == 0x3 { 0 } else { 1 });
    let n_hi = all.iter().take_while(|t| t.2 & 0x3 == 0x3).count();
    let tasks = Arc::new(all);
    let pids: BTreeSet<u32> = tasks.iter().map(|t| t.0).collect();

    // 主线程统一打开所有相关进程一次,worker 共享句柄值(句柄表进程内全局,可并发使用)
    let mut proc_map: HashMap<u32, SharedH> = HashMap::new();
    for pid in &pids {
        if let Ok(h) = unsafe {
            OpenProcess(PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION, false, *pid)
        } {
            proc_map.insert(*pid, SharedH(h));
        }
    }
    let proc_map = Arc::new(proc_map);

    // NtQueryObject 对个别句柄可能无限阻塞:16 worker 并行取名,
    // 硬截止时间 8s 兜底;高优先批扫完且经过 800ms 宽限即提前结束。
    // 仍阻塞的 worker 不 join,任其随进程退出回收(不占 CPU)。
    let targets = Arc::new(targets);
    let cursor = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<(u32, String)>();
    let mut workers = Vec::new();
    for _ in 0..16 {
        let tasks = Arc::clone(&tasks);
        let targets = Arc::clone(&targets);
        let proc_map = Arc::clone(&proc_map);
        let cursor = Arc::clone(&cursor);
        let stop = Arc::clone(&stop);
        let tx = tx.clone();
        workers.push(std::thread::spawn(move || {
            scan_worker(&tasks, &targets, &proc_map, &cursor, &stop, &tx);
        }));
    }
    drop(tx);

    // pid -> 命中的大写设备路径
    let mut hits: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    let hard_deadline = std::time::Duration::from_secs(8);
    let hi_grace = std::time::Duration::from_millis(800);
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok((pid, dev)) => {
                hits.entry(pid).or_default().insert(dev);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let hi_done = cursor.load(Ordering::Relaxed) >= n_hi;
                if started.elapsed() >= hard_deadline
                    || (hi_done && started.elapsed() >= hi_grace)
                {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop.store(true, Ordering::Relaxed);
    // 不 join(见上)
    drop(workers);
    // 关闭共享进程句柄(仅当无 worker 持有时;阻塞中的 worker 任其随进程退出回收)
    if let Ok(map) = Arc::try_unwrap(proc_map) {
        for (_, SharedH(h)) in map {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }

    // 设备路径 -> COMn
    let mut found: BTreeMap<u32, BTreeSet<String>> = BTreeMap::new();
    for (pid, devs) in hits {
        for dev in devs {
            for (com, mapped) in &device_map {
                if mapped == &dev {
                    found.entry(pid).or_default().insert(com.clone());
                }
            }
        }
    }

    let mut out = Vec::new();
    for (pid, ports) in found {
        let name = process_file_name(pid).unwrap_or_else(|| format!("<pid {}>", pid));
        let x64 = is_x64_process(pid);
        out.push(SerialProcess {
            pid,
            name,
            ports: ports.into_iter().collect(),
            x64,
        });
    }
    out.sort_by(|a, b| a.pid.cmp(&b.pid));
    out
}

/// 扫描 worker:原子游标抢占任务,共享进程句柄表;命中串口设备时上报 (pid, 大写路径)
fn scan_worker(
    tasks: &[(u32, usize, u32)],
    targets: &BTreeSet<String>,
    proc_map: &HashMap<u32, SharedH>,
    cursor: &AtomicUsize,
    stop: &AtomicBool,
    tx: &mpsc::Sender<(u32, String)>,
) {
    let cur_proc = unsafe { GetCurrentProcess() };
    let mut name_buf = vec![0u8; 4096];

    while !stop.load(Ordering::Relaxed) {
        let i = cursor.fetch_add(1, Ordering::Relaxed);
        let Some(&(pid, handle_value, _access)) = tasks.get(i) else { break };
        let Some(&SharedH(sh)) = proc_map.get(&pid) else {
            continue;
        };

        let mut dup = HANDLE(std::ptr::null_mut());
        let ok = unsafe {
            DuplicateHandle(
                sh,
                HANDLE(handle_value as *mut c_void),
                cur_proc,
                &mut dup,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok.is_err() {
            continue;
        }
        // 候选进程已预筛,直接取名与目标设备路径精确匹配
        if let Some(name) = query_object_name(dup, &mut name_buf) {
            let up = name.to_ascii_uppercase();
            if targets.contains(&up) && tx.send((pid, up)).is_err() {
                unsafe {
                    let _ = CloseHandle(dup);
                }
                break;
            }
        }
        unsafe {
            let _ = CloseHandle(dup);
        }
    }
}
