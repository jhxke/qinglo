//! K 线图预览视图：解析 kline DSL 并渲染蜡烛图 + 成交量 + 十字光标 + 分页。
//!
//! **阶段 1 占位**：原 egui 版本 1699 行（最大模块），重度依赖
//! `egui::Painter` 绘制坐标轴 / 蜡烛 / 影线 / 成交量柱 / MA 折线 / 十字光标 /
//! 滚动分页。阶段 2 起用 `iced::widget::canvas` + `Program` 重写。

use iced::Element;
use super::state::Message;
use super::placeholder_view;

#[allow(dead_code)]
pub fn view_kline_chart_placeholder() -> Element<'static, Message> {
    placeholder_view(
        "K 线图预览（阶段 1 占位）",
        "阶段 2 起用 iced::widget::canvas 重写蜡烛图 / 成交量 / 十字光标 / 分页",
    )
}
