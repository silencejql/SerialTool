//! serial_agent:注入目标进程的监控 DLL
//!
//! inline hook(kernel32):CreateFileW/A 记录串口句柄 → COMn 映射;
//! WriteFile 抓 TX(程序→设备);ReadFile / GetOverlappedResult 抓 RX(设备→程序);
//! CloseHandle 清理映射。事件经命名管道 `\\.\pipe\serialtool_mon` 回传监控端。
//!
//! 设计约束:
//! - 本 DLL panic = abort,任何 hook 回调都不允许 panic(会杀死目标进程)
//! - DllMain 零重逻辑,只 CreateThread;真正初始化在 worker 线程(避开 loader lock)
#![allow(clippy::missing_safety_doc)]

use std::collections::{HashMap, VecDeque};
use std::ffi::{c_void, CStr};
use std::mem::size_of;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicIsize, AtomicBool, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use retour::GenericDetour;
use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::Foundation::{
    BOOL, CloseHandle, HANDLE, HMODULE, SetLastError, GetLastError, ERROR_IO_PENDING,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, QueryDosDeviceW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_MODE, OPEN_EXISTING,
};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::LibraryLoader::{
    FreeLibraryAndExitThread, GetModuleFileNameW, GetModuleHandleW, GetProcAddress,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateThread, GetCurrentProcessId, ResetEvent, Sleep, WaitForSingleObject,
    LPTHREAD_START_ROUTINE, THREAD_CREATION_FLAGS,
};

const PIPE_NAME: &str = r"\\.\pipe\serialtool_mon";

// ---- 帧类型(agent → 监控端) ----
const FT_ATTACH: u8 = 1;
const FT_RX: u8 = 2;
const FT_TX: u8 = 3;
const FT_INFO: u8 = 4;

// ---- 下行命令(监控端 → agent) ----
const CMD_DETACH: u8 = 1;

// ---- 全局状态 ----
static HMODULE_SELF: AtomicIsize = AtomicIsize::new(0);
static RUNNING: AtomicBool = AtomicBool::new(true);
/// hook 是否已安装(只装一次;断线重连不重复 enable)
static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);
/// 裸指针的 Send 包装(仅在 ReadFile→GetOverlappedResult 调用窗口内解引用)
struct SendPtr(*mut u8);
unsafe impl Send for SendPtr {}

/// HANDLE 数值 -> "COMn"
static HANDLES: Lazy<Mutex<HashMap<usize, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));
/// 异步读:OVERLAPPED 指针 -> (HANDLE 数值, buffer)
static PENDING: Lazy<Mutex<HashMap<usize, (usize, SendPtr)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
/// 待发送帧队列
static QUEUE: Lazy<Mutex<VecDeque<Vec<u8>>>> = Lazy::new(|| Mutex::new(VecDeque::new()));

// =====================================================================
// 帧编解码
// =====================================================================

fn pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

fn build_frame(t: u8, port: &str, data: &[u8]) -> Vec<u8> {
    let pb = port.as_bytes();
    let plen = pb.len().min(u16::MAX as usize) as u16;
    let mut v = Vec::with_capacity(2 + 1 + 4 + 2 + plen as usize + 4 + data.len());
    v.extend_from_slice(&[0xA5, 0x5A, t]);
    v.extend_from_slice(&pid().to_le_bytes());
    v.extend_from_slice(&plen.to_le_bytes());
    v.extend_from_slice(&pb[..plen as usize]);
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(data);
    v
}

fn push_frame(t: u8, port: &str, data: &[u8]) {
    let f = build_frame(t, port, data);
    if let Ok(mut q) = QUEUE.lock() {
        if q.len() < 4096 {
            q.push_back(f);
        }
    }
}

/// 从路径中提取 "COMn"。接受 `COM3`、`\\.\COM10` 等形式
fn extract_com(name: &str) -> Option<String> {
    let up = name.to_ascii_uppercase().replace('/', "\\");
    let body = up.rsplit('\\').next().unwrap_or(&up);
    let digits = body.strip_prefix("COM")?;
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(body.to_string())
}

