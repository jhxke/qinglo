//! 折线图预览渲染引擎。
//!
//! 与 K线预览（`kline_chart_view.rs` 解析 DSL 字符串）不同，折线算子输出的是
//! `DataFrameArray`（透传供下游使用），因此本模块直接从节点预览缓存的
//! `PortData::DataFrameArray` 读取数据，并按节点参数（`date_col`/`close_col`）
//! 用 egui `Painter` 手画折线图（含坐标轴、缩放/滚动、十字光标 tooltip）。
//!
//! 入口 [`render_line_chart_preview_window`]：`tab.line_chart_preview_node_id` 为
//! None 时直接返回；多 DataFrame 以顶部 ComboBox 切换。

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};
use operator_executor_client::{DataFrame, DataType, PortData};

use super::state::DagTab;
use crate::data_preview;

// =============================== 配色与常量 ===============================

/// 折线主色（浅蓝）。与 K线的红/绿蜡烛区分，呼应算子卡片蓝色系。
const LINE_COLOR: Color32 = Color32::from_rgb(86, 180, 233);
/// 数据点圆点半径。
const POINT_RADIUS: f32 = 2.0;
/// 十字光标颜色（半透明白）。egui 0.26 的 Color32 无法在 const 中带 alpha，故用函数。
fn crosshair_color() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 90)
}
/// 单图表交互态（滚动位置 + 可见数量）。与 K线 ChartState 同款语义。
#[derive(Clone, Copy, Default)]
struct ChartState {
    first_visible: usize,
    visible_count: usize, // 0 = 全显
}
const MIN_VISIBLE: usize = 10;

// =============================== 预览窗口 ===============================

/// 渲染折线图预览浮动窗口。`tab.line_chart_preview_node_id` 为 None 时直接返回。
pub fn render_line_chart_preview_window(ui: &mut Ui, tab: &mut DagTab) {
    let node_id = match tab.line_chart_preview_node_id.clone() {
        Some(id) => id,
        None => return,
    };

    // 节点可能已删除：优先用缓存里的名称，其次从图查找，最后回退到 ID。
    let cache = data_preview::load_preview_cache(&node_id);
    let graph_name = tab.graph.get_node(&node_id).map(|n| n.operator_type.name().to_string());
    let node_name = cache
        .as_ref()
        .map(|c| c.node_name.clone())
        .filter(|n| !n.is_empty())
        .or(graph_name)
        .unwrap_or_else(|| node_id.clone());

    // 从节点参数读取 date_col / close_col / title_col（用户在右侧参数面板配置的值）
    let params = tab
        .graph
        .get_node(&node_id)
        .map(|n| extract_line_params(&n.operator_type));

    let mut open = true;
    let title = format!("折线图预览 - {}", node_name);

    let screen = ui.ctx().screen_rect();
    let max_w = (screen.width() * 0.85).max(560.0);
    let max_h = (screen.height() * 0.85).max(360.0);
    let default_w = 900.0f32.min(max_w);
    let default_h = 520.0f32.min(max_h);

    egui::Window::new(title)
        .open(&mut open)
        .default_width(default_w)
        .default_height(default_h)
        .max_size(egui::vec2(max_w, max_h))
        .min_width(420.0)
        .min_height(280.0)
        .resizable(true)
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            match &cache {
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label("该节点尚无预览数据");
                        ui.add_space(4.0);
                        ui.label("请先执行该算子（右键「运行到此结点」或顶部运行）。");
                    });
                }
                Some(data) => render_line_chart_body(ui, data, &node_id, params.as_ref()),
            }
        });

    if !open {
        tab.line_chart_preview_node_id = None;
    }
}

/// 从算子定义的 `port_params` 中提取折线算子参数（`direction == Param` 的项）。
///
/// 折线算子的 `date_col`/`close_col`/`title_col` 都是 String 参数，存储在
/// `default_value` 字段中（用户在右侧参数面板编辑时直接修改该字段）。
fn extract_line_params(operator_type: &crate::dag::OperatorType) -> LineParams {
    let def = operator_type.as_custom();
    let mut params = LineParams::default();
    for p in &def.port_params {
        if p.direction != crate::dag::PortDirection::Param {
            continue;
        }
        match p.name.as_str() {
            "date_col" => params.date_col = p.default_value.clone(),
            "close_col" => params.close_col = p.default_value.clone(),
            "title_col" => params.title_col = p.default_value.clone(),
            _ => {}
        }
    }
    // 回退默认值（与算子端 with_defaults 一致）
    if params.date_col.is_empty() {
        params.date_col = "date".to_string();
    }
    if params.close_col.is_empty() {
        params.close_col = "close".to_string();
    }
    params
}

