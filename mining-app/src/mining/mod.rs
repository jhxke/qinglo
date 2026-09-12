//! 挖掘子系统根模块（与 [`crate::ui::mining`] 视图层对应的数据层 / 服务层）。
//!
//! 这里把 DAG 编排相关的非 UI 模块统一收纳：
//!
//! | 子模块 | 角色 |
//! |---|---|
//! | [`dag`] | DAG 图数据结构、算子类型注册表、参数定义 |
//! | [`dag_store`] | 建模（一张 DAG）的磁盘序列化 / 列表 / CRUD |
//! | [`geom`] | 画布用的 `Vec2` 几何类型，被数据层与 UI 层共用 |
//! | [`debug_executor`] | DAG 调试会话、诊断信息 |
//! | [`operator_executor`] | 调用算子 runtime 服务执行 DAG / 节点 / 流式 |
//! | [`data_preview`] | Debug 模式下数据预览缓存与序列化 |
//!
//! 模块组织原则：UI 视图族在 [`crate::ui::mining`]，本模块只承载"与 DAG
//! 数据生命周期相关"的逻辑，避免出现 `crate::dag` / `crate::dag_store`
//! / `crate::geom` 这种与 UI 模块平级的散落数据层。
//!
//! 子模块内部互引用统一用 `crate::mining::xxx` 全路径，让"是否跨子模块"
//! 在调用点即可读出，便于后续按需做更深一层的拆分（例如把 operator_executor
//! 升级成独立 crate 时只需改一处 `crate::mining::` 前缀）。

pub mod dag;
pub mod dag_store;
pub mod data_preview;
pub mod debug_executor;
pub mod geom;
pub mod operator_executor;