fn extract_com_w(p: PCWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut chars: Vec<u16> = Vec::with_capacity(64);
    unsafe {
        let mut q = p.0;
        while *q != 0 {
            chars.push(*q);
            q = q.add(1);
            if chars.len() > 260 {
                return None;
            }
        }
    }
    extract_com(&String::from_utf16_lossy(&chars))
}

fn extract_com_a(p: PCSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p.0 as *const c_char) }.to_string_lossy();
    extract_com(&s)
}

fn lookup_port(h: HANDLE) -> Option<String> {
    HANDLES.lock().ok()?.get(&(h.0 as usize)).cloned()
}

// =====================================================================
// Hook 函数
// =====================================================================

type FnCreateFileW = unsafe extern "system" fn(
    PCWSTR,
    u32,
    u32,
    *const c_void,
    u32,
    u32,
    HANDLE,
) -> HANDLE;
type FnCreateFileA = unsafe extern "system" fn(
    PCSTR,
    u32,
    u32,
    *const c_void,
    u32,
    u32,
    HANDLE,
) -> HANDLE;
type FnReadFile =
    unsafe extern "system" fn(HANDLE, *mut u8, u32, *mut u32, *mut OVERLAPPED) -> BOOL;
type FnWriteFile =
    unsafe extern "system" fn(HANDLE, *const u8, u32, *mut u32, *mut OVERLAPPED) -> BOOL;
type FnGetOverlappedResult =
    unsafe extern "system" fn(HANDLE, *mut OVERLAPPED, *mut u32, BOOL) -> BOOL;
type FnCloseHandle = unsafe extern "system" fn(HANDLE) -> BOOL;

static HOOK_CREATE_W: Lazy<GenericDetour<FnCreateFileW>> = Lazy::new(|| {
    let target: FnCreateFileW = unsafe { std::mem::transmute(proc_addr(b"CreateFileW\0")) };
    unsafe {
        GenericDetour::new(target, detour_create_w)
            .unwrap_or_else(|_| std::process::abort())
    }
});
static HOOK_CREATE_A: Lazy<GenericDetour<FnCreateFileA>> = Lazy::new(|| {
    let target: FnCreateFileA = unsafe { std::mem::transmute(proc_addr(b"CreateFileA\0")) };
    unsafe {
        GenericDetour::new(target, detour_create_a)
            .unwrap_or_else(|_| std::process::abort())
    }
});
static HOOK_READ: Lazy<GenericDetour<FnReadFile>> = Lazy::new(|| {
    let target: FnReadFile = unsafe { std::mem::transmute(proc_addr(b"ReadFile\0")) };
    unsafe {
        GenericDetour::new(target, detour_read_file)
            .unwrap_or_else(|_| std::process::abort())
    }
});
static HOOK_WRITE: Lazy<GenericDetour<FnWriteFile>> = Lazy::new(|| {
    let target: FnWriteFile = unsafe { std::mem::transmute(proc_addr(b"WriteFile\0")) };
    unsafe {
        GenericDetour::new(target, detour_write_file)
            .unwrap_or_else(|_| std::process::abort())
    }
});
static HOOK_OVERLAPPED: Lazy<GenericDetour<FnGetOverlappedResult>> = Lazy::new(|| {
    let target: FnGetOverlappedResult =
        unsafe { std::mem::transmute(proc_addr(b"GetOverlappedResult\0")) };
    unsafe {
        GenericDetour::new(target, detour_get_overlapped_result)
            .unwrap_or_else(|_| std::process::abort())
    }
});
static HOOK_CLOSE: Lazy<GenericDetour<FnCloseHandle>> = Lazy::new(|| {
    let target: FnCloseHandle = unsafe { std::mem::transmute(proc_addr(b"CloseHandle\0")) };
    unsafe {
        GenericDetour::new(target, detour_close_handle)
            .unwrap_or_else(|_| std::process::abort())
    }
});