#[derive(Default, Clone)]
struct LineParams {
    date_col: String,
    close_col: String,
    title_col: String,
}

fn render_line_chart_body(ui: &mut Ui, data: &data_preview::PreviewData, node_id: &str, params: Option<&LineParams>) {
    // 顶栏信息
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("节点: {}", data.node_name));
        ui.separator();
        ui.label(format!("保存时间: {}", data.saved_at));
        if let Some(p) = params {
            ui.separator();
            ui.label(format!("日期列: {} · 收盘价列: {}", p.date_col, p.close_col));
        }
    });
    ui.separator();

    if data.outputs.is_empty() {
        ui.label("该算子无输出数据。");
        return;
    }

    // 找首个 DataFrameArray 输出
    let dfs: Vec<&DataFrame> = data
        .outputs
        .iter()
        .find_map(|p| match p {
            PortData::DataFrameArray(dfs) => Some(dfs.iter().collect()),
            _ => None,
        })
        .unwrap_or_default();

    if dfs.is_empty() {
        let types: Vec<&str> = data.outputs.iter().map(|p| p.type_name()).collect();
        ui.colored_label(
            Color32::from_rgb(231, 76, 60),
            format!("该节点输出非 DataFrameArray（输出类型: {}）", types.join(", ")),
        );
        ui.add_space(6.0);
        ui.label("折线图预览仅适用于「折线可视化算子」等输出 DataFrameArray 的节点。");
        return;
    }

    let params = params.cloned().unwrap_or_default();

    // 单个 DataFrame 直接渲染；多个用 ComboBox 切换（与数据预览/K线多 chart 一致）
    if dfs.len() == 1 {
        render_line_chart(ui, dfs[0], &params, node_id, 0);
    } else {
        let tab_id = ui.id().with("line_chart_tab").with(node_id);
        let mut current: usize =
            ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<usize>(tab_id));
        if current >= dfs.len() {
            current = 0;
        }
        ui.horizontal(|ui| {
            ui.strong(format!("共 {} 个 DataFrame", dfs.len()));
            ui.separator();
            // 标题：优先用 title_col 首行值，否则用 "图表 N"
            let selected_title = chart_title(dfs[current], &params, current);
            egui::ComboBox::from_id_source(tab_id)
                .selected_text(&selected_title)
                .show_ui(ui, |ui| {
                    for (i, df) in dfs.iter().enumerate() {
                        let title = chart_title(df, &params, i);
                        ui.selectable_value(&mut current, i, title);
                    }
                });
            ui.separator();
            ui.colored_label(
                Color32::from_rgb(180, 200, 220),
                format!("{} 行 × {} 列", dfs[current].row_count, dfs[current].columns.len()),
            );
        });
        ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<usize>(tab_id) = current);
        ui.separator();
        render_line_chart(ui, dfs[current], &params, node_id, current);
    }
}

