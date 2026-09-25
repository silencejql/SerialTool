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

    // 嵌入 exe 图标(assets/app_icon.rc 引用 icon.ico)
    println!("cargo:rerun-if-changed=assets/app_icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/app_icon.rc", embed_resource::NONE);

    // ---- 版本号 ----
    // 读取 assets/version.txt(语义化版本 主.次.补丁)作为本次构建版本号;
    // release.ps1 发布成功后自动把补丁位 +1 写回,故首版为 1.0.1、之后 1.0.2 ……
    // debug 构建带 -dev 后缀。
    let version_file = manifest.join("assets").join("version.txt");
    let version = std::fs::read_to_string(&version_file)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()));
    println!("cargo:rerun-if-changed=assets/version.txt");
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let suffix = if profile == "release" { "" } else { "-dev" };
    println!("cargo:rustc-env=APP_VERSION={version}{suffix}");
}
