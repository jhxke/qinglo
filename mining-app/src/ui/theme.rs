use egui::{Color32, Frame};

/// Trae 风格深黑主题色板。
///
/// 相比纯 VS Code 灰，整体基底更接近纯黑，层次靠「卡片浮层」色而非大块灰差产生，
/// 配合圆角让功能区域呈现 IDE 式的轻量卡片化观感。
///
/// 层次：标题栏 ≈ 活动栏 ≈ 主内容区（最深底） < 侧边栏 < 卡片浮层 < 画布网格。
pub const TITLE_BAR_BG: Color32 = Color32::from_rgb(18, 18, 18);       // #121212
pub const ACTIVITY_BAR_BG: Color32 = Color32::from_rgb(18, 18, 18);    // #121212（与标题栏一致）
pub const SIDEBAR_BG: Color32 = Color32::from_rgb(22, 22, 22);         // #161616
pub const PANEL_BG: Color32 = Color32::from_rgb(18, 18, 18);           // #121212
pub const CANVAS_BG: Color32 = Color32::from_rgb(24, 24, 26);          // #18181A
pub const CANVAS_GRID: Color32 = Color32::from_rgb(42, 42, 44);        // #2A2A2C 次网格，灰黑，低调不突兀
pub const CANVAS_GRID_MAJOR: Color32 = Color32::from_rgb(58, 58, 60);  // #3A3A3C 主网格，略亮的灰黑

/// 卡片 / 浮层底色：比主内容区略亮一档，用于 group 区块、日志面板、Tab 条等
/// 「功能区域」，配合圆角形成轻微抬升（elevation）效果。
pub const CARD_BG: Color32 = Color32::from_rgb(28, 28, 30);            // #1C1C1E
/// 卡片描边：低明度细线，提供柔和边界而不显割裂。
pub const CARD_STROKE: Color32 = Color32::from_rgb(48, 48, 50);        // #303032

/// 底部状态栏：比卡片略沉，作为窗口底部的收束条，
/// 文本使用白色以保持可读对比度。
pub const STATUS_BAR_BG: Color32 = Color32::from_rgb(30, 30, 32);      // #1E1E20
/// 状态栏条目悬停态背景（叠加在灰底之上，半透明白）。
/// egui 0.26 的 Color32 无法在 const 中带 alpha，故用函数构造（同 accent_dim 模式）。
pub fn status_bar_hover() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 30)
}

/// 强调色（激活指示条、选中态）
pub const ACCENT: Color32 = Color32::from_rgb(56, 130, 245);           // #3882F5

/// 半透明强调色。egui 0.26 的 Color32 无法在 const 中带 alpha，故用函数构造。
pub fn accent_dim() -> Color32 {
    Color32::from_rgba_unmultiplied(56, 130, 245, 80)
}

/// 交互态背景
pub const HOVER_BG: Color32 = Color32::from_rgb(40, 40, 42);           // #28282A
pub const DIVIDER: Color32 = Color32::from_rgb(40, 40, 42);            // #28282A

/// 统一圆角半径。
/// - 控件级（按钮 / 输入框 / 组合框）使用 [`WIDGET_ROUNDING`]
/// - 卡片 / 面板级功能区域使用 [`CARD_ROUNDING`]
/// - 浮动工具栏 / 弹窗使用 [`FLOAT_ROUNDING`]
pub const WIDGET_ROUNDING: f32 = 5.0;
pub const CARD_ROUNDING: f32 = 8.0;
pub const FLOAT_ROUNDING: f32 = 10.0;

/// 文字层次
pub const TEXT_STRONG: Color32 = Color32::from_rgb(232, 232, 234);
pub const TEXT_HOVER: Color32 = Color32::from_rgb(218, 218, 220);
pub const TEXT_WEAK: Color32 = Color32::from_rgb(140, 140, 144);

/// 构造一个卡片风格的 [`Frame`]：CARD_BG 底 + 圆角 + 细描边。
///
/// 用于 group 区块、设置分区等「功能区域」的容器，配合圆角形成轻微抬升感。
/// 调用方可用 `.fill(...)` / `.inner_margin(...)` 覆盖默认值以适应具体场景。
pub fn card_frame() -> Frame {
    Frame::none()
        .fill(CARD_BG)
        .inner_margin(egui::Margin::same(10.0))
        .rounding(CARD_ROUNDING)
        .stroke(egui::Stroke::new(1.0, CARD_STROKE))
}
