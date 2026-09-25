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

    // ---- 发布版本号 ----
    // release 构建时把 assets/build_num.txt 内的构建号 +1,
    // 组合成 "主.次.补丁.构建号"(如 0.1.0.7)通过 APP_VERSION 注入程序;
    // debug 构建不递增,带 -dev 后缀。发布请用 release.ps1(会强制重跑本脚本)。
    println!("cargo:rerun-if-env-changed=PROFILE");
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let num_file = manifest.join("assets").join("build_num.txt");
    let mut build_num: u32 = std::fs::read_to_string(&num_file)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if profile == "release" {
        build_num += 1;
        std::fs::write(&num_file, build_num.to_string()).expect("写入 build_num.txt 失败");
    }
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let suffix = if profile == "release" { "" } else { "-dev" };
    println!("cargo:rustc-env=APP_VERSION={pkg_version}.{build_num}{suffix}");
}
