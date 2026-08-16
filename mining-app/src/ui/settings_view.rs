//! 系统设置视图：Rust 工具链路径 / 编译目录 / 测试 / 自动检测。
//!
//! **阶段 1 占位**：原 egui 版本 205 行，含路径输入框 / 测试 / 保存 /
//! 自动检测按钮 + 结果展示。阶段 2 起回填完整 UI。

use iced::Element;
use super::state::{Message, UiState};
use super::placeholder_view;

pub fn view_settings(_state: &UiState) -> Element<'_, Message> {
    placeholder_view(
        "系统设置（阶段 1 占位）",
        "阶段 2 起回填：Rust 工具链路径 / 编译目录 / 测试 / 自动检测",
    )
}
