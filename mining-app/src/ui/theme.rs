//! Trae 风格深黑主题色板。
//!
//! 从 egui 迁移到 Iced 后颜色由 `egui::Color32`（u8 RGBA）改为
//! `iced::Color`（f32 0..1），通过 `Color::from_rgb8` 构造。
//!
//! 注：`iced::Color::from_rgb8` 不是 `const fn`，所以所有颜色都提供
//! `fn xxx() -> Color` 形式，便于使用方直接引用（不占内存、每次调用都
//! 等价）。圆角/尺寸这类 `f32` 仍然保留为 `const`。

use iced::{Color, Theme};
use iced::theme::Palette;

pub fn title_bar_bg() -> Color { Color::from_rgb8(18, 18, 18) }          // #121212
pub fn activity_bar_bg() -> Color { Color::from_rgb8(18, 18, 18) }      // #121212
pub fn sidebar_bg() -> Color { Color::from_rgb8(22, 22, 22) }           // #161616
pub fn panel_bg() -> Color { Color::from_rgb8(18, 18, 18) }             // #121212
pub fn canvas_bg() -> Color { Color::from_rgb8(15, 15, 15) }            // #0F0F0F
pub fn canvas_grid() -> Color { Color::from_rgb8(34, 34, 36) }          // #222224

pub fn card_bg() -> Color { Color::from_rgb8(28, 28, 30) }              // #1C1C1E
pub fn card_stroke() -> Color { Color::from_rgb8(48, 48, 50) }          // #303032

pub fn status_bar_bg() -> Color { Color::from_rgb8(30, 30, 32) }        // #1E1E20
pub fn status_bar_hover() -> Color {
    Color { r: 1.0, g: 1.0, b: 1.0, a: 30.0 / 255.0 }
}

pub fn accent() -> Color { Color::from_rgb8(56, 130, 245) }             // #3882F5
pub fn accent_dim() -> Color {
    Color { r: 56.0 / 255.0, g: 130.0 / 255.0, b: 245.0 / 255.0, a: 80.0 / 255.0 }
}

pub fn hover_bg() -> Color { Color::from_rgb8(40, 40, 42) }             // #28282A
pub fn divider() -> Color { Color::from_rgb8(40, 40, 42) }              // #28282A

pub const WIDGET_ROUNDING: f32 = 5.0;
pub const CARD_ROUNDING: f32 = 8.0;
pub const FLOAT_ROUNDING: f32 = 10.0;

pub fn text_strong() -> Color { Color::from_rgb8(232, 232, 234) }
pub fn text_hover() -> Color { Color::from_rgb8(218, 218, 220) }
pub fn text_weak() -> Color { Color::from_rgb8(140, 140, 144) }

pub fn success() -> Color { Color::from_rgb8(80, 220, 110) }
pub fn warning() -> Color { Color::from_rgb8(240, 190, 70) }
pub fn danger()  -> Color { Color::from_rgb8(245, 100, 100) }

/// 构造应用级自定义暗色主题。
///
/// iced 0.14 的 `Theme` 是 enum，无法子类化，但可以通过 `Theme::custom`
/// 创建基于 [`Palette`] 的 Custom 变体。`Palette` 仅含 6 个语义色
/// （background / text / primary / success / warning / danger），
/// 用于 iced 内部为没有显式 `style` 闭包覆盖的 widget 自动应用颜色。
/// 细粒度颜色（标题栏、画布、卡片等）仍由本文件中的 [`Color`]
/// 常量/函数提供，通过 widget 的 `style(|_t, ...)| {...})` 闭包覆盖。
///
/// 调用方：`MyApp::theme` 返回此函数的结果，代替 `Theme::Dark`。
pub fn dark_theme() -> Theme {
    Theme::custom(
        "Qingluo Dark",
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

// 为便于过渡，保留旧常量名作为函数别名
pub const TITLE_BAR_BG: Color = Color { r: 18.0/255.0, g: 18.0/255.0, b: 18.0/255.0, a: 1.0 };
pub const ACTIVITY_BAR_BG: Color = Color { r: 18.0/255.0, g: 18.0/255.0, b: 18.0/255.0, a: 1.0 };
pub const SIDEBAR_BG: Color = Color { r: 22.0/255.0, g: 22.0/255.0, b: 22.0/255.0, a: 1.0 };
pub const PANEL_BG: Color = Color { r: 18.0/255.0, g: 18.0/255.0, b: 18.0/255.0, a: 1.0 };
pub const CANVAS_BG: Color = Color { r: 15.0/255.0, g: 15.0/255.0, b: 15.0/255.0, a: 1.0 };
pub const CANVAS_GRID: Color = Color { r: 34.0/255.0, g: 34.0/255.0, b: 36.0/255.0, a: 1.0 };
pub const CARD_BG: Color = Color { r: 28.0/255.0, g: 28.0/255.0, b: 30.0/255.0, a: 1.0 };
pub const CARD_STROKE: Color = Color { r: 48.0/255.0, g: 48.0/255.0, b: 50.0/255.0, a: 1.0 };
pub const STATUS_BAR_BG: Color = Color { r: 30.0/255.0, g: 30.0/255.0, b: 32.0/255.0, a: 1.0 };
pub const ACCENT: Color = Color { r: 56.0/255.0, g: 130.0/255.0, b: 245.0/255.0, a: 1.0 };
pub const HOVER_BG: Color = Color { r: 40.0/255.0, g: 40.0/255.0, b: 42.0/255.0, a: 1.0 };
pub const DIVIDER: Color = Color { r: 40.0/255.0, g: 40.0/255.0, b: 42.0/255.0, a: 1.0 };
pub const TEXT_STRONG: Color = Color { r: 232.0/255.0, g: 232.0/255.0, b: 234.0/255.0, a: 1.0 };
pub const TEXT_HOVER: Color = Color { r: 218.0/255.0, g: 218.0/255.0, b: 220.0/255.0, a: 1.0 };
pub const TEXT_WEAK: Color = Color { r: 140.0/255.0, g: 140.0/255.0, b: 144.0/255.0, a: 1.0 };
