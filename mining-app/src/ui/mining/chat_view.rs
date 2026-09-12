//! 聊天预览视图：解析 chat DSL 并渲染气泡界面（用户 / 助手 / 流式状态 / token 计数）。
//!
//! **阶段 1 占位**：原 egui 版本 698 行。阶段 2 起用 Iced Column + 自定义
//! Container 样式重写气泡，并用 subscription 推进 streaming 状态。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_chat_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "聊天预览（阶段 1 占位）",
        "阶段 2 起回填：chat DSL 解析 + 气泡渲染 + 流式状态",
    )
}
