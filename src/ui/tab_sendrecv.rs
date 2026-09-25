//! 收发页:RX/TX 分色显示 + 发送区
use egui::{RichText, ScrollArea};

use crate::app::SerialApp;
use crate::serial::preset::{DataFormat, SendPreset};
use crate::ui::theme::*;
use crate::ui::data_row;

pub fn ui(app: &mut SerialApp, ui: &mut egui::Ui) {
    let total_h = ui.available_height();

    // 高度预算:发送框固定约 136px(含外边距),其余全部给数据显示区
    const SEND_H: f32 = 136.0;
    const GAP: f32 = 6.0;
    const FRAME_PAD: f32 = 16.0; // 数据框上下内边距
    const FILTER_H: f32 = 28.0; // 过滤栏 + 间距
    let scroll_h = (total_h - SEND_H - GAP - FRAME_PAD - FILTER_H).max(80.0);

    // ---- 数据显示区 ----
    egui::Frame::none()
        .fill(BG)
        .stroke(egui::Stroke::new(1_f32, BORDER))
        .rounding(8_f32)
        .inner_margin(egui::Margin::same(8_f32))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // 实时过滤栏
            let filtering = !app.recv_filter.trim().is_empty();
            ui.horizontal(|ui| {
                ui.label(RichText::new("🔍").color(TEXT_DIM));
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut app.recv_filter)
                        .desired_width(220.0)
                        .hint_text("过滤(端口/RX/TX/HEX/文本,空格分隔)"),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    app.recv_filter.clear();
                }
                if filtering && ui.small_button("✕").on_hover_text("清除过滤").clicked() {
                    app.recv_filter.clear();
                }
                if filtering {
                    let shown = app.lines.iter().filter(|l| l.matches_filter(&app.recv_filter)).count();
                    ui.label(
                        RichText::new(format!("{shown}/{}", app.lines.len()))
                            .color(TEXT_DIM)
                            .small()
                            .monospace(),
                    );
                }
            });
            ui.add_space(2.0);

            ScrollArea::vertical()
                .max_height(scroll_h)
                .auto_shrink([false, false])
                .stick_to_bottom(app.auto_scroll && !filtering)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if app.lines.is_empty() {
                        ui.label(
                            RichText::new("暂无数据 —— 打开串口后开始收发")
                                .color(TEXT_DIM),
                        );
                    }
                    let mut any = false;
                    for line in &app.lines {
                        if !line.matches_filter(&app.recv_filter) {
                            continue;
                        }
                        any = true;
                        data_row(ui, line, app.display_format);
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

    ui.add_space(6.0);

    // ---- 发送区 ----
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1_f32, BORDER))
        .rounding(8_f32)
        .inner_margin(egui::Margin::same(8_f32))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.send_format, DataFormat::Ascii, "ASCII");
                ui.selectable_value(&mut app.send_format, DataFormat::Hex, "HEX");
                ui.separator();
                ui.checkbox(&mut app.append_crlf, "追加 CRLF");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let connected = app.serial_handle.is_some();
                    if ui
                        .add_enabled(connected, egui::Button::new("发送  ⏎").fill(ACCENT).min_size(egui::vec2(96.0, 24.0)))
                        .on_disabled_hover_text("请先在左侧打开串口")
                        .clicked()
                    {
                        do_send(app);
                    }
                });
            });
            ui.add_space(2.0);
            let ctrl_enter = ui.ctx().input(|i| {
                i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl && app.tab == crate::app::Tab::SendRecv
            });
            ui.add(
                egui::TextEdit::multiline(&mut app.send_input)
                    .desired_rows(3)
                    .desired_width(ui.available_width())
                    .code_editor(),
            );
            ui.label(
                RichText::new("Ctrl + Enter 发送 · 左侧预设支持一键发送与定时重发")
                    .color(TEXT_DIM)
                    .small(),
            );
            if ctrl_enter {
                do_send(app);
            }
        });
}

fn do_send(app: &mut SerialApp) {
    let tmp = SendPreset {
        id: 0,
        name: String::new(),
        content: app.send_input.clone(),
        format: app.send_format,
        append_crlf: app.append_crlf,
        repeat_interval_ms: None,
        enabled: false,
        expanded: false,
    };
    match tmp.decode() {
        Ok(data) => {
            if let Err(e) = app.send_bytes(&data) {
                app.set_error(format!("发送失败: {e}"));
            }
        }
        Err(e) => app.set_error(format!("数据格式错误: {e}")),
    }
}
