//! 青萝石墨黑灰科技主题色板 v3（Graphite）。
//!
//! 配色设计理念：
//! - 背景：中性炭黑灰层级（#08090B → #16161A），无彩色偏向，冷静硬朗
//! - 主色调：深青（#0E7490 → #0891B2），按钮/选中态，白字可读
//! - 强调色：电光青（#22D3EE），用于状态、徽标、发光线条，营造科技感
//! - 卡片：深灰实色 + 极细中性边框
//! - 文字：三层中性灰度（强/中/弱）
//!
//! 圆角/尺寸更柔和，主按钮 10px，卡片 12px。

use iced::{Background, Border, Color, Theme};
use iced::theme::Palette;
use iced_aw::style::{self as aw_style, Status as AwStatus};

// ===== 背景色系（中性炭黑灰） =====
/// 窗口最底层背景（近黑炭灰）
pub fn window_bg() -> Color { Color::from_rgb8(11, 11, 14) }           // #0B0B0E
/// 标题栏 / 活动栏背景
pub fn title_bar_bg() -> Color { Color::from_rgb8(17, 17, 20) }       // #111114
pub fn activity_bar_bg() -> Color { Color::from_rgb8(17, 17, 20) }    // #111114
/// 侧边栏 / 面板背景（稍亮一层）
pub fn sidebar_bg() -> Color { Color::from_rgb8(22, 22, 26) }         // #16161A
pub fn panel_bg() -> Color { Color::from_rgb8(18, 19, 22) }           // #121316
/// 画布背景（最深，突出节点）
pub fn canvas_bg() -> Color { Color::from_rgb8(8, 9, 11) }            // #08090B
/// 画布网格线（极弱对比）
pub fn canvas_grid() -> Color { Color::from_rgb8(29, 31, 36) }        // #1D1F24

// ===== 卡片 / 边框色系 =====
/// 卡片背景（深炭灰）
pub fn card_bg() -> Color { Color::from_rgb8(27, 29, 34) }            // #1B1D22
/// 卡片边框（极细中性灰）
pub fn card_stroke() -> Color { Color::from_rgb8(46, 49, 56) }        // #2E3138
/// 卡片悬浮时背景
pub fn card_hover_bg() -> Color { Color::from_rgb8(36, 39, 46) }      // #24272E

// ===== 状态栏 =====
pub fn status_bar_bg() -> Color { Color::from_rgb8(16, 17, 20) }      // #101114
pub fn status_bar_hover() -> Color {
    Color { r: 1.0, g: 1.0, b: 1.0, a: 40.0 / 255.0 }
}

// ===== 主色调系（深青科技色） =====
/// 主色：深青（按钮、链接、选中态；白字对比度充足）
pub fn accent() -> Color { Color::from_rgb8(14, 116, 144) }           // #0E7490 (Cyan-700)
/// 主色亮版（hover）
pub fn accent_bright() -> Color { Color::from_rgb8(8, 145, 178) }     // #0891B2 (Cyan-600)
/// 主色暗版（按下）
pub fn accent_dark() -> Color { Color::from_rgb8(21, 94, 117) }       // #155E75 (Cyan-800)
/// 主色弱化半透明（底色/边框用）
pub fn accent_dim() -> Color {
    Color { r: 8.0 / 255.0, g: 145.0 / 255.0, b: 178.0 / 255.0, a: 120.0 / 255.0 }
}
/// 次色：电光青（状态、徽章、装饰、发光线条）
pub fn accent_teal() -> Color { Color::from_rgb8(34, 211, 238) }      // #22D3EE (Cyan-400)
/// 渐变终点：天蓝
pub fn accent_blue() -> Color { Color::from_rgb8(56, 189, 248) }      // #38BDF8 (Sky-400)

// ===== 交互态 =====
pub fn hover_bg() -> Color { Color::from_rgb8(35, 38, 44) }           // #23262C
pub fn pressed_bg() -> Color { Color::from_rgb8(43, 47, 55) }         // #2B2F37
pub fn divider() -> Color { Color::from_rgb8(35, 38, 44) }            // #23262C

// ===== 圆角系统 =====
pub const WIDGET_ROUNDING: f32 = 8.0;    // 小控件（按钮、输入框）
pub const CARD_ROUNDING: f32 = 12.0;     // 卡片、面板
pub const FLOAT_ROUNDING: f32 = 16.0;    // 浮层、对话框
pub const PILL_ROUNDING: f32 = 999.0;    // 胶囊按钮