fn proc_addr(name: &[u8]) -> *const c_void {
    unsafe {
        let hm = GetModuleHandleW(PCWSTR(w!("kernel32.dll").as_ptr())).unwrap_or_default();
        GetProcAddress(hm, PCSTR(name.as_ptr()))
            .map(|a| a as *const c_void)
            .unwrap_or(std::ptr::null())
    }
}

unsafe extern "system" fn detour_create_w(
    name: PCWSTR,
    access: u32,
    share: u32,
    sec: *const c_void,
    disp: u32,
    flags: u32,
    templ: HANDLE,
) -> HANDLE {
    let port = extract_com_w(name);
    let ret = HOOK_CREATE_W.call(name, access, share, sec, disp, flags, templ);
    let err = GetLastError();
    if ret != INVALID_HANDLE_VALUE && !ret.0.is_null() {
        if let Some(p) = port {
            if let Ok(mut m) = HANDLES.lock() {
                m.insert(ret.0 as usize, p);
            }
        }
    }
    SetLastError(err);
    ret
}

unsafe extern "system" fn detour_create_a(
    name: PCSTR,
    access: u32,
    share: u32,
    sec: *const c_void,
    disp: u32,
    flags: u32,
    templ: HANDLE,
) -> HANDLE {
    let port = extract_com_a(name);
    let ret = HOOK_CREATE_A.call(name, access, share, sec, disp, flags, templ);
    let err = GetLastError();
    if ret != INVALID_HANDLE_VALUE && !ret.0.is_null() {
        if let Some(p) = port {
            if let Ok(mut m) = HANDLES.lock() {
                m.insert(ret.0 as usize, p);
            }
        }
    }
    SetLastError(err);
    ret
}

unsafe extern "system" fn detour_write_file(
    h: HANDLE,
    buf: *const u8,
    n: u32,
    nwritten: *mut u32,
    ov: *mut OVERLAPPED,
) -> BOOL {
    let ret = HOOK_WRITE.call(h, buf, n, nwritten, ov);
    let err = GetLastError();
    if !buf.is_null() && n > 0 {
        if let Some(port) = lookup_port(h) {
            // 同步成功:直接记录;overlapped 发送返回 ERROR_IO_PENDING 时也在此立即快照
            // (缓冲在调用返回瞬间仍有效,push_frame 同步拷贝字节),否则 TX 会全部漏掉。
            let ok_sync = ret.as_bool();
            let pending = !ok_sync && err == ERROR_IO_PENDING;
            if ok_sync || pending {
                let data = std::slice::from_raw_parts(buf, n as usize);
                push_frame(FT_TX, &port, data);
            }
        }
    }
    SetLastError(err);
    ret
}

unsafe extern "system" fn detour_read_file(
    h: HANDLE,
    buf: *mut u8,
    n: u32,
    nread: *mut u32,
    ov: *mut OVERLAPPED,
) -> BOOL {
    let ret = HOOK_READ.call(h, buf, n, nread, ov);
    let err = GetLastError();
    if ret.as_bool() {
        if !nread.is_null() {
            let got = *nread;
            if got > 0 && !buf.is_null() {
                if let Some(port) = lookup_port(h) {
                    let data = std::slice::from_raw_parts(buf, got as usize);
                    push_frame(FT_RX, &port, data);
                }
            }
        }
    } else if err == ERROR_IO_PENDING && !ov.is_null() && !buf.is_null() {
        // overlapped 异步:记录待完成映射,由 GetOverlappedResult 补抓
        if let Ok(mut p) = PENDING.lock() {
            p.insert(ov as usize, (h.0 as usize, SendPtr(buf)));
        }
    }
    SetLastError(err);
    ret
}

unsafe extern "system" fn detour_get_overlapped_result(
    h: HANDLE,
    ov: *mut OVERLAPPED,
    count: *mut u32,
    wait: BOOL,
) -> BOOL {
    let ret = HOOK_OVERLAPPED.call(h, ov, count, wait);
    let err = GetLastError();
    if ret.as_bool() && !count.is_null() && !ov.is_null() {
        let n = *count;
        if n > 0 {
            let taken = PENDING.lock().ok().and_then(|mut p| p.remove(&(ov as usize)));
            if let Some((_, sp)) = taken {
                let buf = sp.0;
                if let Some(port) = lookup_port(h) {
                    let data = std::slice::from_raw_parts(buf, n as usize);
                    push_frame(FT_RX, &port, data);
                }
            }
        }
    }
    SetLastError(err);
    ret
}

