//! 自有几何类型，避免在 `dag.rs` / `state.rs` 等核心数据结构层直接依赖
//! `iced::Vector` / `iced::Point`（这些类型不 derive `Serialize/Deserialize`，
//! 会导致 `Node` 等需要序列化的结构无法编译）。
//!
//! `Vec2` 提供与原 `egui::Vec2` 兼容的字段名 `x` / `y`，并通过 `From` 互转
//! 与 `iced::Vector` 之间无障碍转换。视图层需要 iced::Vector 时用 `.into()`
//! 即可。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl From<iced::Vector> for Vec2 {
    fn from(v: iced::Vector) -> Self {
        Self { x: v.x, y: v.y }
    }
}

impl From<Vec2> for iced::Vector {
    fn from(v: Vec2) -> Self {
        iced::Vector::new(v.x, v.y)
    }
}
