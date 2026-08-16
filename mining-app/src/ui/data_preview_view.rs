//! 数据预览表格视图：分页查询 + 200 行 × N 列纯展示。
//!
//! **阶段 1 占位**：原 egui 版本 608 行。memory 提示：单元格严禁挂
//! on_hover_text 等交互事件，避免上千传感器拖死 UI。阶段 2 起用
//! Iced `scrollable` + `column!`/`row!` 纯 `text` 渲染，沿用同样约束。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_data_preview_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "数据预览（阶段 1 占位）",
        "阶段 2 起回填：分页表格 + 200 行上限 + 纯 text 渲染",
    )
}