unsafe extern "system" fn detour_close_handle(h: HANDLE) -> BOOL {
    let ret = HOOK_CLOSE.call(h);
    let err = GetLastError();
    if ret.as_bool() {
        if let Ok(mut m) = HANDLES.lock() {
            m.remove(&(h.0 as usize));
        }
    }
    SetLastError(err);
    ret
}

// =====================================================================
// 管道通信与安装流程
// =====================================================================

fn connect_pipe() -> Option<HANDLE> {
    let mut wide: Vec<u16> = PIPE_NAME.encode_utf16().collect();
    wide.push(0);
    // 持续等待监控端挂出管道:监控端重启/重复注入场景下,驻留的 worker 靠此自动重连。
    // 未收到 detach 前不放弃(此时若 hook 已装则继续采集入队,未装则零开销等待)。
    loop {
        if !RUNNING.load(Ordering::Relaxed) {
            return None;
        }
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                0x4000_0000 | 0x8000_0000, // GENERIC_WRITE | GENERIC_READ
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                // overlapped 句柄:worker 用带超时的 overlapped 读可靠感知命令与对端断开
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                HANDLE::default(),
            )
        };
        if let Ok(h) = h {
            return Some(h);
        }
        unsafe { Sleep(200) };
    }
}

// =====================================================================
// 注入时收养进程已打开的串口句柄
// =====================================================================
//
// 注入发生时目标程序往往早已打开串口,CreateFile hook 无法补记这些句柄,
// 因此 attach 时自行枚举:系统句柄表(仅本进程) → 设备名 → COMn 反查。

#[repr(C)]
struct SystemHandleInfoEx {
    number_of_handles: usize,
    _reserved: usize,
}

