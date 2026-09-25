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
                    app.status = format!("日志目录:{}", app.log_cfg.dir);
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
                app.status = "日志配置已更新".into();
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
        });
}
