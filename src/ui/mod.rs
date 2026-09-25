//! 顶层布局:顶栏 + 左侧配置面板 + 右侧标签页 + 底栏
pub mod panel_config;
pub mod tab_log;
pub mod tab_monitor;
pub mod tab_sendrecv;
pub mod theme;

use egui::RichText;

use crate::app::{SerialApp, Tab};
use crate::logger::{hex_string, visible_ascii};
use crate::serial::preset::DataFormat;
use theme::*;

pub fn format_data(data: &[u8], fmt: DataFormat) -> String {
    match fmt {
        DataFormat::Hex => hex_string(data),
        DataFormat::Ascii => visible_ascii(data),
    }
}

pub fn human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.2} MB", n as f64 / (1024.0 * 1024.0))
    }
}

pub fn build_ui(app: &mut SerialApp, ctx: &egui::Context) {
    // ---- 顶栏 ----
    egui::TopBottomPanel::top("top_bar")
        .exact_height(48.0)
        .frame(
            egui::Frame::side_top_panel(ctx.style().as_ref())
                .inner_margin(egui::Margin::symmetric(8.0, 6.0)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(4.0);
                ui.heading(RichText::new("◆ SerialTool").color(ACCENT).strong());
                ui.label(RichText::new("串口收发 / 监控").color(TEXT_DIM));
                ui.separator();
                for (tab, name) in [
                    (Tab::SendRecv, "收发"),
                    (Tab::Monitor, "监控"),
                    (Tab::Log, "日志"),
                ] {
                    if ui
                        .selectable_label(app.tab == tab, name)
                        .on_hover_text(match tab {
                            Tab::SendRecv => "串口收发与发送预设",
                            Tab::Monitor => "注入式双向监控,不占用串口",
                            Tab::Log => "通讯日志配置",
                        })
                        .clicked()
                    {
                        app.tab = tab;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (txt, c) = if app.serial_handle.is_some() {
                        (
                            format!("● {} 已连接", app.serial_cfg.port_name),
                            RX_C,
                        )
                    } else {
                        ("○ 未连接".to_string(), TEXT_DIM)
                    };
                    ui.label(RichText::new(txt).color(c).strong());
                });
            });
        });

    // ---- 底栏 ----
    egui::TopBottomPanel::bottom("status_bar")
        .exact_height(36.0)
        .frame(
            egui::Frame::side_top_panel(ctx.style().as_ref())
                .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(format!("RX: {}", human_bytes(app.rx_bytes))).color(RX_C));
                ui.label(RichText::new(format!("TX: {}", human_bytes(app.tx_bytes))).color(TX_C));
                ui.separator();
                ui.label(RichText::new("显示:").color(TEXT_DIM));
                if ui
                    .selectable_label(app.display_format == DataFormat::Hex, "HEX")
                    .clicked()
                {
                    app.display_format = DataFormat::Hex;
                }
                if ui
                    .selectable_label(app.display_format == DataFormat::Ascii, "文本")
                    .clicked()
                {
                    app.display_format = DataFormat::Ascii;
                }
                ui.checkbox(&mut app.auto_scroll, "自动滚动");
                if ui.button("清空显示").clicked() {
                    app.lines.clear();
                    app.monitor_lines.clear();
                }
                if ui.button("保存日志").clicked() {
                    app.export_current_log();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let color = if app.status_err { ERR_C } else { TEXT_DIM };
                    ui.label(RichText::new(&app.status).color(color));
                });
            });
        });

    // ---- 左侧配置面板 ----
    egui::SidePanel::left("left_panel")
        .default_width(292.0)
        .resizable(true)
        .width_range(240.0..=420.0)
        .show(ctx, |ui| {
            panel_config::ui(app, ui);
        });

    // ---- 中央标签页 ----
    egui::CentralPanel::default().show(ctx, |ui| match app.tab {
        Tab::SendRecv => tab_sendrecv::ui(app, ui),
        Tab::Monitor => tab_monitor::ui(app, ui),
        Tab::Log => tab_log::ui(app, ui),
    });

    if app.editor_open {
        preset_editor(app, ctx);
    }
}

fn preset_editor(app: &mut SerialApp, ctx: &egui::Context) {
    let mut keep_open = true;
    egui::Window::new("编辑发送预设")
        .open(&mut keep_open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.add_space(2.0);
            egui::Grid::new("preset_editor_grid")
                .num_columns(2)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    ui.label("名称");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.editing.name)
                            .desired_width(240.0),
                    );
                    ui.end_row();

                    ui.label("数据格式");
                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut app.editing.format,
                            DataFormat::Ascii,
                            "ASCII",
                        );
                        ui.radio_value(&mut app.editing.format, DataFormat::Hex, "HEX");
                    });
                    ui.end_row();

                    ui.label("追加 CRLF");
                    ui.checkbox(&mut app.editing.append_crlf, "发送时追加 \\r\\n");
                    ui.end_row();

                    ui.label("定时重发");
                    ui.horizontal(|ui| {
                        let mut repeat = app.editing.repeat_interval_ms.is_some();
                        if ui.checkbox(&mut repeat, "启用").changed() {
                            app.editing.repeat_interval_ms = if repeat { Some(1000) } else { None };
                            app.editing.enabled = repeat;
                        }
                        if let Some(ms) = app.editing.repeat_interval_ms.as_mut() {
                            ui.add(
                                egui::DragValue::new(ms)
                                    .speed(50.0)
                                    .range(10..=3_600_000)
                                    .suffix(" ms"),
                            );
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(4.0);
            ui.label("内容");
            ui.add(
                egui::TextEdit::multiline(&mut app.editing.content)
                    .desired_rows(5)
                    .desired_width(340.0)
                    .code_editor(),
            );
            if app.editing.format == DataFormat::Hex {
                ui.label(
                    RichText::new("HEX 示例:AA 55 0F(支持空格、逗号、0x 前缀)")
                        .color(TEXT_DIM)
                        .small(),
                );
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    match app.editing.decode() {
                        Ok(_) => {
                            let mut p = app.editing.clone();
                            if let Some(i) = app.editing_index {
                                p.id = app.presets[i].id;
                                app.presets[i] = p;
                            } else {
                                p.id = app.next_preset_id;
                                app.next_preset_id += 1;
                                app.presets.push(p);
                            }
                            app.editor_open = false;
                            app.sync_repeat_threads();
                        }
                        Err(e) => app.set_error(format!("预设数据格式错误: {e}")),
                    }
                }
                if ui.button("取消").clicked() {
                    app.editor_open = false;
                }
            });
        });
    if !keep_open {
        app.editor_open = false;
    }
}

/// 渲染一条带时间戳和方向颜色的数据行
pub fn data_row(ui: &mut egui::Ui, line: &crate::app::LogLine, fmt: DataFormat) {
    if !line.note.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("[{}]", line.ts.format("%H:%M:%S%.3f")))
                    .color(TEXT_DIM)
                    .monospace(),
            );
            ui.label(RichText::new(&line.note).color(TX_C).monospace());
        });
        return;
    }
    let (dir, color) = match line.dir {
        crate::app::Dir::Rx => ("RX", RX_C),
        crate::app::Dir::Tx => ("TX", TX_C),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("[{}]", line.ts.format("%H:%M:%S%.3f")))
                .color(TEXT_DIM)
                .monospace(),
        );
        ui.label(RichText::new(dir).color(color).strong().monospace());
        ui.label(RichText::new(format_data(&line.bytes, fmt)).color(color).monospace());
    });
}
