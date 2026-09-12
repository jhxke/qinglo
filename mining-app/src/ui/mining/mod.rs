//! 挖掘分析视图族子模块。
//!
//! 把与 DAG 编排 / 算子参数 / 日志 / 各类节点可视化相关的视图文件统一收纳
//! 到本子模块下，便于按视图族做代码组织上的切割。模块对外仍通过
//! [`crate::ui`] 顶层重新导出公共入口（`view_mining_analysis` 等），
//! 调用方（main.rs）无需关心实际落点。
//!
//! 子模块内部用 `super::state` / `super::icons` / `super::theme`
//! / `super::placeholder_view` 时，`super::` 解析到本 `mining` 模块，
//! 借助下面的 `pub use` 引入等同于直接访问 [`crate::ui`] 下的同名项，
//! 子文件 `use super::xxx` 路径保持不变，迁移零回归。
//!
//! 不含状态：[`UiState`] / [`Message`] 等核心类型仍归 [`crate::ui::state`]，
//! 本模块只负责"视图如何渲染"。

pub mod chat_view;
pub mod code_editor;
pub mod dag_canvas;
pub mod data_preview_view;
pub mod histogram_view;
pub mod kline_chart_view;
pub mod line_chart_view;
pub mod log_panel;
pub mod markdown_view;
pub mod mining_analysis_view;
pub mod operator_params_editor;

// 把 ui 顶层的 state / icons / theme / placeholder_view 引入本模块命名空间，
// 让子文件继续用 `super::state` / `super::icons` / `super::theme`
// / `super::placeholder_view` 而无需改路径。
pub use super::placeholder_view;
pub use super::state;
pub use super::icons;
pub use super::theme;

// 对外公共入口（保持与重组前 ui::* 的调用路径兼容）。
pub use mining_analysis_view::{
    poll_dag_exec_task, release_all_debug_sessions, try_spawn_pending_dag_exec,
    view_mining_analysis,
};
