//! 算子参数编辑器视图：节点参数表单（字符串 / 数字 / 枚举 / 列表）。
//!
//! **阶段 1 占位**：原 egui 版本 283 行。阶段 2 起用 `iced::widget::form`
//! 系列（text_input / number_input / picklist / checkbox）重写。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_operator_params_editor_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "算子参数编辑器（阶段 1 占位）",
        "阶段 2 起用 iced::widget::form 系列重写",
    )
}