#[repr(C)]
struct SystemHandleEntryEx {
    object: *mut c_void,
    unique_process_id: usize,
    handle_value: usize,
    granted_access: u32,
    _creator_back_trace_index: u16,
    object_type_index: u16,
    _handle_attributes: u32,
    _reserved2: u32,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    _maximum_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct ObjectNameInformation {
    name: UnicodeString,
}

const STATUS_INFO_LENGTH_MISMATCH: i32 = -1_073_741_820; // 0xC0000004
const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
const OBJECT_NAME_INFORMATION_CLASS: u32 = 1;

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
    fn NtQueryObject(
        handle: HANDLE,
        class: u32,
        info: *mut c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
}

unsafe fn snapshot_handles() -> Option<Vec<u8>> {
    let mut cap: usize = 1 << 16;
    loop {
        let mut buf = vec![0u8; cap];
        let mut need: u32 = 0;
        let st = NtQuerySystemInformation(
            SYSTEM_EXTENDED_HANDLE_INFORMATION,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
            &mut need,
        );
        if st == 0 {
            return Some(buf);
        }
        if st != STATUS_INFO_LENGTH_MISMATCH {
            return None;
        }
        let next = ((need as usize).saturating_add(4096)).max(cap * 2);
        if next > 1 << 28 {
            return None;
        }
        cap = next;
    }
}

unsafe fn query_object_name(handle: HANDLE) -> Option<String> {
    let mut buf = vec![0u8; 512];
    let mut need: u32 = 0;
    let mut st = NtQueryObject(
        handle,
        OBJECT_NAME_INFORMATION_CLASS,
        buf.as_mut_ptr() as *mut c_void,
        buf.len() as u32,
        &mut need,
    );
    if st == STATUS_INFO_LENGTH_MISMATCH && need as usize > buf.len() {
        buf.resize(need as usize + 16, 0);
        st = NtQueryObject(
            handle,
            OBJECT_NAME_INFORMATION_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
            &mut need,
        );
    }
    if st != 0 {
        return None;
    }
    let info = &*(buf.as_ptr() as *const ObjectNameInformation);
    if info.name.buffer.is_null() || info.name.length == 0 {
        return None;
    }
    let chars = info.name.length as usize / 2;
    let slice = std::slice::from_raw_parts(info.name.buffer, chars);
    Some(String::from_utf16_lossy(slice))
}

/// 建立 `\DEVICE\VSERIAL_0`(大写) -> `COM10` 映射
unsafe fn com_device_map() -> HashMap<String, String> {
    let mut map = HashMap::new();
    // QueryDosDeviceW(NULL) 缓冲不足时返回 0(ERROR_MORE_DATA),需倍增重试
    let mut cap: usize = 16384;
    let (list, n) = loop {
        let mut list = vec![0u16; cap];
        let n = QueryDosDeviceW(PCWSTR::null(), Some(&mut list)) as usize;
        if n > 0 {
            break (list, n);
        }
        if cap >= 1 << 20 {
            return map;
        }
        cap *= 4;
    };
    if n == 0 {
        return map;
    }
    let mut start = 0usize;
    while start < n {
        let end = list[start..].iter().position(|&c| c == 0).map(|p| start + p);
        let end = match end {
            Some(e) if e > start => e,
            _ => break,
        };
        let dos = String::from_utf16_lossy(&list[start..end]);
        start = end + 1;
        // 仅查 COMn,避免遍历 C:/D:/管道等全部 DOS 名
        if extract_com(&dos).is_none() {
            continue;
        }
        let mut wide: Vec<u16> = dos.encode_utf16().collect();
        wide.push(0);
        let mut dev = vec![0u16; 512];
        let m = QueryDosDeviceW(PCWSTR(wide.as_ptr()), Some(&mut dev)) as usize;
        if m > 0 {
            let z = dev.iter().position(|&c| c == 0).unwrap_or(m);
            let path = String::from_utf16_lossy(&dev[..z]).to_ascii_uppercase();
            map.insert(path, dos);
        }
    }
    map
}

/// 收养目标进程注入前已打开的串口句柄,填入 HANDLES 映射
unsafe fn adopt_existing_handles() {
    let devmap = com_device_map();
    if devmap.is_empty() {
        return;
    }

    // File 类型探针:打开自身 exe,从句柄表反查 ObjectTypeIndex
    let mut exe = vec![0u16; 1024];
    let plen = GetModuleFileNameW(HMODULE::default(), &mut exe) as usize;
    let probe = if plen > 0 {
        exe.truncate(plen);
        exe.push(0);
        CreateFileW(
            PCWSTR(exe.as_ptr()),
            0x80, // FILE_READ_ATTRIBUTES
            FILE_SHARE_MODE(7),
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
        .ok()
    } else {
        None
    };

    let buf = match snapshot_handles() {
        Some(b) => b,
        None => {
            if let Some(h) = probe {
                let _ = CloseHandle(h);
            }
            return;
        }
    };
    let head = &*(buf.as_ptr() as *const SystemHandleInfoEx);
    let count = head.number_of_handles.min(
        (buf.len() - size_of::<SystemHandleInfoEx>()) / size_of::<SystemHandleEntryEx>(),
    );
    let entries = std::slice::from_raw_parts(
        buf.as_ptr()
            .add(size_of::<SystemHandleInfoEx>()) as *const SystemHandleEntryEx,
        count,
    );
    let me = GetCurrentProcessId() as usize;
    let file_type = probe.and_then(|ph| {
        let idx = entries
            .iter()
            .find(|e| e.unique_process_id == me && e.handle_value == ph.0 as usize)
            .map(|e| e.object_type_index);
        let _ = CloseHandle(ph);
        idx
    });

    let mut adopted: u32 = 0;
    if let Ok(mut map) = HANDLES.lock() {
        for e in entries.iter().filter(|e| e.unique_process_id == me) {
            if let Some(ti) = file_type {
                if e.object_type_index != ti {
                    continue;
                }
            }
            if map.contains_key(&e.handle_value) {
                continue;
            }
            if let Some(name) = query_object_name(HANDLE(e.handle_value as *mut c_void)) {
                if let Some(com) = devmap.get(&name.to_ascii_uppercase()) {
                    map.insert(e.handle_value, com.clone());
                    adopted += 1;
                }
            }
        }
    }
    if adopted > 0 {
        push_frame(
            FT_INFO,
            "",
            format!("已接管 {adopted} 个注入前已打开的串口句柄").as_bytes(),
        );
    }
}

fn install_hooks() -> bool {
    unsafe {
        let all = [
            HOOK_CREATE_W.enable().is_ok(),
            HOOK_CREATE_A.enable().is_ok(),
            HOOK_READ.enable().is_ok(),
            HOOK_WRITE.enable().is_ok(),
            HOOK_OVERLAPPED.enable().is_ok(),
            HOOK_CLOSE.enable().is_ok(),
        ];
        all.iter().all(|x| *x)
    }
}

/// 幂等安装 hook(重连时不重复 enable,retour 对已启用 detour 再 enable 会报错)
fn ensure_hooks() -> bool {
    if HOOKS_INSTALLED.load(Ordering::Acquire) {
        return true;
    }
    if install_hooks() {
        HOOKS_INSTALLED.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

unsafe fn disable_hooks() {
    let _ = HOOK_CREATE_W.disable();
    let _ = HOOK_CREATE_A.disable();
    let _ = HOOK_READ.disable();
    let _ = HOOK_WRITE.disable();
    let _ = HOOK_OVERLAPPED.disable();
    let _ = HOOK_CLOSE.disable();
}

/// 向(overlapped)管道句柄写完整个缓冲
fn pipe_write_all(h: HANDLE, data: &[u8]) -> bool {
    let ev = match unsafe { CreateEventW(None, BOOL(1), BOOL(0), PCWSTR::null()) } {
        Ok(e) => e,
        Err(_) => return false,
    };
    let mut ok_all = true;
    let mut off = 0usize;
    while off < data.len() {
        let mut ov = OVERLAPPED::default();
        ov.hEvent = ev;
        unsafe {
            let _ = ResetEvent(ev);
        }
        let mut written: u32 = 0;
        let r = unsafe {
            WriteFile(
                h,
                Some(&data[off..]),
                Some(&mut written as *mut u32),
                Some(&mut ov as *mut OVERLAPPED),
            )
        };
        if r.is_err() {
            if unsafe { GetLastError().0 } == ERROR_IO_PENDING.0 {
                if unsafe { WaitForSingleObject(ev, u32::MAX) } != WAIT_OBJECT_0 {
                    ok_all = false;
                    break;
                }
                if unsafe { GetOverlappedResult(h, &ov, &mut written, true) }.is_err() {
                    ok_all = false;
                    break;
                }
            } else {
                ok_all = false;
                break;
            }
        }
        if written == 0 {
            ok_all = false;
            break;
        }
        off += written as usize;
    }
    unsafe {
        let _ = CloseHandle(ev);
    }
    ok_all
}

extern "system" fn worker(_: *mut c_void) -> u32 {
    // 等 DllMain 离开 loader lock
    unsafe { Sleep(100) };

    // 会话循环:监控端异常退出时不卸载 hook、不退出线程,保持采集并等待其重启重连;
    // 只有显式收到 detach 才跳出并自释放。这样重复"注入"也能让驻留 worker 重新上线。
    let mut detach = false;
    while !detach {
        let pipe = match connect_pipe() {
            Some(h) => h,
            None => break,
        };

        // overlapped 读所需的手动重置事件(每次会话一个)
        let ev = match unsafe { CreateEventW(None, BOOL(1), BOOL(0), PCWSTR::null()) } {
            Ok(h) => h,
            Err(_) => {
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(pipe);
                }
                unsafe { Sleep(500) };
                continue;
            }
        };

        if !ensure_hooks() {
            push_frame(FT_INFO, "", "hook 安装失败".as_bytes());
        }
        // 重连时重新收养期间新打开的串口句柄;INFO 帧入队,随队列在 ATTACH 后补发
        unsafe { adopt_existing_handles() };
        // 先报上线,监控端据此把状态切到"监控中"
        if !pipe_write_all(pipe, &build_frame(FT_ATTACH, "", b"")) {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(ev);
                let _ = windows::Win32::Foundation::CloseHandle(pipe);
            }
            continue;
        }

        // 会话级长挂 overlapped 读:一次发起,Wait 超时仅用来穿插上行 flush,
        // 不重复发起(否则同一事件堆积多个未完成 IRP)。命令到达/对端断开都会完成它。
        let mut cmd_buf = [0u8; 64];
        let mut cmd_got: u32 = 0;
        let mut cmd_ov = OVERLAPPED::default();
        cmd_ov.hEvent = ev;
        let mut read_pending = false;

        'pump: loop {
            // 批量发送队列(断线期间积压的数据在此补发)
            loop {
                let frame = QUEUE.lock().ok().and_then(|mut q| q.pop_front());
                match frame {
                    Some(f) => {
                        if !pipe_write_all(pipe, &f) {
                            break 'pump; // 管道已断
                        }
                    }
                    None => break,
                }
            }

            if !read_pending {
                unsafe {
                    let _ = ResetEvent(ev);
                }
                cmd_got = 0;
                let r = unsafe {
                    ReadFile(
                        pipe,
                        Some(&mut cmd_buf[..]),
                        Some(&mut cmd_got as *mut u32),
                        Some(&mut cmd_ov as *mut OVERLAPPED),
                    )
                };
                if r.is_err() && unsafe { GetLastError().0 } != ERROR_IO_PENDING.0 {
                    break 'pump;
                }
                read_pending = true;
            }

            match unsafe { WaitForSingleObject(ev, 100) } {
                WAIT_TIMEOUT => continue, // 同一读请求继续挂起
                WAIT_OBJECT_0 => {
                    read_pending = false;
                    if unsafe { GetOverlappedResult(pipe, &cmd_ov, &mut cmd_got, true) }.is_err() {
                        break 'pump;
                    }
                    if cmd_got == 0 {
                        break 'pump; // 对端关闭(EOF/broken)
                    }
                    // 帧形如 A5 5A cmd,扫描 detach
                    let mut is_detach = false;
                    let mut i = 0;
                    while i + 2 < cmd_got as usize {
                        if cmd_buf[i] == 0xA5 && cmd_buf[i + 1] == 0x5A {
                            if cmd_buf[i + 2] == CMD_DETACH {
                                is_detach = true;
                            }
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                    if is_detach {
                        detach = true;
                        break 'pump;
                    }
                }
                _ => break 'pump,
            }
        }

        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(ev);
            let _ = windows::Win32::Foundation::CloseHandle(pipe);
        }
        // Broken -> 保留 hook 与队列,回到 connect_pipe 等待新监控端;Detach -> 跳出收尾
    }

    // 仅在收到 detach 后到达:卸载 hook 并从目标进程自释放
    unsafe { disable_hooks() };
    let hmod = HMODULE_SELF.load(Ordering::Relaxed);
    if hmod != 0 {
        unsafe {
            FreeLibraryAndExitThread(HMODULE(hmod as *mut c_void), 0);
        }
    }
    0
}

#[no_mangle]
unsafe extern "system" fn DllMain(hinst: HANDLE, reason: u32, _reserved: *mut c_void) -> BOOL {
    const ATTACH: u32 = 1;
    if reason == ATTACH {
        HMODULE_SELF.store(hinst.0 as isize, Ordering::Relaxed);
        // CreateThread 是 DllMain 中少数安全的调用;worker 会先 Sleep 等待 loader lock
        let routine: LPTHREAD_START_ROUTINE = Some(worker);
        let _ = CreateThread(
            None,
            0,
            routine,
            None,
            THREAD_CREATION_FLAGS(0),
            None,
        );
    }
    BOOL(1)
}