/// 计算单个 DataFrame 的折线图标题。
///
/// 若配置了 `title_col` 且该列存在、首行有值，则用首行值；否则用 "图表 N"。
fn chart_title(df: &DataFrame, params: &LineParams, idx: usize) -> String {
    if !params.title_col.is_empty() {
        if let Some(col) = df.column(&params.title_col) {
            if let Some(s) = col.get_string(0) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    format!("图表 {}", idx + 1)
}

// =============================== 折线图绘制 ===============================

/// 渲染单个 DataFrame 的折线图到指定区域。
///
/// - 从 `date_col` 提取日期标签（String/Int64/Float64 均按字符串格式化；缺失用 `#i` 兜底）；
/// - 从 `close_col` 提取收盘价（必须 Float64，否则在图区中央显示红色提示）；
/// - NaN/Inf 的点会被跳过，折线在该处断开；
/// - 横轴按行索引等间距分布，日期标签采样显示避免重叠；
/// - 支持滚轮缩放、拖拽水平滚动、十字光标 tooltip。
fn render_line_chart(ui: &mut Ui, df: &DataFrame, params: &LineParams, node_id: &str, chart_idx: usize) {
    let n = df.row_count;
    if n == 0 {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label("无数据行");
        });
        return;
    }

    // 提取收盘价列（必须 Float64）
    let close_col = match df.column(&params.close_col) {
        Some(c) if matches!(c.data_type, DataType::Float64) => c,
        Some(c) => {
            render_chart_error(ui, format!(
                "收盘价列 '{}' 类型为 {:?}，折线图需要 Float64",
                params.close_col, c.data_type
            ));
            return;
        }
        None => {
            render_chart_error(ui, format!(
                "缺少收盘价列 '{}'（Float64）",
                params.close_col
            ));
            return;
        }
    };
    let closes = close_col.to_f64_vec();

    // 提取日期列（支持多种类型；缺失用行号兜底）
    let dates = extract_date_labels(df, &params.date_col, n);

    // 分配绘图区域
    let avail_h = ui.available_height().max(220.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), avail_h), Sense::drag());

    let painter = ui.painter().with_clip_rect(rect);

    // 布局边距（与 K线一致：右侧价格轴、底部日期轴）
    let pad = 8.0;
    let right_axis_w = 64.0;
    let bottom_axis_h = 22.0;
    let plot_rect = Rect::from_min_size(
        rect.min + Vec2::new(pad, pad),
        Vec2::new(
            (rect.width() - pad - right_axis_w).max(40.0),
            (rect.height() - pad - bottom_axis_h).max(40.0),
        ),
    );

    // ---- 交互态 ----
    let state_id = ui.id().with("line_chart").with(node_id).with(chart_idx);
    let mut state: ChartState =
        ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<ChartState>(state_id));

    let total = n;
    if state.visible_count == 0 {
        state.visible_count = total;
    }
    let visible_count = state.visible_count.clamp(MIN_VISIBLE, total);
    if state.first_visible + visible_count > total {
        state.first_visible = total.saturating_sub(visible_count);
    }
    let end = (state.first_visible + visible_count).min(total);

    let col_w = plot_rect.width() / visible_count as f32;

    // ---- 滚轮缩放 ----
    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let factor = if scroll > 0.0 { 0.9 } else { 1.1 };
            let new_vc = ((visible_count as f32) * factor).round() as usize;
            let new_vc = new_vc.clamp(MIN_VISIBLE, total);
            let center = state.first_visible as f64 + visible_count as f64 / 2.0;
            state.visible_count = new_vc;
            state.first_visible =
                ((center - new_vc as f64 / 2.0).round() as isize).max(0) as usize;
            state.first_visible = state.first_visible.min(total.saturating_sub(new_vc));
        }
    }

    // ---- 拖拽水平滚动 ----
    if response.dragged() {
        let dx = response.drag_delta().x;
        if col_w > 0.0 {
            let shift = (-dx / col_w) as isize;
            let new_first = (state.first_visible as isize + shift).max(0) as usize;
            state.first_visible = new_first.min(total.saturating_sub(visible_count));
        }
    }

    // ---- 价格范围（可见区间的有效收盘价）----
    let mut min_price = f64::INFINITY;
    let mut max_price = f64::NEG_INFINITY;
    for i in state.first_visible..end {
        if let Some(v) = closes.get(i).copied().flatten() {
            if v.is_finite() {
                min_price = min_price.min(v);
                max_price = max_price.max(v);
            }
        }
    }
    if !min_price.is_finite() || !max_price.is_finite() {
        painter.rect_filled(plot_rect, 0.0, super::theme::CANVAS_BG);
        painter.text(
            plot_rect.center(),
            Align2::CENTER_CENTER,
            "可见区间无有效收盘价数据",
            FontId::proportional(13.0),
            super::theme::TEXT_WEAK,
        );
        write_state(ui, state_id, state);
        return;
    }
    let pad_price = (max_price - min_price).max(1e-9) * 0.05;
    min_price -= pad_price;
    max_price += pad_price;
    if (max_price - min_price).abs() < 1e-9 {
        min_price -= 1.0;
        max_price += 1.0;
    }

    let x_of = |i: usize| -> f32 {
        plot_rect.left() + (i as f32 - state.first_visible as f32 + 0.5) * col_w
    };
    let y_of = |price: f64| -> f32 {
        let ratio = (price - min_price) / (max_price - min_price);
        plot_rect.bottom() - (ratio as f32) * plot_rect.height()
    };

    // ---- 背景 ----
    painter.rect_filled(plot_rect, 0.0, super::theme::CANVAS_BG);

    // ---- 水平网格 + 价格刻度 ----
    let ticks = nice_ticks(min_price, max_price, 5);
    let grid_stroke = Stroke::new(1.0, super::theme::CANVAS_GRID);
    for &t in &ticks {
        let y = y_of(t);
        if (plot_rect.top()..=plot_rect.bottom()).contains(&y) {
            painter.line_segment(
                [Pos2::new(plot_rect.left(), y), Pos2::new(plot_rect.right(), y)],
                grid_stroke,
            );
            painter.text(
                Pos2::new(plot_rect.right() + 4.0, y),
                Align2::LEFT_CENTER,
                format!("{:.2}", t),
                FontId::proportional(11.0),
                super::theme::TEXT_WEAK,
            );
        }
    }

    // ---- 折线（按 None / NaN 分段，避免跨缺失点画长线）----
    let mut pts: Vec<Pos2> = Vec::with_capacity(visible_count);
    for i in state.first_visible..end {
        match closes.get(i).copied().flatten() {
            Some(v) if v.is_finite() => {
                pts.push(Pos2::new(x_of(i), y_of(v)));
            }
            _ => {
                if pts.len() >= 2 {
                    painter.add(Shape::line(pts.clone(), Stroke::new(1.8, LINE_COLOR)));
                }
                pts.clear();
            }
        }
    }
    if pts.len() >= 2 {
        painter.add(Shape::line(pts.clone(), Stroke::new(1.8, LINE_COLOR)));
    }
    // 数据点圆点（仅在可见点不多时绘制，避免密集时糊成一团）
    if visible_count <= 120 {
        for i in state.first_visible..end {
            if let Some(v) = closes.get(i).copied().flatten() {
                if v.is_finite() {
                    let p = Pos2::new(x_of(i), y_of(v));
                    painter.circle_filled(p, POINT_RADIUS, LINE_COLOR);
                }
            }
        }
    }

    // ---- 日期轴（采样显示，避免重叠）----
    let label_step = ((visible_count as f32) * 70.0 / plot_rect.width().max(1.0))
        .ceil()
        .max(1.0) as usize;
    let date_color = super::theme::TEXT_WEAK;
    for i in state.first_visible..end {
        if (i - state.first_visible) % label_step != 0 {
            continue;
        }
        let x = x_of(i);
        let date = truncate_date(dates.get(i).map(|s| s.as_str()).unwrap_or(""));
        painter.text(
            Pos2::new(x, plot_rect.bottom() + 4.0),
            Align2::CENTER_TOP,
            date,
            FontId::proportional(10.0),
            date_color,
        );
    }

    // ---- 十字光标 + tooltip ----
    if response.hovered() {
        if let Some(hover) = response.hover_pos() {
            if plot_rect.contains(hover) {
                draw_dashed_v(&painter, hover.x, plot_rect.top(), plot_rect.bottom(), crosshair_color());
                draw_dashed_h(&painter, hover.y, plot_rect.left(), plot_rect.right(), crosshair_color());
                // 定位数据点
                let rel = (hover.x - plot_rect.left()) / col_w;
                let idx = (state.first_visible as f32 + rel) as isize;
                let idx = idx.clamp(state.first_visible as isize, (end - 1) as isize) as usize;
                let date = dates.get(idx).cloned().unwrap_or_default();
                let close = closes.get(idx).copied().flatten();
                let close_str = match close {
                    Some(v) if v.is_finite() => format!("{:.4}", v),
                    _ => "NULL".to_string(),
                };
                let tip_pos = Pos2::new(hover.x + 12.0, hover.y + 12.0);
                let tip_id = ui.id().with("line_tip").with(chart_idx);
                egui::Area::new(tip_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(tip_pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::group(ui.style())
                            .fill(super::theme::CARD_BG)
                            .show(ui, |ui| {
                                ui.set_min_width(110.0);
                                ui.label(egui::RichText::new(&date).strong().color(super::theme::TEXT_STRONG));
                                ui.label(format!("收: {}", close_str));
                            });
                    });
            }
        }
    }

    write_state(ui, state_id, state);

    // 提示拖拽/缩放
    let _ = response;
}

