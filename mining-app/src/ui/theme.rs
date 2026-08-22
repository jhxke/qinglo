//! 青萝现代暗色主题色板 v2。
//!
//! 配色设计理念：
//! - 背景：深蓝灰渐变系（#0B0F19 → #131826），替代纯黑更有层次感
//! - 主色调：蓝紫渐变（#6366F1 紫 → #3B82F6 蓝），更现代高级感
//! - 强调色：青绿（#22D3EE），用于状态、徽标
//! - 卡片：半透明玻璃拟态 + 极细边框
//! - 文字：三层灰度（强/中/弱）+ 统一蓝色调
//!
//! 圆角/尺寸更柔和，主按钮 10px，卡片 12px。

use iced::{Background, Border, Color, Theme};
use iced::theme::Palette;
use iced_aw::style::{self as aw_style, Status as AwStatus};

// ===== 背景色系 =====
/// 窗口最底层背景（深蓝灰，带轻微紫调）
pub fn window_bg() -> Color { Color::from_rgb8(11, 15, 25) }           // #0B0F19
/// 标题栏 / 活动栏背景
pub fn title_bar_bg() -> Color { Color::from_rgb8(13, 18, 32) }       // #0D1220
pub fn activity_bar_bg() -> Color { Color::from_rgb8(13, 18, 32) }    // #0D1220
/// 侧边栏 / 面板背景（稍亮一层）
pub fn sidebar_bg() -> Color { Color::from_rgb8(17, 22, 38) }         // #111626
pub fn panel_bg() -> Color { Color::from_rgb8(15, 20, 34) }           // #0F1422
/// 画布背景（最深，突出节点）
pub fn canvas_bg() -> Color { Color::from_rgb8(9, 12, 20) }           // #090C14
/// 画布网格线（极弱对比）
pub fn canvas_grid() -> Color { Color::from_rgb8(29, 35, 54) }        // #1D2336

// ===== 卡片 / 边框色系 =====
/// 卡片背景（半透明 + 蓝紫微光）
pub fn card_bg() -> Color { Color::from_rgb8(24, 30, 48) }            // #181E30
/// 卡片边框（极细微光）
pub fn card_stroke() -> Color { Color::from_rgb8(45, 55, 82) }        // #2D3752
/// 卡片悬浮时背景
pub fn card_hover_bg() -> Color { Color::from_rgb8(32, 40, 62) }      // #20283E

// ===== 状态栏 =====
pub fn status_bar_bg() -> Color { Color::from_rgb8(19, 25, 42) }      // #13192A
pub fn status_bar_hover() -> Color {
    Color { r: 1.0, g: 1.0, b: 1.0, a: 40.0 / 255.0 }
}

// ===== 主色调系（蓝紫渐变） =====
/// 主色：靛蓝紫（按钮、链接、选中态）
pub fn accent() -> Color { Color::from_rgb8(99, 102, 241) }           // #6366F1 (Indigo-500)
/// 主色亮版（hover）
pub fn accent_bright() -> Color { Color::from_rgb8(129, 140, 248) }   // #818CF8 (Indigo-400)
/// 主色暗版（按下）
pub fn accent_dark() -> Color { Color::from_rgb8(79, 70, 229) }       // #4F46E5 (Indigo-600)
/// 主色弱化半透明（底色/边框用）
pub fn accent_dim() -> Color {
    Color { r: 99.0 / 255.0, g: 102.0 / 255.0, b: 241.0 / 255.0, a: 120.0 / 255.0 }
}
/// 次色：青蓝色（状态、徽章、装饰）
pub fn accent_teal() -> Color { Color::from_rgb8(34, 211, 238) }      // #22D3EE (Cyan-400)
/// 渐变终点：蓝色
pub fn accent_blue() -> Color { Color::from_rgb8(59, 130, 246) }      // #3B82F6 (Blue-500)

