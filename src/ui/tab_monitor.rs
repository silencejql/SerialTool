//! 监控页:扫描持有串口句柄的进程 → 注入 hook DLL → 双向数据流(不占用串口)
use egui::{RichText, ScrollArea};

use crate::app::{Dir, LogLine, SerialApp};
use crate::ui::format_data;
use crate::ui::theme::*;

pub fn ui(app: &mut SerialApp, ui: &mut egui::Ui) {
    // ---- 原理说明 ----
    egui::Frame::none()
        .fill(WARN_BG)
        .stroke(egui::Stroke::new(1_f32, BORDER))
        .rounding(8_f32)
        .inner_margin(egui::Margin::symmetric(10_f32, 8_f32))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("◆ 注入式监控:不占用串口,收发双向可见,彻底规避错误码 5")
                    .color(ACCENT)
                    .strong(),
            );
            ui.label(
                RichText::new(
                    "自动扫描已打开串口的进程,向其注入 hook 接管 CreateFile / ReadFile / WriteFile,\
                     注入前已打开的串口句柄也会自动接管;支持 64 位(x64)与 32 位(x86/WOW64)目标进程。",
                )
                .color(TEXT_DIM)
                .small(),
            );
        });

    ui.add_space(6.0);

    // ---- 扫描 / 控制 ----
    ui.horizontal(|ui| {
        let scan_label = if app.scanning { "扫描中…" } else { "⟳ 扫描串口进程" };
        if ui
            .add_enabled(!app.scanning, egui::Button::new(scan_label).fill(ACCENT))
            .on_hover_text("枚举当前已打开串口的进程")
            .clicked()
        {
            app.start_scan();
        }
        let any_online = app.targets.values().any(|t| t.online);
        if ui
            .add_enabled(any_online, egui::Button::new("■ 停止全部"))
            .on_hover_text("卸载所有目标进程中的 hook")
            .clicked()
        {
            app.detach_all();
        }
        let n_online = app.targets.values().filter(|t| t.online).count();
        ui.label(
            RichText::new(if n_online > 0 {
                format!("监控中:{n_online} 个进程")
            } else {
                "监控中:无".to_string()
            })
            .color(if n_online > 0 { RX_C } else { TEXT_DIM })
            .small(),
        );
    });

    // ---- 进程列表(必须限制 ScrollArea 高度,否则它会吞掉全部剩余空间) ----
    let list_h = (app.scan_results.len() as f32)
        .mul_add(30.0, 12.0)
        .clamp(54.0, 210.0);
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1_f32, BORDER))
        .rounding(8_f32)
        .inner_margin(egui::Margin::symmetric(8_f32, 6_f32))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ScrollArea::vertical()
                .id_salt("mon_proc_list")
                .max_height(list_h)
                .min_scrolled_height(48.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                if app.scanning && app.scan_results.is_empty() {
                    ui.label(RichText::new("正在扫描系统句柄,通常 1 秒内完成…").color(TEXT_DIM));
                } else if app.scan_results.is_empty() {
                    ui.label(
                        RichText::new("尚未发现打开串口的进程 —— 先点击「扫描串口进程」")
                            .color(TEXT_DIM),
                    );
                } else {
                    let mut i = 0;
                    while i < app.scan_results.len() {
                        process_row(app, ui, i);
                        i += 1;
                    }
                }
            });
        });

    ui.add_space(6.0);

    // ---- 双向数据流输出 ----
    let total_h = ui.available_height();
    egui::Frame::none()
        .fill(BG)
        .stroke(egui::Stroke::new(1_f32, BORDER))
        .rounding(8_f32)
        .inner_margin(egui::Margin::same(8_f32))
        .show(ui, |ui| {
            ui.set_min_height((total_h - 20.0).max(80.0));
            ui.set_width(ui.available_width());

            // 实时过滤栏
            let filtering = !app.monitor_filter.trim().is_empty();
            ui.horizontal(|ui| {
                ui.label(RichText::new("🔍").color(TEXT_DIM));
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut app.monitor_filter)
                        .desired_width(240.0)
                        .hint_text("过滤(COM端口/进程/RX/TX/HEX/文本,空格分隔)"),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    app.monitor_filter.clear();
                }
                if filtering && ui.small_button("✕").on_hover_text("清除过滤").clicked() {
                    app.monitor_filter.clear();
                }
                if filtering {
                    let shown = app
                        .monitor_lines
                        .iter()
                        .filter(|l| l.matches_filter(&app.monitor_filter))
                        .count();
                    ui.label(
                        RichText::new(format!("{shown}/{}", app.monitor_lines.len()))
                            .color(TEXT_DIM)
                            .small()
                            .monospace(),
                    );
                }
            });
            ui.add_space(2.0);

            ScrollArea::vertical()
                .id_salt("mon_data_flow")
                .auto_shrink([false, false])
                .stick_to_bottom(app.monitor_auto_scroll && !filtering)
                .show(ui, |ui| {
                    if app.monitor_lines.is_empty() {
                        ui.label(
                            RichText::new("扫描到目标进程后点击「注入」,这里显示其 RX / TX 双向数据")
                                .color(TEXT_DIM),
                        );
                    }
                    let mut any = false;
                    for line in &app.monitor_lines {
                        if !line.matches_filter(&app.monitor_filter) {
                            continue;
                        }
                        any = true;
                        monitor_row(ui, line, app.display_format);
                    }
                    if filtering && !any {
                        ui.label(
                            RichText::new("无匹配行")
                                .color(TEXT_DIM)
                                .italics(),
                        );
                    }
                });
        });
}

