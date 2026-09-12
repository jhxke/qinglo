//! 直方图预览视图：按 x_col/y_col/left_col/right_col 渲染柱状图。
//!
//! **阶段 1 占位**：原 egui 版本 604 行，重度依赖 `egui::Painter` 绘制
//! 坐标轴 / 柱体 / 标签 / 悬停 tooltip。阶段 2 起用 `iced::widget::canvas`
//! 重写，沿用 factor_histogram_operator / histogram_visualization_operator
//! 的列结构约定。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_histogram_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "直方图预览（阶段 1 占位）",
        "阶段 2 起用 iced::widget::canvas 重写坐标轴 / 柱体 / 标签",
    )
}