// ===== 交互态 =====
pub fn hover_bg() -> Color { Color::from_rgb8(35, 42, 66) }           // #232A42
pub fn pressed_bg() -> Color { Color::from_rgb8(42, 50, 78) }         // #2A324E
pub fn divider() -> Color { Color::from_rgb8(35, 42, 66) }            // #232A42

// ===== 圆角系统 =====
pub const WIDGET_ROUNDING: f32 = 8.0;    // 小控件（按钮、输入框）
pub const CARD_ROUNDING: f32 = 12.0;     // 卡片、面板
pub const FLOAT_ROUNDING: f32 = 16.0;    // 浮层、对话框
pub const PILL_ROUNDING: f32 = 999.0;    // 胶囊按钮

// ===== 文字色系（统一蓝灰调，避免纯灰偏冷绿） =====
pub fn text_strong() -> Color { Color::from_rgb8(235, 238, 250) }      // 主文字（近白偏蓝）
pub fn text_hover() -> Color { Color::from_rgb8(215, 220, 240) }      // hover 文字
pub fn text_weak() -> Color { Color::from_rgb8(142, 151, 185) }       // 次要文字

// ===== 语义色 =====
pub fn success() -> Color { Color::from_rgb8(52, 211, 153) }           // #34D399 翠绿（Emerald-400）
pub fn warning() -> Color { Color::from_rgb8(251, 191, 36) }           // #FBBF24 琥珀（Amber-400）
pub fn danger()  -> Color { Color::from_rgb8(248, 113, 113) }          // #F87171 珊瑚红（Red-400）
pub fn info()    -> Color { accent_teal() }

/// 构造应用级自定义暗色主题。
pub fn dark_theme() -> Theme {
    Theme::custom(
        "Qingluo Dark v2",
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
pub const TITLE_BAR_BG: Color = Color { r: 13.0/255.0, g: 18.0/255.0, b: 32.0/255.0, a: 1.0 };
pub const ACTIVITY_BAR_BG: Color = Color { r: 13.0/255.0, g: 18.0/255.0, b: 32.0/255.0, a: 1.0 };
pub const SIDEBAR_BG: Color = Color { r: 17.0/255.0, g: 22.0/255.0, b: 38.0/255.0, a: 1.0 };
pub const PANEL_BG: Color = Color { r: 15.0/255.0, g: 20.0/255.0, b: 34.0/255.0, a: 1.0 };
pub const CANVAS_BG: Color = Color { r: 9.0/255.0, g: 12.0/255.0, b: 20.0/255.0, a: 1.0 };
pub const CANVAS_GRID: Color = Color { r: 29.0/255.0, g: 35.0/255.0, b: 54.0/255.0, a: 1.0 };
pub const CARD_BG: Color = Color { r: 24.0/255.0, g: 30.0/255.0, b: 48.0/255.0, a: 1.0 };
pub const CARD_STROKE: Color = Color { r: 45.0/255.0, g: 55.0/255.0, b: 82.0/255.0, a: 1.0 };
pub const STATUS_BAR_BG: Color = Color { r: 19.0/255.0, g: 25.0/255.0, b: 42.0/255.0, a: 1.0 };
pub const ACCENT: Color = Color { r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 1.0 };
pub const HOVER_BG: Color = Color { r: 35.0/255.0, g: 42.0/255.0, b: 66.0/255.0, a: 1.0 };
pub const DIVIDER: Color = Color { r: 35.0/255.0, g: 42.0/255.0, b: 66.0/255.0, a: 1.0 };
pub const TEXT_STRONG: Color = Color { r: 235.0/255.0, g: 238.0/255.0, b: 250.0/255.0, a: 1.0 };
pub const TEXT_HOVER: Color = Color { r: 215.0/255.0, g: 220.0/255.0, b: 240.0/255.0, a: 1.0 };
pub const TEXT_WEAK: Color = Color { r: 142.0/255.0, g: 151.0/255.0, b: 185.0/255.0, a: 1.0 };

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
                    r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 95.0/255.0,
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
                Color { r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 95.0/255.0 },
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
            r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 35.0/255.0,
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
