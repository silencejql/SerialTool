//! 左侧面板:串口参数配置(波特率可任意自定义) + 发送预设管理(勾选内联展开)
use egui::{RichText, ScrollArea, TextEdit};

use crate::app::SerialApp;
use crate::serial::preset::{DataFormat, SendPreset};
use crate::ui::theme::*;

/// 常用波特率速选(任意速率可直接在输入框填写)
const BAUD_RATES: [u32; 12] = [
    1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1_500_000,
];
const DATA_BITS: [u8; 4] = [5, 6, 7, 8];
const STOP_BITS: [(f32, &str); 2] = [(1.0, "1"), (2.0, "2")];
const PARITIES: [&str; 3] = ["None", "Odd", "Even"];
const FLOWS: [&str; 3] = ["None", "Software", "Hardware"];

pub fn ui(app: &mut SerialApp, ui: &mut egui::Ui) {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading(RichText::new("串口配置").color(ACCENT));
            ui.add_space(4.0);

            let connected = app.serial_handle.is_some();

            ui.add_enabled_ui(!connected, |ui| {
                egui::Grid::new("serial_cfg_grid")
                    .num_columns(2)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("端口");
                        ui.horizontal(|ui| {
                            let selected = if app.serial_cfg.port_name.is_empty() {
                                "请选择".to_string()
                            } else {
                                app.serial_cfg.port_name.clone()
                            };
                            egui::ComboBox::from_id_salt("port_combo")
                                .selected_text(selected)
                                .width(130.0)
                                .show_ui(ui, |ui| {
                                    for p in app.available_ports.clone() {
                                        if ui
                                            .selectable_label(
                                                app.serial_cfg.port_name == p,
                                                p.clone(),
                                            )
                                            .clicked()
                                        {
                                            app.serial_cfg.port_name = p;
                                        }
                                    }
                                });
                            if ui.small_button("⟳").on_hover_text("刷新端口列表").clicked() {
                                app.refresh_ports();
                            }
                        });
                        ui.end_row();

                        ui.label("波特率");
                        ui.horizontal(|ui| {
                            // 任意波特率:直接输入数字
                            let resp = ui.add(
                                TextEdit::singleline(&mut app.baud_input)
                                    .desired_width(78.0)
                                    .clip_text(true),
                            );
                            if resp.changed() {
                                app.sync_baud_input();
                            }
                            ui.label(RichText::new("bps").color(TEXT_DIM));
                            egui::ComboBox::from_id_salt("baud_combo")
                                .selected_text("速选")
                                .width(92.0)
                                .show_ui(ui, |ui| {
                                    for b in BAUD_RATES {
                                        if ui
                                            .selectable_label(
                                                app.serial_cfg.baud_rate == b,
                                                b.to_string(),
                                            )
                                            .clicked()
                                        {
                                            app.set_baud(b);
                                        }
                                    }
                                });
                        });
                        ui.end_row();

                        ui.label("数据位");
                        egui::ComboBox::from_id_salt("databits_combo")
                            .selected_text(app.serial_cfg.data_bits.to_string())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for d in DATA_BITS {
                                    if ui
                                        .selectable_label(app.serial_cfg.data_bits == d, d.to_string())
                                        .clicked()
                                    {
                                        app.serial_cfg.data_bits = d;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("停止位");
                        egui::ComboBox::from_id_salt("stopbits_combo")
                            .selected_text(stop_label(app.serial_cfg.stop_bits))
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for (v, name) in STOP_BITS {
                                    if ui
                                        .selectable_label(
                                            (app.serial_cfg.stop_bits - v).abs() < 0.1,
                                            name,
                                        )
                                        .clicked()
                                    {
                                        app.serial_cfg.stop_bits = v;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("校验位");
                        egui::ComboBox::from_id_salt("parity_combo")
                            .selected_text(app.serial_cfg.parity.clone())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for p in PARITIES {
                                    if ui
                                        .selectable_label(app.serial_cfg.parity == p, p)
                                        .clicked()
                                    {
                                        app.serial_cfg.parity = p.to_string();
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("流控");
                        egui::ComboBox::from_id_salt("flow_combo")
                            .selected_text(app.serial_cfg.flow_control.clone())
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                for f in FLOWS {
                                    if ui
                                        .selectable_label(app.serial_cfg.flow_control == f, f)
                                        .clicked()
                                    {
                                        app.serial_cfg.flow_control = f.to_string();
                                    }
                                }
                            });
                        ui.end_row();
                    });
            });
            ui.label(
                RichText::new("波特率支持任意自定义数值,如 125000、31250(MIDI)等")
                    .color(TEXT_DIM)
                    .small(),
            );

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!connected, egui::Button::new("打开串口").fill(ACCENT))
                    .clicked()
                {
                    app.open_serial();
                }
                if ui.add_enabled(connected, egui::Button::new("关闭串口")).clicked() {
                    app.close_serial();
                }
            });

            ui.separator();

            // ---- 发送预设 ----
            ui.horizontal(|ui| {
                ui.heading(RichText::new("发送预设").color(ACCENT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("＋ 新建").clicked() {
                        app.editing = SendPreset::new(app.next_preset_id);
                        app.editing_index = None;
                        app.editor_open = true;
                    }
                });
            });
            ui.label(
                RichText::new("勾选预设可在列表中直接展开并修改内容,取消勾选只显示名称")
                    .color(TEXT_DIM)
                    .small(),
            );
            ui.add_space(4.0);

            let mut resync = false;
            let mut removed: Option<usize> = None;
            let mut i = 0;
            while i < app.presets.len() {
                // 该卡片内产生的动作,闭包结束借用后再执行
                let mut send_now = false;
                let mut card_resync = false;

                {
                    let preset = &mut app.presets[i];
                    let name = preset.name.clone();
                    let expanded = preset.expanded;
                    let has_interval = preset.repeat_interval_ms.is_some();
                    let interval_text = preset
                        .repeat_interval_ms
                        .map(|ms| format!("{ms}ms"))
                        .unwrap_or_default();

                    egui::Frame::none()
                        .fill(WIDGET)
                        .stroke(egui::Stroke::new(1_f32, BORDER))
                        .rounding(6.0)
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());

                            // 顶行:展开勾选(即"是否直接显示内容") + 名称 + 操作
                            ui.horizontal(|ui| {
                                ui.checkbox(
                                    &mut preset.expanded,
                                    RichText::new(if name.is_empty() { "(未命名)" } else { &name })
                                        .color(if preset.enabled { RX_C } else { TEXT })
                                        .strong(),
                                )
                                .on_hover_text("勾选:内联显示并可直接修改内容;取消:只显示名称");
                                if has_interval {
                                    ui.label(
                                        RichText::new(format!("⏱{interval_text}"))
                                            .color(ACCENT)
                                            .small(),
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("删").on_hover_text("删除预设").clicked()
                                        {
                                            removed = Some(i);
                                        }
                                        if ui
                                            .small_button("发")
                                            .on_hover_text("立即发送一次")
                                            .clicked()
                                        {
                                            send_now = true;
                                        }
                                        // 收起态仍可用勾选启停定时重发
                                        if !expanded && has_interval {
                                            let mut e = preset.enabled;
                                            if ui
                                                .checkbox(&mut e, "定时")
                                                .on_hover_text(format!(
                                                    "启用/停止 {interval_text} 定时重发"
                                                ))
                                                .changed()
                                            {
                                                preset.enabled = e;
                                                card_resync = true;
                                            }
                                        }
                                    },
                                );
                            });

                            // 展开态:内联编辑
                            if preset.expanded {
                                ui.add_space(3.0);
                                ui.add(
                                    TextEdit::singleline(&mut preset.name)
                                        .desired_width(ui.available_width())
                                        .hint_text("预设名称"),
                                );
                                ui.add(
                                    TextEdit::multiline(&mut preset.content)
                                        .desired_rows(3)
                                        .desired_width(ui.available_width())
                                        .code_editor(),
                                );
                                ui.horizontal_wrapped(|ui| {
                                    ui.radio_value(
                                        &mut preset.format,
                                        DataFormat::Ascii,
                                        "ASCII",
                                    );
                                    ui.radio_value(&mut preset.format, DataFormat::Hex, "HEX");
                                    ui.checkbox(&mut preset.append_crlf, "追加 CRLF");
                                });
                                ui.horizontal_wrapped(|ui| {
                                    // 定时重发:勾选启用 + 周期编辑
                                    let mut repeat_on = preset.repeat_interval_ms.is_some();
                                    if ui.checkbox(&mut repeat_on, "定时重发").changed() {
                                        preset.repeat_interval_ms =
                                            if repeat_on { Some(1000) } else { None };
                                        preset.enabled = repeat_on;
                                        card_resync = true;
                                    }
                                    if let Some(ms) = preset.repeat_interval_ms.as_mut() {
                                        let before = *ms;
                                        ui.add(
                                            egui::DragValue::new(ms)
                                                .speed(50.0)
                                                .range(10..=3_600_000)
                                                .suffix(" ms"),
                                        );
                                        // 周期改变且已启用时重建线程
                                        if before != *ms && preset.enabled {
                                            card_resync = true;
                                        }
                                        let mut en = preset.enabled;
                                        if ui.checkbox(&mut en, "启用").changed() {
                                            preset.enabled = en;
                                            card_resync = true;
                                        }
                                    }
                                });
                                if preset.format == DataFormat::Hex {
                                    ui.label(
                                        RichText::new("HEX:AA 55 0F(支持空格、逗号、0x 前缀)")
                                            .color(TEXT_DIM)
                                            .small(),
                                    );
                                }
                            }
                        });
                }

                if send_now {
                    if let Ok(data) = app.presets[i].decode() {
                        if let Err(e) = app.send_bytes(&data) {
                            app.set_error(e);
                        }
                    } else {
                        app.set_error("预设数据格式错误");
                    }
                }
                if card_resync {
                    resync = true;
                }
                ui.add_space(4.0);
                i += 1;
            }
            if let Some(i) = removed {
                app.presets.remove(i);
                resync = true;
            }
            if resync {
                app.sync_repeat_threads();
            }
        });
}

fn stop_label(v: f32) -> String {
    if (v - 2.0).abs() < 0.1 {
        "2".into()
    } else {
        "1".into()
    }
}
