//! 折线图预览视图：按 date_col / close_col 渲染时间序列折线。
//!
//! **阶段 1 占位**：原 egui 版本 551 行。阶段 2 起用 `iced::widget::canvas`
//! 重写坐标轴 / 折线 / 数据点 / 悬停 tooltip。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_line_chart_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "折线图预览（阶段 1 占位）",
        "阶段 2 起用 iced::widget::canvas 重写折线 / 坐标轴 / tooltip",
    )
}