// ===== 文字色系（中性灰白） =====
pub fn text_strong() -> Color { Color::from_rgb8(233, 235, 239) }      // 主文字（近白）
pub fn text_hover() -> Color { Color::from_rgb8(211, 214, 220) }      // hover 文字
pub fn text_weak() -> Color { Color::from_rgb8(140, 144, 155) }       // 次要文字

// ===== 语义色 =====
pub fn success() -> Color { Color::from_rgb8(52, 211, 153) }           // #34D399 翠绿（Emerald-400）
pub fn warning() -> Color { Color::from_rgb8(251, 191, 36) }           // #FBBF24 琥珀（Amber-400）
pub fn danger()  -> Color { Color::from_rgb8(248, 113, 113) }          // #F87171 珊瑚红（Red-400）
pub fn info()    -> Color { accent_teal() }

/// 构造应用级自定义暗色主题。
pub fn dark_theme() -> Theme {
    Theme::custom(
        "Qingluo Graphite",
        Palette {
            background: panel_bg(),
            text: text_strong(),
            primary: accent(),
            success: success(),
            warning: warning(),
            danger: danger(),
        },
    )
}

// ===== 兼容旧常量（供外部引用） =====
pub const TITLE_BAR_BG: Color = Color { r: 17.0/255.0, g: 17.0/255.0, b: 20.0/255.0, a: 1.0 };
pub const ACTIVITY_BAR_BG: Color = Color { r: 17.0/255.0, g: 17.0/255.0, b: 20.0/255.0, a: 1.0 };
pub const SIDEBAR_BG: Color = Color { r: 22.0/255.0, g: 22.0/255.0, b: 26.0/255.0, a: 1.0 };
pub const PANEL_BG: Color = Color { r: 18.0/255.0, g: 19.0/255.0, b: 22.0/255.0, a: 1.0 };
pub const CANVAS_BG: Color = Color { r: 8.0/255.0, g: 9.0/255.0, b: 11.0/255.0, a: 1.0 };
pub const CANVAS_GRID: Color = Color { r: 29.0/255.0, g: 31.0/255.0, b: 36.0/255.0, a: 1.0 };
pub const CARD_BG: Color = Color { r: 27.0/255.0, g: 29.0/255.0, b: 34.0/255.0, a: 1.0 };
pub const CARD_STROKE: Color = Color { r: 46.0/255.0, g: 49.0/255.0, b: 56.0/255.0, a: 1.0 };
pub const STATUS_BAR_BG: Color = Color { r: 16.0/255.0, g: 17.0/255.0, b: 20.0/255.0, a: 1.0 };
pub const ACCENT: Color = Color { r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 1.0 };
pub const HOVER_BG: Color = Color { r: 35.0/255.0, g: 38.0/255.0, b: 44.0/255.0, a: 1.0 };
pub const DIVIDER: Color = Color { r: 35.0/255.0, g: 38.0/255.0, b: 44.0/255.0, a: 1.0 };
pub const TEXT_STRONG: Color = Color { r: 233.0/255.0, g: 235.0/255.0, b: 239.0/255.0, a: 1.0 };
pub const TEXT_HOVER: Color = Color { r: 211.0/255.0, g: 214.0/255.0, b: 220.0/255.0, a: 1.0 };
pub const TEXT_WEAK: Color = Color { r: 140.0/255.0, g: 144.0/255.0, b: 155.0/255.0, a: 1.0 };

// ===== iced_aw 组件样式接入层 =====
//
// iced_aw 0.14 采用 `Catalog` 模式：`iced::Theme` 已默认实现各组件的 Catalog，
// 调用 `.style(f)` 传入 `impl Fn(&Theme, Status) -> Style + 'a` 即可。
// 下面提供一组返回 'static 闭包的工厂函数，复用现有蓝紫调色板，让 iced_aw
// 组件外观与应用整体风格保持一致。

