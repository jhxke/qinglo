pub mod activity_bar;
pub mod chat_view;
pub mod code_editor;
pub mod dag_canvas;
pub mod data_preview_view;
pub mod histogram_view;
pub mod kline_chart_view;
pub mod line_chart_view;
pub mod markdown_view;
pub mod mining_analysis_view;
pub mod operator_development_view;
pub mod operator_params_editor;
pub mod settings_view;
pub mod state;
pub mod status_bar;
pub mod theme;
pub mod title_bar;

pub use state::*;

pub use activity_bar::view_activity_bar;
pub use mining_analysis_view::view_mining_analysis;
pub use mining_analysis_view::poll_dag_exec_task;
pub use mining_analysis_view::release_all_debug_sessions;
pub use mining_analysis_view::try_spawn_pending_dag_exec;
pub use operator_development_view::view_operator_development;
pub use settings_view::view_settings;
pub use status_bar::view_status_bar;
pub use title_bar::view_title_bar;

use iced::{Alignment, Element, Length};
use iced::widget::{column, container, text};

/// 阶段 1 占位视图 helper：居中显示标题 + 副标题，背景为默认暗色。
pub fn placeholder_view(title: &'static str, hint: &'static str) -> Element<'static, Message> {
    let title_w = text(title).color(theme::TEXT_STRONG).size(18.0);
    let hint_w = text(hint).color(theme::TEXT_WEAK).size(12.0);
    let col = column![title_w, hint_w].spacing(8).align_x(Alignment::Center);
    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}
