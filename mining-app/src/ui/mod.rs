pub mod activity_bar;
pub mod chat_view;
pub mod code_editor;
pub mod dag_canvas;
pub mod data_preview_view;
pub mod histogram_view;
pub mod icons;
pub mod kline_chart_view;
pub mod line_chart_view;
pub mod log_panel;
pub mod markdown_view;
pub mod mining_analysis_view;
pub mod operator_params_editor;
pub mod settings_view;
pub mod state;
pub mod status_bar;
pub mod theme;
pub mod title_bar;
/// wry WebView2 菜单试验（仅 Windows）。
#[cfg(windows)]
pub mod webview_menu;
/// 网页菜单插件系统：trait + 注册表 + 内外部插件动态加载（仅 Windows）。
#[cfg(windows)]
pub mod webview_plugins;

pub use state::*;

pub use activity_bar::view_activity_bar;
pub use mining_analysis_view::view_mining_analysis;
pub use mining_analysis_view::poll_dag_exec_task;
pub use mining_analysis_view::release_all_debug_sessions;
pub use mining_analysis_view::try_spawn_pending_dag_exec;
pub use settings_view::view_settings;
pub use status_bar::view_status_bar;
pub use title_bar::view_title_bar;

use iced::{Alignment, Element, Length};
use iced::widget::{column, container, text};

/// 阶段 1 占位视图 helper：居中显示标题 + 副标题，背景为默认暗色。
///
/// 接受任意生命周期的字符串（错误信息是运行时构造的 String），
/// 副标题限宽 480px 自动换行，避免长错误串横向溢出。
pub fn placeholder_view(
    title: impl Into<String>,
    hint: impl Into<String>,
) -> Element<'static, Message> {
    let title_w = text(title.into()).color(theme::TEXT_STRONG).size(18.0);
    let hint_w = text(hint.into())
        .color(theme::TEXT_WEAK)
        .size(12.0)
        .width(Length::Fixed(480.0))
        .align_x(iced::alignment::Horizontal::Center);
    let col = column![title_w, hint_w].spacing(8).align_x(Alignment::Center);
    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}