/// 主区域 TabBar（顶部 Tab 行）样式：
/// - 整体背景：panel_bg
/// - 选中 tab：靛蓝半透明 + accent_bright 边框
/// - hover tab：hover_bg
/// - 非 tab：透明
pub fn top_tab_bar_style(
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::tab_bar::Style + 'static {
    use aw_style::tab_bar::Style as TabBarStyle;
    move |_t, status| {
        let (label_bg, label_border, text) = match status {
            AwStatus::Selected => (
                Background::Color(Color {
                    r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 95.0/255.0,
                }),
                accent_bright(),
                Color::WHITE,
            ),
            AwStatus::Hovered => (
                Background::Color(hover_bg()),
                card_stroke(),
                text_hover(),
            ),
            AwStatus::Active | AwStatus::Focused => (
                Background::Color(Color::TRANSPARENT),
                card_stroke(),
                text_hover(),
            ),
            AwStatus::Pressed => (
                Background::Color(pressed_bg()),
                card_stroke(),
                text_strong(),
            ),
            AwStatus::Disabled => (
                Background::Color(Color::TRANSPARENT),
                card_stroke(),
                text_weak(),
            ),
        };
        TabBarStyle {
            background: Some(Background::Color(panel_bg())),
            border_color: Some(divider()),
            border_width: 0.0,
            tab_border_radius: WIDGET_ROUNDING.into(),
            tab_label_background: label_bg,
            tab_label_border_color: label_border,
            tab_label_border_width: 1.0,
            icon_color: text_weak(),
            icon_background: None,
            icon_border_radius: 6.0.into(),
            text_color: text,
        }
    }
}

/// 左侧合并面板顶部 TabBar 样式：激活态实色 accent 填充，与未激活完全区分。
pub fn left_panel_tab_bar_style(
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::tab_bar::Style + 'static {
    use aw_style::tab_bar::Style as TabBarStyle;
    move |_t, status| {
        let (label_bg, label_border, border_width, text, radius) = match status {
            AwStatus::Selected => (
                Background::Color(accent()),
                accent_bright(),
                1.0,
                Color::WHITE,
                6.0,
            ),
            AwStatus::Hovered => (
                Background::Color(hover_bg()),
                Color::TRANSPARENT,
                0.0,
                text_hover(),
                6.0,
            ),
            _ => (
                Background::Color(Color::TRANSPARENT),
                Color::TRANSPARENT,
                0.0,
                text_weak(),
                6.0,
            ),
        };
        TabBarStyle {
            background: Some(Background::Color(sidebar_bg())),
            border_color: Some(divider()),
            border_width: 0.0,
            tab_border_radius: radius.into(),
            tab_label_background: label_bg,
            tab_label_border_color: label_border,
            tab_label_border_width: border_width,
            icon_color: text_weak(),
            icon_background: None,
            icon_border_radius: radius.into(),
            text_color: text,
        }
    }
}

/// 日志面板分类胶囊 TabBar 样式：选中态 accent 填充，hover 弱底色。
pub fn log_tab_bar_style(
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::tab_bar::Style + 'static {
    use aw_style::tab_bar::Style as TabBarStyle;
    move |_t, status| {
        let (label_bg, label_border, text) = match status {
            AwStatus::Selected => (
                Background::Color(accent()),
                accent_bright(),
                Color::WHITE,
            ),
            AwStatus::Hovered => (
                Background::Color(hover_bg()),
                Color::TRANSPARENT,
                text_hover(),
            ),
            _ => (
                Background::Color(Color::TRANSPARENT),
                Color::TRANSPARENT,
                text_weak(),
            ),
        };
        TabBarStyle {
            background: Some(Background::Color(panel_bg())),
            border_color: None,
            border_width: 0.0,
            tab_border_radius: 8.0.into(),
            tab_label_background: label_bg,
            tab_label_border_color: label_border,
            tab_label_border_width: if matches!(status, AwStatus::Selected) { 1.0 } else { 0.0 },
            icon_color: text_weak(),
            icon_background: None,
            icon_border_radius: 8.0.into(),
            text_color: text,
        }
    }
}

/// 对话框 / 浮层 Card 样式：panel_bg 底 + 微光边框 + 大圆角。
pub fn float_card_style(
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::card::Style + 'static {
    use aw_style::card::Style as CardStyle;
    move |_t, _status| {
        CardStyle {
            background: Background::Color(panel_bg()),
            border_radius: FLOAT_ROUNDING,
            border_width: 1.0,
            border_color: Color { r: 1.0, g: 1.0, b: 1.0, a: 25.0/255.0 },
            head_background: Background::Color(Color::TRANSPARENT),
            head_text_color: text_strong(),
            body_background: Background::Color(Color::TRANSPARENT),
            body_text_color: text_strong(),
            foot_background: Background::Color(Color::TRANSPARENT),
            foot_text_color: text_hover(),
            close_color: text_weak(),
        }
    }
}

