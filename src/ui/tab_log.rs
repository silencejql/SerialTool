//! 日志页:日志开关、目录、归档策略
use egui::RichText;

use crate::app::SerialApp;
use crate::ui::theme::*;

pub fn ui(app: &mut SerialApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    egui::Frame::none()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1_f32, BORDER))
        .rounding(8_f32)
        .inner_margin(egui::Margin::same(12_f32))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading(RichText::new("通讯日志").color(ACCENT));
            ui.add_space(6.0);

            let mut changed = false;
            if ui.checkbox(&mut app.log_cfg.enabled, "启用通讯日志").changed() {
                changed = true;
            }

            ui.add_space(4.0);
            ui.label("日志目录");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut app.log_cfg.dir)
                        .desired_width(460.0)
                        .code_editor(),
                );
                if ui.button("打开目录").clicked() {
                    let _ = std::process::Command::new("explorer")
                        .arg(&app.log_cfg.dir)
                        .spawn();
                }
                if ui.button("选择并创建").clicked() {
                    let _ = std::fs::create_dir_all(&app.log_cfg.dir);
                    app.set_status(format!("日志目录:{}", app.log_cfg.dir));
                }
            });

            ui.add_space(4.0);
            if ui
                .checkbox(&mut app.log_cfg.split_by_port, "按串口分文件(serial_COM3_*.log)")
                .changed()
            {
                changed = true;
            }
            if ui
                .checkbox(&mut app.log_cfg.split_by_hour, "按小时滚动(否则按天)")
                .changed()
            {
                changed = true;
            }

            if changed {
                app.logger.update_cfg(app.log_cfg.clone());
                app.set_status("日志配置已更新");
            }

            ui.add_space(10.0);
            ui.separator();
            ui.label(
                RichText::new(
                    "本机收发与注入监控数据统一实时记录,行格式:\n\
                     2026-09-23 12:00:01.123 [COM3] [target.exe] RX (16): AA 55 0F .. |text..|",
                )
                .color(TEXT_DIM)
                .monospace()
                .small(),
            );
            ui.add_space(8.0);
            if ui.button("立即保存当前收发记录").clicked() {
                app.export_current_log();
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(4.0);
            ui.heading(RichText::new("日志查询").color(ACCENT));
            ui.add_space(4.0);

            let files = app.list_log_files();
            ui.horizontal(|ui| {
                // 预留:文件下拉 160 + 查询 ~52 + 清空结果 ~76 + 三处间距
                let reserved = 160.0 + 52.0 + 76.0 + 24.0;
                let field_w = (ui.available_width() - reserved).max(180.0);
                let resp = ui
                    .allocate_ui(egui::vec2(field_w, 26.0), |ui| {
                        crate::ui::search_field(
                            ui,
                            &mut app.log_query,
                            "搜索关键词,逗号分隔多个(空=全部),回车查询",
                        )
                    })
                    .inner;
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    app.search_logs();
                }
                egui::ComboBox::from_id_salt("log_query_file")
                    .selected_text(app.log_query_file.as_deref().unwrap_or("全部文件"))
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.log_query_file, None, "全部文件");
                        for f in &files {
                            ui.selectable_value(&mut app.log_query_file, Some(f.clone()), f);
                        }
                    });
                if ui.button("查询").on_hover_text("也可在输入框按回车").clicked() {
                    app.search_logs();
                }
                if !app.log_results.is_empty() && ui.button("清空结果").clicked() {
                    app.log_results.clear();
                }
            });

            if !app.log_results.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("共 {} 行命中", app.log_results.len()))
                        .color(TEXT_DIM)
                        .small(),
                );
                egui::ScrollArea::vertical()
                    .id_salt("log_query_results")
                    .max_height(320.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let mut last_file = String::new();
                        for (file, line) in &app.log_results {
                            if *file != last_file {
                                last_file = file.clone();
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(format!("— {file} —"))
                                        .color(ACCENT)
                                        .small()
                                        .strong(),
                                );
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(line).monospace().small(),
                                )
                                .selectable(true)
                                .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        }
                    });
            }
        });
}
