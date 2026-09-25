//! 构建脚本:将 release 版 agent DLL(x64 与 x86 各一份)复制到 OUT_DIR,供主程序
//! `include_bytes!` 嵌入。注入时按目标进程位数选择对应 DLL。
//! 注意:agent DLL 始终用 release 构建(体积与稳定性),与主程序 profile 无关。
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = manifest.join("target");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // (相对 target 目录的源 DLL, 输出文件名)
    let agents = [
        ("release/serial_agent.dll", "serial_agent_x64.dll"),
        (
            "i686-pc-windows-msvc/release/serial_agent.dll",
            "serial_agent_x86.dll",
        ),
    ];

    let mut missing = Vec::new();
    for (rel, out_name) in agents {
        let src = target.join(rel);
        if !src.exists() {
            missing.push(rel);
            continue;
        }
        std::fs::copy(&src, out.join(out_name)).expect("复制 agent DLL 失败");
        println!("cargo:rerun-if-changed={}", src.display());
    }

    if !missing.is_empty() {
        panic!(
            "缺少 agent DLL: {missing:?}\n请先依次执行:\n    \
             cargo build -p serial_agent --release\n    \
             cargo build -p serial_agent --release --target i686-pc-windows-msvc",
        );
    }
}