/// 列表项 / 算子卡片 Card 样式：card_bg 底 + card_stroke 边框 + 中圆角。
pub fn list_card_style(
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::card::Style + 'static {
    use aw_style::card::Style as CardStyle;
    move |_t, status| {
        let (bg, border_c) = match status {
            AwStatus::Hovered => (card_hover_bg(), accent_dim()),
            AwStatus::Selected => (
                Color { r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 95.0/255.0 },
                accent_bright(),
            ),
            _ => (card_bg(), card_stroke()),
        };
        CardStyle {
            background: Background::Color(bg),
            border_radius: CARD_ROUNDING,
            border_width: 1.0,
            border_color: border_c,
            head_background: Background::Color(Color::TRANSPARENT),
            head_text_color: text_strong(),
            body_background: Background::Color(Color::TRANSPARENT),
            body_text_color: text_strong(),
            foot_background: Background::Color(Color::TRANSPARENT),
            foot_text_color: text_weak(),
            close_color: text_weak(),
        }
    }
}

/// 标题栏装饰 Badge 样式：accent_teal 弱化底 + 同色细边框 + 同色文字。
/// 用于标题栏 "Quant IDE" 等装饰徽章。
pub fn title_badge_style(
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::badge::Style + 'static {
    use aw_style::badge::Style as BadgeStyle;
    move |_t, _status| BadgeStyle {
        background: Background::Color(Color {
            r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 15.0/255.0,
        }),
        border_radius: Some(PILL_ROUNDING),
        border_width: 1.0,
        border_color: Some(Color {
            r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 40.0/255.0,
        }),
        text_color: accent_teal(),
    }
}

/// 列表项计数 Badge 样式：accent 弱化底 + 白色文字，用于建模列表数量徽章。
pub fn count_badge_style(
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::badge::Style + 'static {
    use aw_style::badge::Style as BadgeStyle;
    move |_t, _status| BadgeStyle {
        background: Background::Color(Color {
            r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 35.0/255.0,
        }),
        border_radius: Some(PILL_ROUNDING),
        border_width: 0.0,
        border_color: None,
        text_color: Color::WHITE,
    }
}

/// 状态栏胶囊 Badge 样式：根据传入语义色生成弱化底 + 同色边框 + 同色文字。
/// 用于状态栏左侧状态点 + 右侧 "执行中/未保存/视图名" 等胶囊。
pub fn status_pill_style(
    accent_color: Color,
) -> impl Fn(&iced::Theme, AwStatus) -> aw_style::badge::Style + 'static {
    use aw_style::badge::Style as BadgeStyle;
    move |_t, _status| BadgeStyle {
        background: Background::Color(Color {
            r: accent_color.r, g: accent_color.g, b: accent_color.b, a: 12.0/255.0,
        }),
        border_radius: Some(PILL_ROUNDING),
        border_width: 1.0,
        border_color: Some(Color {
            r: accent_color.r, g: accent_color.g, b: accent_color.b, a: 35.0/255.0,
        }),
        text_color: accent_color,
    }
}

/// LabeledFrame 栏目样式：仅控制边框颜色 + 圆角，标题内容自带。
/// （iced_aw::widget::labeled_frame::Style 只有 color + radius 两个字段）
pub fn labeled_frame_style(
) -> impl Fn(&iced::Theme, AwStatus) -> iced_aw::widget::labeled_frame::Style + 'static {
    move |_t, _status| iced_aw::widget::labeled_frame::Style {
        color: Background::Color(card_stroke()),
        radius: CARD_ROUNDING.into(),
    }
}

/// Border 便捷构造器：给容器加 1px card_stroke 边框。
pub fn card_border() -> Border {
    Border {
        color: card_stroke(),
        width: 1.0,
        radius: CARD_ROUNDING.into(),
    }
}

/// 主按钮 border（accent 边框）。
pub fn accent_border() -> Border {
    Border {
        color: accent_bright(),
        width: 1.0,
        radius: WIDGET_ROUNDING.into(),
    }
}