/// 单个进程行:`[COMn] 进程名.exe (pid) 位数  [注入/停止]`
fn process_row(app: &mut SerialApp, ui: &mut egui::Ui, i: usize) {
    let p = app.scan_results[i].clone();
    let target = app.targets.get(&p.pid);
    let online = target.map(|t| t.online).unwrap_or(false);
    let pending = target.is_some() && !online;

    ui.horizontal(|ui| {
        // 状态点
        let dot = if online { "●" } else if pending { "◐" } else { "○" };
        let dot_c = if online {
            RX_C
        } else if pending {
            TX_C
        } else {
            TEXT_DIM
        };
        ui.label(RichText::new(dot).color(dot_c));

        // 串口标记(每个端口一个彩色标签)
        for port in &p.ports {
            ui.label(
                RichText::new(format!("[{port}]"))
                    .color(port_color(port))
                    .strong()
                    .monospace(),
            );
        }

        ui.label(RichText::new(&p.name).color(TEXT).strong().monospace());
        ui.label(RichText::new(format!("pid {}", p.pid)).color(TEXT_DIM).monospace());

        // 位数标记(32 位用强调色提示,注入时会自动选用 32 位 agent)
        let arch_txt = if p.x64 { "x64" } else { "32位" };
        ui.label(
            RichText::new(arch_txt)
                .color(if p.x64 { TEXT_DIM } else { ACCENT })
                .small()
                .monospace(),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if online {
                if ui.button("停止").on_hover_text("卸载该进程中的 hook").clicked() {
                    app.detach_pid(p.pid);
                }
            } else if pending {
                ui.add_enabled(false, egui::Button::new("注入中…"));
            } else if ui
                .add(egui::Button::new("注入").fill(ACCENT))
                .on_hover_text(if p.x64 {
                    "向该 64 位进程注入监控 hook,不影响其串口通信"
                } else {
                    "向该 32 位(WOW64)进程注入 32 位监控 hook,不影响其串口通信"
                })
                .clicked()
            {
                app.inject_pid(p.pid, p.name.clone());
            }
        });
    });
}

fn monitor_row(ui: &mut egui::Ui, line: &LogLine, fmt: crate::serial::preset::DataFormat) {
    if !line.note.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("[{}]", line.ts.format("%H:%M:%S%.3f")))
                    .color(TEXT_DIM)
                    .monospace(),
            );
            if !line.process.is_empty() {
                ui.label(
                    RichText::new(format!("[{}]", line.process))
                        .color(ACCENT)
                        .strong()
                        .monospace(),
                );
            }
            ui.label(
                RichText::new(&line.note)
                    .color(if line.note.contains("失败") {
                        ERR_C
                    } else {
                        TX_C
                    })
                    .monospace(),
            );
        });
        return;
    }
    let color = port_color(&line.port);
    let dir = match line.dir {
        Dir::Rx => "RX",
        Dir::Tx => "TX",
    };
    let dir_color = match line.dir {
        Dir::Rx => RX_C,
        Dir::Tx => TX_C,
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("[{}]", line.ts.format("%H:%M:%S%.3f")))
                .color(TEXT_DIM)
                .monospace(),
        );
        ui.label(RichText::new(format!("[{}]", line.port)).color(color).strong().monospace());
        ui.label(
            RichText::new(format!("[{}]", line.process))
                .color(TEXT_DIM)
                .monospace(),
        );
        ui.label(RichText::new(dir).color(dir_color).strong().monospace());
        ui.label(RichText::new(format_data(&line.bytes, fmt)).color(TEXT).monospace());
    });
}
