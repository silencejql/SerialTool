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

            // 实时过滤栏:胶囊搜索框填满左侧,右侧固定摆放清除按钮与计数
            let filtering = !app.recv_filter.trim().is_empty();
            ui.horizontal(|ui| {
                // × 按钮 22 + 计数 ~64 + 两个间距,提前预留,避免换行/溢出
                let reserved = if filtering { 22.0 + 64.0 + 16.0 } else { 0.0 };
                let field_w = (ui.available_width() - reserved).max(120.0);
                let resp = ui
                    .allocate_ui(egui::vec2(field_w, 26.0), |ui| {
                        crate::ui::search_field(
                            ui,
                            &mut app.recv_filter,
                            "端口/RX/TX/HEX/文本,空格分隔,多个条件任一命中",
                        )
                    })
                    .inner;
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    app.recv_filter.clear();
                }
                if filtering {
                    if ui.small_button("×").on_hover_text("清除过滤").clicked() {
                        app.recv_filter.clear();
                    }
                    let shown = app
                        .lines
                        .iter()
                        .filter(|l| l.matches_filter(&app.recv_filter))
                        .count();
                    ui.label(
                        RichText::new(format!("{shown}/{}", app.lines.len()))
                            .color(TEXT_DIM)
                            .small()
                            .monospace(),
                    );
                }
            });
            ui.add_space(2.0);

            // 悬浮工具条高度/底部留白:让最后一行可以滚到视口第一行,不被悬浮条遮挡
            const BAR_H: f32 = 28.0;
            const ROW_H: f32 = 16.0;
            let pad_bottom = (scroll_h - ROW_H).max(BAR_H + 8.0);

            ScrollArea::vertical()
                .max_height(scroll_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // 压缩实时日志行距
                    ui.spacing_mut().item_spacing.y = 1.0;
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
                    // 末行位置:贴底吸附时让它停在悬浮条上方而不是被遮住
                    let last_line = ui.min_rect().bottom() - if any { ROW_H } else { 0.0 };
                    ui.add_space(pad_bottom);
                    if app.auto_scroll && !filtering && any {
                        ui.scroll_to_rect(
                            egui::Rect::from_min_max(
                                egui::pos2(ui.min_rect().left(), last_line),
                                egui::pos2(ui.min_rect().right(), last_line + ROW_H + BAR_H + 6.0),
                            ),
                            Some(egui::Align::BOTTOM),
                        );
                    }
                });

            // ---- 悬浮工具条:覆盖在数据区底部,右缩 14px 避开滚动条 ----
            let outer = ui.min_rect();
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(outer.left() + 8.0, outer.bottom() - BAR_H - 6.0),
                egui::vec2((outer.width() - 16.0 - 14.0).max(120.0), BAR_H),
            );
            let mut bar_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(bar_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0xE8))
                .stroke(egui::Stroke::new(1_f32, BORDER))
                .rounding(14_f32)
                .inner_margin(egui::Margin::symmetric(10.0, 0.0))
                .show(&mut bar_ui, |ui| {
                    ui.set_height(BAR_H - 2.0);
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
                    ui.separator();
                    ui.checkbox(&mut app.auto_scroll, "自动滚动");
                    ui.separator();
                    if ui.button("清空显示").clicked() {
                        app.lines.clear();
                        app.monitor_lines.clear();
                    }
                    if ui.button("保存日志").clicked() {
                        app.export_current_log();
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
            // 提示行在上,输入框居中,格式行+发送按钮在下
            ui.label(
                RichText::new("Ctrl + Enter 发送 · 左侧预设支持一键发送与定时重发")
                    .color(TEXT_DIM)
                    .small(),
            );
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
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.send_format, DataFormat::Ascii, "ASCII");
                ui.selectable_value(&mut app.send_format, DataFormat::Hex, "HEX");
                ui.separator();
                ui.checkbox(&mut app.append_crlf, "追加 CRLF");
                // 有界的 RTL 区域承载发送按钮:宽度占满剩余空间,按钮在其中右对齐
                let connected = app.serial_handle.is_some();
                let row_w = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(row_w, 24.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let resp = crate::ui::send_button(ui, connected).on_hover_text(
                            if connected {
                                "发送(Ctrl+Enter)"
                            } else {
                                "请先在左侧打开串口"
                            },
                        );
                        if resp.clicked() {
                            do_send(app);
                        }
                    },
                );
            });
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
        // HEX 空白且未勾 CRLF 时解码为 0 字节:拦截,不入队、不记 TX
        Ok(data) if data.is_empty() => app.set_error("发送内容为空"),
        Ok(data) => {
            if let Err(e) = app.send_bytes(&data) {
                app.set_error(format!("发送失败: {e}"));
            }
        }
        Err(e) => app.set_error(format!("数据格式错误: {e}")),
    }
}
