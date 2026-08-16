//! 代码编辑器视图：算子 Rust 源代码编辑（语法高亮 + 行号）。
//!
//! **阶段 1 占位**：原 egui 版本 586 行。阶段 2 起考虑用 `iced::widget::text_editor`
//! （官方富文本编辑器组件，支持光标 / 选区 / 滚动 / 语法高亮样式）重写。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_code_editor_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "代码编辑器（阶段 1 占位）",
        "阶段 2 起用 iced::widget::text_editor 重写",
    )
}
