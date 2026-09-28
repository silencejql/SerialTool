# SerialTool

体积小、功能强的串口收发与注入式监控上位机。基于 Rust + egui 构建，单文件绿色 portable，同时提供 GUI 与 CLI 诊断命令。

当前版本：**1.0.8**

## 特性

### 串口收发（收发页）
- 基础参数：波特率 / 数据位 / 停止位 / 校验 / 流控
- 两种接收组帧模式（左侧面板可切换，持久化）
  - **空闲间隔**：总线空闲 > 10ms 作为一帧边界，自动合并被驱动拆碎的多次 read
  - **换行符**：按 `\n` 切分一行为一帧（兼容 `\r\n`），无换行结尾的残余等 100ms 兜底出帧
- HEX / ASCII 双向显示与发送
- HEX 解析容错：支持 `0x` 前缀、空格/逗号分隔、连续书写（中间的 `0x` 不会被误删）
- 实时关键词过滤：端口 / RX / TX / HEX / 文本 / 进程名，空格分隔多条件任一命中
- 数据区悬浮工具条：显示格式切换、自动滚动、清空显示、保存日志
- 发送区：Ctrl+Enter 发送、追加 CRLF、定时发送（最小 20ms）、清空输入
- TX/RX 字节计数

### 预设（左侧面板）
- 多条发送预设，支持 HEX / ASCII、追加 CRLF、定时重发
- 内联展开编辑，一键发送

### 注入式监控（监控页）
- 扫描持有串口句柄的进程，一键注入 agent DLL
- agent 通过 inline hook 捕获 `CreateFileW/A`、`ReadFile`、`WriteFile`、`GetOverlappedResult`、`CloseHandle`
- 双向数据流实时显示（RX/TX 分色），与收发页共用组帧模式
- 同时支持 **x64 与 x86 (WOW64)** 目标，注入时自动匹配目标位数
- PID 白名单门控：避免 agent 管道自连，`取消监控` 后驻留 agent 不会自动接回
- detach 安全：冻结目标线程、修正指令指针、原子恢复 hook，避免高频读线程撞上半恢复代码导致崩溃
- agent 数据通知事件即时冲刷，配合监控页 10ms 空闲 / 换行聚合，一条消息不会被拆成多帧
- 目标退出探活：CLI 每 200ms、GUI 每秒，自动收回白名单、冲出尾帧并提示

### 日志
- 持久化日志（按天分文件），日志目录跟随配置目录
- 日志查询页：关键词搜索、按文件筛选、导出
- 实时收发与监控数据同步写入日志

### 配置与便携
- 配置文件 `config.json`、日志目录 `logs\` 路径优先级：
  1. 环境变量 `SERIALTOOL_CONFIG_DIR`
  2. exe 同级 `config_path.txt` 指针文件
  3. exe 所在目录（不可写则退回 `%APPDATA%\SerialTool`）
- 旧版 `%APPDATA%\SerialTool\config.json` 首次启动自动迁移

## 安装与构建

### 运行环境
- Windows 7+ (x64)
- 依赖 DLL 已嵌入 exe，无需额外运行时

### 从源码构建

```powershell
# Rust 工具链（stable，需 i686 目标用于 x86 agent）
rustup target add i686-pc-windows-msvc

# 构建 release（自动嵌入 x64 + x86 agent DLL）
cargo build --release
# 产物：target\release\serial_tool.exe
```

debug 构建同时生成可执行的控制台程序，用于 CLI 诊断：

```powershell
cargo build
# 产物：target\debug\serial_tool.exe
```

## 使用

### GUI

双击 `serial_tool.exe` 即可。窗口标题栏显示当前版本。

### CLI 诊断命令

debug 构建可执行，release 为窗口子系统无控制台：

```text
serial_tool scan                              扫描持有串口句柄的进程
serial_tool inject <pid> [毫秒]               注入并打印双向数据流
serial_tool loop   <COMn> [baud] [间隔ms] [持续ms]   周期收发（充当被注入目标）
serial_tool send   <COMn> [baud] <text...>
serial_tool recv   <COMn> [baud] [毫秒]
```

环境变量 `SERIALTOOL_FRAME=newline` 时 CLI 收发按换行符组帧（默认空闲间隔）。

## 项目结构

```
serial_tool/
├── src/                        主程序
│   ├── main.rs                 入口 + CLI 命令分发
│   ├── app.rs                  顶层状态、事件分发、监控聚合
│   ├── config.rs               配置持久化
│   ├── logger.rs               日志
│   ├── serial/
│   │   ├── port.rs             串口打开/读线程/组帧
│   │   └── preset.rs           发送预设
│   ├── injection/              注入式监控
│   │   ├── scan.rs             扫描串口进程
│   │   ├── inject.rs           注入逻辑（WOW64 跨位数）
│   │   ├── agent_pipe.rs       命名管道服务端
│   │   └── frame.rs            通信协议帧
│   └── ui/                     egui 界面
│       ├── tab_sendrecv.rs    收发页
│       ├── tab_monitor.rs     监控页
│       ├── tab_log.rs          日志页
│       ├── panel_config.rs    左侧配置面板
│       └── theme.rs            主题与字体
├── crates/
│   ├── serial_agent/           注入用 agent DLL（x64 + x86，build.rs 嵌入 exe）
│   └── retour/                 本地修补的 retour 0.3.1（修 i686 ABI 宏）
├── assets/                     图标、版本号
├── build.rs                    嵌入 agent DLL 与 exe 资源
├── stress.ps1                  真实串口稳定性测试（release）
└── stress_inject.ps1           注入回归测试（debug）
```

## 测试

完整稳定性测试矩阵（59 项）：

```powershell
# 1) 单测（21 个：组帧/HEX/监控聚合/过滤/配置/协议帧/日志）
cargo test

# 2) 注入回归（14 个：x64 10 轮 + x86 WOW64 5 轮 + 异常参数 + 碎片拼接）
cargo build
powershell -NoProfile -ExecutionPolicy Bypass -File .\stress_inject.ps1

# 3) 真实串口压力（24 个：双组帧模式时序边界/二进制/长行/吞吐/快速开关/异常发送）
cargo build --release
powershell -NoProfile -ExecutionPolicy Bypass -File .\stress.ps1
```

需要 com0com 虚拟串口对 COM10 ↔ COM11。详见 [`.trae/skills/serial-stability-test/SKILL.md`](.trae/skills/serial-stability-test/SKILL.md)。

## 版本

版本号格式 `major.minor.patch`，存于 `assets/version.txt`。`git commit` 时由 pre-commit 钩子自动把 patch 位 +1。

| 标签 | 内容 |
|---|---|
| v1.0.3 | 配置路径可配置（便携模式） |
| v1.0.4 | 定时发送、UI 布局调整 |
| v1.0.5 | 修复取消注入导致被监控进程闪退 |
| v1.0.6 | 修复监控页一条数据偶发拆成两条 |
| v1.0.7 | 监控页兼容换行符组帧模式 |
| v1.0.8 | 目标退出探活 + 全量稳定性测试矩阵 |

## 许可

私有项目，未发布开源许可。