/// 在图区中央显示红色错误提示（列缺失/类型不符时）。
fn render_chart_error(ui: &mut Ui, msg: String) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.colored_label(Color32::from_rgb(231, 76, 60), msg);
        ui.add_space(6.0);
        ui.label("请在右侧「算子运行参数」面板检查 date_col / close_col 配置。");
    });
}

/// 从 DataFrame 提取日期标签为字符串向量。按列类型分派；列缺失时用行号兜底。
fn extract_date_labels(df: &DataFrame, name: &str, n: usize) -> Vec<String> {
    if let Some(col) = df.column(name) {
        match col.data_type {
            DataType::String => (0..n).map(|i| col.get_string(i).unwrap_or("").to_string()).collect(),
            DataType::Int64 => (0..n).map(|i| col.get_i64(i).map(|v| v.to_string()).unwrap_or_default()).collect(),
            DataType::Float64 => (0..n).map(|i| col.get_f64(i).map(format_float_label).unwrap_or_default()).collect(),
            _ => (0..n).map(|i| format!("#{}", i)).collect(),
        }
    } else {
        (0..n).map(|i| format!("#{}", i)).collect()
    }
}

/// 日期轴标签格式化：保留月-日（若有），否则原样截断到 8 字符。
fn truncate_date(s: &str) -> String {
    if s.len() >= 10 && s.as_bytes()[4] == b'-' {
        s[5..10].to_string()
    } else if s.chars().count() > 8 {
        s.chars().take(8).collect()
    } else {
        s.to_string()
    }
}

