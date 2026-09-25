//! 科技风浅色主题配色与样式覆写(主配色为白色)
use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

/// 运行时加载 Windows 系统字体(不内嵌,不增加 exe 体积):
/// - 界面整体以微软雅黑为首选,中英文同一字体,混排字重/基线统一、笔画饱满;
/// - HEX/数据等宽区以 Consolas 为首选(数字字母严格对齐),中文回退雅黑;
/// - egui 内置字体保留在回退链末端,兜底罕见字符。
pub fn setup_fonts(ctx: &egui::Context) {
    const CJK_FONTS: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    const MONO_FONTS: &[&str] = &[r"C:\Windows\Fonts\consola.ttf"];

    let mut fonts = egui::FontDefinitions::default();

    if let Some(data) = CJK_FONTS.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts
            .font_data
            .insert("ui_cjk".to_owned(), egui::FontData::from_owned(data));
        if let Some(f) = fonts.families.get_mut(&FontFamily::Proportional) {
            f.insert(0, "ui_cjk".to_owned());
        }
        if let Some(f) = fonts.families.get_mut(&FontFamily::Monospace) {
            f.push("ui_cjk".to_owned());
        }
    }
    if let Some(data) = MONO_FONTS.iter().find_map(|p| std::fs::read(p).ok()) {
        fonts
            .font_data
            .insert("ui_mono".to_owned(), egui::FontData::from_owned(data));
        if let Some(f) = fonts.families.get_mut(&FontFamily::Monospace) {
            f.insert(0, "ui_mono".to_owned());
        }
    }

    ctx.set_fonts(fonts);
}

// ---- 浅色科技风调色板 ----
pub const BG: Color32 = Color32::from_rgb(0xF4, 0xF8, 0xFD);
pub const PANEL: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const WIDGET: Color32 = Color32::from_rgb(0xEE, 0xF4, 0xFB);
pub const WIDGET_HOVER: Color32 = Color32::from_rgb(0xDF, 0xEB, 0xFA);
pub const WIDGET_ACTIVE: Color32 = Color32::from_rgb(0xCF, 0xE0, 0xF8);
pub const BORDER: Color32 = Color32::from_rgb(0xD6, 0xE2, 0xF2);
pub const TEXT: Color32 = Color32::from_rgb(0x16, 0x26, 0x3E);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x5F, 0x72, 0x90);
pub const ACCENT: Color32 = Color32::from_rgb(0x2B, 0x7F, 0xFF);
/// 强调色按钮的悬停/按下(主按钮无浅色 hover)
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x1F, 0x6B, 0xE0);
pub const ACCENT_ACTIVE: Color32 = Color32::from_rgb(0x1A, 0x59, 0xBF);
pub const RX_C: Color32 = Color32::from_rgb(0x00, 0xA3, 0x7E);
pub const TX_C: Color32 = Color32::from_rgb(0xD9, 0x74, 0x0A);
pub const ERR_C: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);
pub const WARN_BG: Color32 = Color32::from_rgb(0xFF, 0xF6, 0xE6);

/// 端口稳定配色(哈希到深色科技调色板,浅底上保证可读)
const PORT_PALETTE: [(u8, u8, u8); 8] = [
    (0x00, 0x86, 0x68), // 青绿
    (0x2B, 0x7F, 0xFF), // 强调蓝
    (0x7A, 0x4D, 0xE0), // 紫
    (0xC2, 0x6A, 0x00), // 琥珀
    (0x08, 0x8E, 0xB8), // 青蓝
    (0xD0, 0x2E, 0x7A), // 品红
    (0x4E, 0x8D, 0x1A), // 草绿
    (0xB8, 0x42, 0x0E), // 橙棕
];

pub fn port_color(port: &str) -> Color32 {
    let mut h: u64 = 14_695_981_039_346_656_037;
    for b in port.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1_099_511_628_211);
    }
    let (r, g, b) = PORT_PALETTE[(h as usize) % PORT_PALETTE.len()];
    Color32::from_rgb(r, g, b)
}

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    // 从 egui 官方 light 视觉基线出发(确保 weak_bg_fill 等所有字段均为浅色),
    // 再逐项覆盖为科技风配色 —— 不能在 dark 基线上只改部分颜色。
    let v = &mut style.visuals;
    *v = egui::Visuals::light();
    v.dark_mode = false;
    v.panel_fill = PANEL;
    v.window_fill = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    v.extreme_bg_color = BG;
    v.faint_bg_color = Color32::from_rgb(0xF0, 0xF5, 0xFC);
    v.code_bg_color = Color32::from_rgb(0xF7, 0xFA, 0xFE);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = Color32::from_rgb(0xCF, 0xE2, 0xFF);
    v.selection.stroke = Stroke::new(1_f32, ACCENT);
    v.window_stroke = Stroke::new(1_f32, BORDER);
    v.window_rounding = Rounding::same(8_f32);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1_f32, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1_f32, TEXT_DIM);
    w.noninteractive.rounding = Rounding::same(6_f32);
    w.inactive.bg_fill = WIDGET;
    w.inactive.bg_stroke = Stroke::new(1_f32, BORDER);
    w.inactive.fg_stroke = Stroke::new(1_f32, TEXT);
    w.inactive.rounding = Rounding::same(6_f32);
    w.hovered.bg_fill = WIDGET_HOVER;
    w.hovered.bg_stroke = Stroke::new(1_f32, ACCENT);
    w.hovered.fg_stroke = Stroke::new(1_f32, TEXT);
    w.hovered.rounding = Rounding::same(6_f32);
    w.active.bg_fill = WIDGET_ACTIVE;
    w.active.bg_stroke = Stroke::new(1_f32, ACCENT);
    w.active.fg_stroke = Stroke::new(1_f32, TEXT);
    w.active.rounding = Rounding::same(6_f32);
    w.open.bg_fill = WIDGET_ACTIVE;
    w.open.bg_stroke = Stroke::new(1_f32, ACCENT);
    w.open.fg_stroke = Stroke::new(1_f32, TEXT);
    w.open.rounding = Rounding::same(6_f32);

    // 统一字号体系(参考主流工具软件:正文 13px,紧凑清晰)
    let ts = &mut style.text_styles;
    ts.insert(TextStyle::Heading, FontId::new(17.0, FontFamily::Proportional));
    ts.insert(TextStyle::Body, FontId::new(13.0, FontFamily::Proportional));
    ts.insert(TextStyle::Button, FontId::new(13.0, FontFamily::Proportional));
    ts.insert(TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace));
    ts.insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));

    style.spacing.item_spacing = egui::vec2(8.0, 5.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(10.0);
    // 控件最小高度与 13px 字体匹配,避免按钮/输入框文字贴边
    style.spacing.interact_size.y = 24.0;

    ctx.set_style(style);
}
