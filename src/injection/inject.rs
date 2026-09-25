//! 向目标进程注入 agent DLL(CreateRemoteThread + LoadLibraryW)。
//!
//! 支持两种目标:
//! - **x64 原生进程**:本进程(64 位)取 kernel32!LoadLibraryW 地址,直接远程线程。
//! - **WOW64 32 位进程**:不能用本进程的 64 位 LoadLibraryW 地址。改为枚举目标的
//!   32 位模块(`EnumProcessModulesEx` + `LIST_MODULES_32BIT`),读取远程 PE 导出表,
//!   解析出目标 32 位 kernel32(经 kernelbase 转发)中的 LoadLibraryW 地址,再创建
//!   WOW64 远程线程。仅支持向 32 位目标注入 32 位 agent DLL。

use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows::core::{s, w};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::ProcessStatus::{
    K32EnumProcessModulesEx, K32GetModuleBaseNameW,
};
use windows::Win32::System::Threading::{
    CreateRemoteThread, OpenProcess, WaitForSingleObject, PROCESS_CREATE_THREAD,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

/// 64 位 agent DLL(由 build.rs 复制)
const AGENT_DLL_X64: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/serial_agent_x64.dll"));
/// 32 位 agent DLL(注入 WOW64 进程)
const AGENT_DLL_X86: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/serial_agent_x86.dll"));

/// 释放与目标位数匹配的内嵌 DLL 到 %TEMP%(内容寻址:同版本复用,避免占用冲突)
fn extract_agent(target_x64: bool) -> Result<PathBuf, String> {
    let bytes = if target_x64 { AGENT_DLL_X64 } else { AGENT_DLL_X86 };
    let arch = if target_x64 { "x64" } else { "x86" };
    let mut hash: u64 = 1469598103934665603;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    let name = format!("serialtool_agent_{arch}_{:x}_{}.dll", hash, bytes.len());
    let path = std::env::temp_dir().join(name);
    if path.exists() {
        return Ok(path);
    }
    std::fs::write(&path, bytes).map_err(|e| format!("释放 agent DLL 失败: {e}"))?;
    Ok(path)
}

/// 注入指定 pid。自动判定目标位数并选择同位数 agent。返回 Err 时给出可直接展示的中文原因。
pub fn inject(pid: u32) -> Result<(), String> {
    let target_x64 = crate::injection::scan::is_x64_process(pid);

    unsafe {
        let proc = OpenProcess(
            PROCESS_CREATE_THREAD
                | PROCESS_VM_OPERATION
                | PROCESS_VM_READ
                | PROCESS_VM_WRITE
                | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
        .map_err(|e| {
            if e.code().0 as u32 == 5 {
                "打开目标进程被拒绝(错误码 5):目标可能以管理员身份运行,请以管理员重启本软件"
                    .to_string()
            } else {
                format!("打开目标进程失败: {e}")
            }
        })?;

        // agent 已驻留时(监控端重启/再次注入):LoadLibrary 只会增加引用计数且
        // DllMain 不再重入,驻留 worker 会通过常驻管道自动重连并重新上报 ATTACH,
        // 因此这里不能重复 LoadLibrary,否则 detach 时一次 FreeLibrary 无法卸载。
        if agent_already_loaded(proc, target_x64) {
            let _ = CloseHandle(proc);
            return Ok(());
        }

        let dll_path = extract_agent(target_x64)?;
        let mut wide: Vec<u16> = dll_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let result = if target_x64 {
            inject_x64(proc, &mut wide)
        } else {
            inject_wow64(proc, &mut wide)
        };
        let _ = CloseHandle(proc);
        result
    }
}

/// 在目标进程分配内存并写入 DLL 路径(宽字符),返回远程缓冲区地址
unsafe fn alloc_write_path(proc_h: HANDLE, wide_path: &[u16]) -> Result<usize, String> {
    let byte_len = wide_path.len() * 2;
    let remote_buf = VirtualAllocEx(
        proc_h,
        None,
        byte_len,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_READWRITE,
    );
    if remote_buf.is_null() {
        return Err(format!(
            "VirtualAllocEx 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    let addr = remote_buf as usize;
    let bytes = std::slice::from_raw_parts(wide_path.as_ptr() as *const u8, byte_len);
    if let Err(e) = WriteProcessMemory(
        proc_h,
        remote_buf,
        bytes.as_ptr() as *const c_void,
        bytes.len(),
        None,
    ) {
        let _ = VirtualFreeEx(proc_h, remote_buf, 0, MEM_RELEASE);
        return Err(format!("WriteProcessMemory 失败: {e}"));
    }
    Ok(addr)
}

/// 创建远程线程并等待 LoadLibrary 返回
unsafe fn run_remote_loadlibrary(
    proc_h: HANDLE,
    remote_buf: usize,
    loadlibrary_addr: usize,
) -> Result<(), String> {
    // FARPROC/线程入口 ABI 兼容(均为 system 调用约定,单指针参数)
    let start: windows::Win32::System::Threading::LPTHREAD_START_ROUTINE = Some(
        std::mem::transmute::<*const c_void, unsafe extern "system" fn(*mut c_void) -> u32>(
            loadlibrary_addr as *const c_void,
        ),
    );
    let thread = CreateRemoteThread(
        proc_h,
        None,
        0,
        start,
        Some(remote_buf as *mut c_void),
        0,
        None,
    )
    .map_err(|e| format!("CreateRemoteThread 失败(可能被杀软拦截): {e}"))?;
    let _ = WaitForSingleObject(thread, 5000);
    let _ = CloseHandle(thread);
    let _ = VirtualFreeEx(proc_h, remote_buf as *mut c_void, 0, MEM_RELEASE);
    Ok(())
}

/// 64 位目标:本进程 kernel32 与同会话 64 位目标加载基址一致,本地取地址即可
unsafe fn inject_x64(proc_h: HANDLE, wide_path: &mut [u16]) -> Result<(), String> {
    let remote_buf = alloc_write_path(proc_h, wide_path)?;
    let k32 = GetModuleHandleW(w!("kernel32.dll"))
        .map_err(|e| format!("GetModuleHandleW: {e}"))?;
    let load_addr = GetProcAddress(k32, s!("LoadLibraryW"))
        .ok_or_else(|| "GetProcAddress(LoadLibraryW) 失败".to_string())?;
    run_remote_loadlibrary(proc_h, remote_buf, load_addr as usize)
}

// =====================================================================
// WOW64(32 位目标)跨位数注入
// =====================================================================

/// LIST_MODULES_32BIT = 0x01 / LIST_MODULES_64BIT = 0x02
const LIST_MODULES_32BIT: u32 = 0x01;
const LIST_MODULES_64BIT: u32 = 0x02;

/// 释放到 TEMP 的 agent DLL 文件名前缀(内容寻址),用于驻留检测
const AGENT_FILE_PREFIX: &str = "serialtool_agent_";

/// 目标进程是否已加载我方 agent(按模块基名前缀判定)。
/// 64 位目标枚举 64 位模块;WOW64 目标必须枚举其 32 位模块。
unsafe fn agent_already_loaded(proc_h: HANDLE, target_x64: bool) -> bool {
    let filter = if target_x64 {
        LIST_MODULES_64BIT
    } else {
        LIST_MODULES_32BIT
    };
    match list_remote_modules(proc_h, filter) {
        Ok(mods) => mods.iter().any(|(n, _)| n.starts_with(AGENT_FILE_PREFIX)),
        Err(_) => false,
    }
}

/// 列出目标模块:(模块基名(小写), 基址)。filter 选择 32/64 位模块视图。
unsafe fn list_remote_modules(
    proc_h: HANDLE,
    filter: u32,
) -> Result<Vec<(String, usize)>, String> {
    let mut needed: u32 = 0;
    let _ = K32EnumProcessModulesEx(
        proc_h,
        std::ptr::null_mut(),
        0,
        &mut needed,
        filter,
    );
    if needed == 0 {
        return Err("枚举目标模块失败".to_string());
    }
    let count = needed as usize / size_of::<HMODULE>();
    let mut modules = vec![HMODULE::default(); count];
    if !K32EnumProcessModulesEx(
        proc_h,
        modules.as_mut_ptr(),
        needed,
        &mut needed,
        filter,
    )
    .as_bool()
    {
        return Err(format!(
            "EnumProcessModulesEx 失败: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut out = Vec::new();
    for m in modules {
        let mut name_buf = [0u16; 260];
        let len = K32GetModuleBaseNameW(proc_h, m, &mut name_buf) as usize;
        if len > 0 {
            let name = String::from_utf16_lossy(&name_buf[..len]).to_lowercase();
            out.push((name, m.0 as usize));
        }
    }
    Ok(out)
}

unsafe fn read_remote(proc_h: HANDLE, addr: usize, buf: &mut [u8]) -> Result<(), String> {
    let mut got: usize = 0;
    ReadProcessMemory(
        proc_h,
        addr as *const c_void,
        buf.as_mut_ptr() as *mut c_void,
        buf.len(),
        Some(&mut got),
    )
    .map_err(|e| format!("ReadProcessMemory@0x{addr:X} 失败: {e}"))?;
    if got != buf.len() {
        return Err(format!(
            "ReadProcessMemory@0x{addr:X} 读取不完整 {got}/{}",
            buf.len()
        ));
    }
    Ok(())
}

#[inline]
fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// 远程模块导出解析结果
enum Export {
    /// 直接导出:函数 RVA(相对模块基址)
    Direct(u32),
    /// 转发导出,如 kernel32.LoadLibraryW -> "KERNELBASE.LoadLibraryW"
    Forward { module: String, func: String },
}

/// 读取远程 32 位 PE 模块的指定导出函数(按名字)。
unsafe fn remote_export(
    proc_h: HANDLE,
    base: usize,
    want: &str,
) -> Result<Export, String> {
    // IMAGE_DOS_HEADER -> e_lfanew
    let mut dos = [0u8; 0x40];
    read_remote(proc_h, base, &mut dos)?;
    if &dos[0..2] != b"MZ" {
        return Err("远程模块缺少 MZ 标志".to_string());
    }
    let pe_off = u32le(&dos, 0x3C) as usize;

    // PE 头 + IMAGE_OPTIONAL_HEADER32(PE32)。导出目录 RVA 位于 optional header 偏移 96。
    let mut pe = [0u8; 0x200];
    read_remote(proc_h, base + pe_off, &mut pe)?;
    if &pe[0..4] != b"PE\0\0" {
        return Err("远程模块缺少 PE 标志".to_string());
    }
    if u16le(&pe, 24) != 0x010B {
        return Err("远程模块不是 32 位 PE(PE32)".to_string());
    }
    let export_rva = u32le(&pe, 24 + 96) as usize;
    let export_size = u32le(&pe, 24 + 100) as usize;
    if export_rva == 0 {
        return Err("远程模块没有导出表".to_string());
    }

    // IMAGE_EXPORT_DIRECTORY(40 字节)
    let mut exp = [0u8; 40];
    read_remote(proc_h, base + export_rva, &mut exp)?;
    let n_names = u32le(&exp, 24) as usize;
    let addr_functions = u32le(&exp, 28) as usize;
    let addr_names = u32le(&exp, 32) as usize;
    let addr_name_ordinals = u32le(&exp, 36) as usize;

    let mut name_ptrs = vec![0u8; 4 * n_names];
    read_remote(proc_h, base + addr_names, &mut name_ptrs)?;
    let mut ord_tbl = vec![0u8; 2 * n_names];
    read_remote(proc_h, base + addr_name_ordinals, &mut ord_tbl)?;

    for i in 0..n_names {
        let name_rva = u32le(&name_ptrs, i * 4) as usize;
        let mut nb = [0u8; 64];
        if read_remote(proc_h, base + name_rva, &mut nb).is_err() {
            continue;
        }
        let end = nb.iter().position(|&c| c == 0).unwrap_or(nb.len());
        if nb[..end].eq_ignore_ascii_case(want.as_bytes()) {
            let ord = u16le(&ord_tbl, i * 2) as usize;
            let mut fr = [0u8; 4];
            read_remote(proc_h, base + addr_functions + ord * 4, &mut fr)?;
            let func_rva = u32le(&fr, 0) as usize;

            // 落在导出目录范围内 => 转发器
            if export_size > 0
                && func_rva >= export_rva
                && func_rva < export_rva + export_size
            {
                let mut fb = [0u8; 128];
                read_remote(proc_h, base + func_rva, &mut fb)?;
                let end = fb.iter().position(|&c| c == 0).unwrap_or(fb.len());
                let fwd = String::from_utf8_lossy(&fb[..end]);
                if let Some((m, f)) = fwd.split_once('.') {
                    return Ok(Export::Forward {
                        module: m.to_string(),
                        func: f.to_string(),
                    });
                }
                return Err(format!("无法解析转发导出: {fwd}"));
            }
            return Ok(Export::Direct(func_rva as u32));
        }
    }
    Err(format!("32 位模块导出中找不到 {want}"))
}

/// 在模块列表中按基名查找基址(kernel32.dll / kernelbase.dll …)
fn base_of<'a>(modules: &'a [(String, usize)], name: &str) -> Option<usize> {
    modules
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, base)| *base)
}

/// 32 位 WOW64 目标:解析其 32 位 LoadLibraryW 地址后远程线程
unsafe fn inject_wow64(proc_h: HANDLE, wide_path: &mut [u16]) -> Result<(), String> {
    let modules = list_remote_modules(proc_h, LIST_MODULES_32BIT)?;
    let k32 = base_of(&modules, "kernel32.dll")
        .ok_or_else(|| "目标 32 位模块列表中没有 kernel32.dll".to_string())?;

    // 解析 LoadLibraryW,处理 kernel32 -> kernelbase(或 api-ms-*) 的转发
    let (lib_base, func_rva) = match remote_export(proc_h, k32, "LoadLibraryW")? {
        Export::Direct(rva) => (k32, rva as usize),
        Export::Forward { module, func } => {
            let raw = module.to_lowercase();
            let candidate = if raw.ends_with(".dll") {
                raw.clone()
            } else {
                format!("{raw}.dll")
            };
            let fwd_base = base_of(&modules, &candidate)
                .or_else(|| base_of(&modules, "kernelbase.dll"))
                .ok_or_else(|| format!("转发目标模块 {candidate} 未在目标中加载"))?;
            match remote_export(proc_h, fwd_base, &func)? {
                Export::Direct(rva) => (fwd_base, rva as usize),
                Export::Forward { .. } => {
                    return Err("LoadLibraryW 存在多级转发,无法解析".to_string())
                }
            }
        }
    };
    let load_addr = lib_base + func_rva;

    let remote_buf = alloc_write_path(proc_h, wide_path)?;
    run_remote_loadlibrary(proc_h, remote_buf, load_addr)
}