/// 浮点日期标签格式化（如 Int 日期被存为 Float64 时）：去掉无意义尾零。
fn format_float_label(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        format!("{:?}", v)
    } else {
        format!("{:?}", v)
    }
}

fn write_state(ui: &Ui, id: egui::Id, state: ChartState) {
    ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<ChartState>(id) = state);
}

/// 画竖向虚线。
fn draw_dashed_v(painter: &Painter, x: f32, y0: f32, y1: f32, color: Color32) {
    let mut y = y0;
    let stroke = Stroke::new(1.0, color);
    while y < y1 {
        let y_next = (y + 6.0).min(y1);
        painter.line_segment([Pos2::new(x, y), Pos2::new(x, y_next)], stroke);
        y = y_next + 4.0;
    }
}

/// 画横向虚线。
fn draw_dashed_h(painter: &Painter, y: f32, x0: f32, x1: f32, color: Color32) {
    let mut x = x0;
    let stroke = Stroke::new(1.0, color);
    while x < x1 {
        let x_next = (x + 6.0).min(x1);
        painter.line_segment([Pos2::new(x, y), Pos2::new(x_next, y)], stroke);
        x = x_next + 4.0;
    }
}

/// 生成「美观」的价格刻度（与 K线 nice_ticks 同实现）。
fn nice_ticks(min: f64, max: f64, count: usize) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || min >= max || count == 0 {
        return vec![];
    }
    let range = max - min;
    let raw_step = range / count as f64;
    let mag = 10f64.powf(raw_step.log10().floor());
    let norm = raw_step / mag;
    let step = (if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    }) * mag;
    let start = (min / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut v = start;
    let mut guard = 0;
    while v <= max + step * 0.5 && guard < 100 {
        ticks.push(v);
        v += step;
        guard += 1;
    }
    ticks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_date_handles_common_formats() {
        assert_eq!(truncate_date("2024-01-02"), "01-02");
        assert_eq!(truncate_date("2024-01-02 10:30"), "01-02");
        assert_eq!(truncate_date("短日期"), "短日期");
        assert_eq!(truncate_date("20240102"), "20240102");
    }

    #[test]
    fn nice_ticks_basic() {
        let t = nice_ticks(0.0, 10.0, 5);
        assert!(!t.is_empty());
        assert!(t[0] >= 0.0);
        assert!(*t.last().unwrap() <= 10.0);
    }

    #[test]
    fn nice_ticks_empty_for_invalid() {
        assert!(nice_ticks(f64::NAN, 10.0, 5).is_empty());
        assert!(nice_ticks(0.0, 10.0, 0).is_empty());
        assert!(nice_ticks(5.0, 5.0, 5).is_empty());
    }
}
