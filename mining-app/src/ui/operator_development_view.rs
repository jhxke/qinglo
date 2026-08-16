//! 算子开发视图：自定义算子编辑器 + Debug 面板 + 运行日志。
//!
//! **阶段 1 占位**：原 egui 版本 428 行，含算子元数据编辑 / 输入端口 /
//! 端口参数 / Rust 代码模板 / Debug 输入框 / 运行按钮 / 诊断结果展示 / 日志。
//! 阶段 2 起回填完整 UI。

use iced::Element;
use super::state::{Message, UiState};
use super::placeholder_view;

pub fn view_operator_development(_state: &UiState) -> Element<'_, Message> {
    placeholder_view(
        "算子开发（阶段 1 占位）",
        "阶段 2 起回填：自定义算子编辑器 + Debug 面板 + 运行日志",
    )
}
