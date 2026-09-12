//! Markdown 渲染视图：算子文档 / 说明展示。
//!
//! **阶段 1 占位**：原 egui 版本 469 行。阶段 2 起用 `iced::widget::markdown`
//! 官方组件重写（支持标题 / 段落 / 代码块 / 列表 / 链接）。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_markdown_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "Markdown 文档（阶段 1 占位）",
        "阶段 2 起用 iced::widget::markdown 重写",
    )
}
