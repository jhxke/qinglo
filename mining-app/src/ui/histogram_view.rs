//! 直方图预览渲染引擎。
//!
//! 与折线图预览类似，直方图展示算子输出的是 `DataFrame`（透传供下游使用），
//! 因此本模块直接从节点预览缓存的 `PortData::DataFrame`（或 `DataFrameArray` 的首个）
//! 读取数据，并按节点参数（`x_col`/`y_col`/`left_col`/`right_col`/`title`）
//! 用 egui `Painter` 手画柱状图（含坐标轴、缩放/滚动、十字光标 tooltip）。
//!
//! 入口 [`render_histogram_preview_window`]：`tab.histogram_preview_node_id` 为
//! None 时直接返回。

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use operator_executor_client::{DataFrame, DataType, PortData};

use super::state::DagTab;
use crate::data_preview;

// =============================== 配色与常量 ===============================

/// 柱子主色（浅蓝绿，直方图算子 color=[52,152,219] 的柔和变体）。
const BAR_COLOR: Color32 = Color32::from_rgb(52, 152, 219);
/// 柱子悬停高亮色（更亮）。
fn bar_hover_color() -> Color32 {
    Color32::from_rgb(80, 180, 240)
}
/// 十字光标颜色（半透明白）。
fn crosshair_color() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 90)
}
/// 单图表交互态（滚动位置 + 可见数量）。
#[derive(Clone, Copy, Default)]
struct ChartState {
    first_visible: usize,
    visible_count: usize, // 0 = 全显
}
const MIN_VISIBLE: usize = 5;

// =============================== 预览窗口 ===============================

/// 渲染直方图预览浮动窗口。`tab.histogram_preview_node_id` 为 None 时直接返回。
pub fn render_histogram_preview_window(ui: &mut Ui, tab: &mut DagTab) {
    let node_id = match tab.histogram_preview_node_id.clone() {
        Some(id) => id,
        None => return,
    };

    let cache = data_preview::load_preview_cache(&node_id);
    let graph_name = tab.graph.get_node(&node_id).map(|n| n.operator_type.name().to_string());
    let node_name = cache
        .as_ref()
        .map(|c| c.node_name.clone())
        .filter(|n| !n.is_empty())
        .or(graph_name)
        .unwrap_or_else(|| node_id.clone());

    // 从节点参数读取 x_col / y_col / left_col / right_col / title
    let params = tab
        .graph
        .get_node(&node_id)
        .map(|n| extract_histogram_params(&n.operator_type));

    let mut open = true;
    let title = format!("直方图预览 - {}", node_name);

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
                Some(data) => render_histogram_body(ui, data, &node_id, params.as_ref()),
            }
        });

    if !open {
        tab.histogram_preview_node_id = None;
    }
}

/// 从算子定义的 `port_params` 中提取直方图算子参数（`direction == Param` 的项）。
fn extract_histogram_params(operator_type: &crate::dag::OperatorType) -> HistogramParams {
    let def = operator_type.as_custom();
    let mut params = HistogramParams::default();
    for p in &def.port_params {
        if p.direction != crate::dag::PortDirection::Param {
            continue;
        }
        match p.name.as_str() {
            "x_col" => params.x_col = p.default_value.clone(),
            "y_col" => params.y_col = p.default_value.clone(),
            "left_col" => params.left_col = p.default_value.clone(),
            "right_col" => params.right_col = p.default_value.clone(),
            "title" => params.title = p.default_value.clone(),
            _ => {}
        }
    }
    // 回退默认值（与算子端 with_defaults 一致）
    if params.x_col.is_empty() {
        params.x_col = "bin_center".to_string();
    }
    if params.y_col.is_empty() {
        params.y_col = "count".to_string();
    }
    if params.left_col.is_empty() {
        params.left_col = "bin_left".to_string();
    }
    if params.right_col.is_empty() {
        params.right_col = "bin_right".to_string();
    }
    params
}

#[derive(Default, Clone)]
struct HistogramParams {
    x_col: String,
    y_col: String,
    left_col: String,
    right_col: String,
    title: String,
}

fn render_histogram_body(ui: &mut Ui, data: &data_preview::PreviewData, node_id: &str, params: Option<&HistogramParams>) {
    // 顶栏信息
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("节点: {}", data.node_name));
        ui.separator();
        ui.label(format!("保存时间: {}", data.saved_at));
        if let Some(p) = params {
            ui.separator();
            ui.label(format!("X: {} · Y: {}", p.x_col, p.y_col));
        }
    });
    ui.separator();

    if data.outputs.is_empty() {
        ui.label("该算子无输出数据。");
        return;
    }

    // 找首个 DataFrame 输出（DataFrame 或 DataFrameArray 的第一个）
    let df_opt: Option<&DataFrame> = data.outputs.iter().find_map(|p| match p {
        PortData::DataFrame(df) => Some(df),
        PortData::DataFrameArray(dfs) => dfs.first(),
        _ => None,
    });

    let df = match df_opt {
        Some(d) => d,
        None => {
            let types: Vec<&str> = data.outputs.iter().map(|p| p.type_name()).collect();
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("该节点输出非 DataFrame（输出类型: {}）", types.join(", ")),
            );
            ui.add_space(6.0);
            ui.label("直方图预览仅适用于「直方图展示算子」等输出 DataFrame 的节点。");
            return;
        }
    };

    let params = params.cloned().unwrap_or_default();

    // 自定义标题：优先用 params.title，否则默认 "直方图"
    if !params.title.is_empty() {
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new(&params.title).color(super::theme::ACCENT));
        });
        ui.separator();
    }

    render_histogram_chart(ui, df, &params, node_id);
}

// =============================== 柱状图绘制 ===============================

/// 渲染单个 DataFrame 的直方图。
///
/// - 从 `x_col` 提取 X 轴值（Float64，分箱中心）；缺失则按行号索引
/// - 从 `y_col` 提取 Y 轴值（Int64 计数或 Float64 频率）
/// - 从 `left_col`/`right_col` 提取箱边界用于 tooltip（缺失则围绕 x_col 估算）
/// - 支持滚轮缩放、拖拽水平滚动、十字光标 tooltip
fn render_histogram_chart(ui: &mut Ui, df: &DataFrame, params: &HistogramParams, node_id: &str) {
    let n = df.row_count;
    if n == 0 {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label("无数据行");
        });
        return;
    }

    // 提取 Y 轴列（计数 Int64 或频率 Float64，统一转为 f64）
    let y_values: Vec<Option<f64>> = match df.column(&params.y_col) {
        Some(col) => match col.data_type {
            DataType::Int64 => (0..n).map(|i| col.get_i64(i).map(|v| v as f64)).collect(),
            DataType::Float64 => (0..n).map(|i| col.get_f64(i)).collect(),
            _ => {
                render_chart_error(ui, format!(
                    "Y 轴列 '{}' 类型为 {:?}，直方图需要 Int64 或 Float64",
                    params.y_col, col.data_type
                ));
                return;
            }
        },
        None => {
            render_chart_error(ui, format!(
                "缺少 Y 轴列 '{}'（Int64/Float64）",
                params.y_col
            ));
            return;
        }
    };

    // 提取 X 轴列（分箱中心，Float64；缺失用行号兜底）
    let x_values: Vec<Option<f64>> = match df.column(&params.x_col) {
        Some(col) if matches!(col.data_type, DataType::Float64) => {
            (0..n).map(|i| col.get_f64(i)).collect()
        }
        Some(col) if matches!(col.data_type, DataType::Int64) => {
            (0..n).map(|i| col.get_i64(i).map(|v| v as f64)).collect()
        }
        _ => (0..n).map(|i| Some(i as f64)).collect(),
    };

    // 提取左右边界列（用于 tooltip，缺失用 None 标记）
    let left_values: Vec<Option<f64>> = match df.column(&params.left_col) {
        Some(col) if matches!(col.data_type, DataType::Float64) => {
            (0..n).map(|i| col.get_f64(i)).collect()
        }
        _ => vec![None; n],
    };
    let right_values: Vec<Option<f64>> = match df.column(&params.right_col) {
        Some(col) if matches!(col.data_type, DataType::Float64) => {
            (0..n).map(|i| col.get_f64(i)).collect()
        }
        _ => vec![None; n],
    };

    // 分配绘图区域
    let avail_h = ui.available_height().max(260.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), avail_h), Sense::drag());

    let painter = ui.painter().with_clip_rect(rect);

    // 布局边距
    let pad = 8.0;
    let right_axis_w = 64.0;
    let bottom_axis_h = 28.0;
    let plot_rect = Rect::from_min_size(
        rect.min + Vec2::new(pad, pad),
        Vec2::new(
            (rect.width() - pad - right_axis_w).max(40.0),
            (rect.height() - pad - bottom_axis_h).max(40.0),
        ),
    );

    // ---- 交互态 ----
    let state_id = ui.id().with("histogram").with(node_id);
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

    let bin_w = plot_rect.width() / visible_count as f32;

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
        if bin_w > 0.0 {
            let shift = (-dx / bin_w) as isize;
            let new_first = (state.first_visible as isize + shift).max(0) as usize;
            state.first_visible = new_first.min(total.saturating_sub(visible_count));
        }
    }

    // ---- Y 值范围（可见区间的有效 Y）----
    let min_y = 0.0f64; // Y 轴从 0 开始（计数/频率都非负）
    let mut max_y = 0.0f64;
    for i in state.first_visible..end {
        if let Some(v) = y_values.get(i).copied().flatten() {
            if v.is_finite() {
                max_y = max_y.max(v);
            }
        }
    }
    if !max_y.is_finite() || max_y <= 0.0 {
        max_y = 1.0;
    }
    let pad_y = max_y * 0.08;
    max_y += pad_y;

    let x_of_bin_left = |i: usize| -> f32 {
        plot_rect.left() + (i as f32 - state.first_visible as f32) * bin_w
    };
    let y_of = |val: f64| -> f32 {
        let ratio = if (max_y - min_y).abs() < 1e-12 {
            0.0
        } else {
            (val - min_y) / (max_y - min_y)
        };
        plot_rect.bottom() - (ratio as f32) * plot_rect.height()
    };

    // ---- 背景 ----
    painter.rect_filled(plot_rect, 0.0, super::theme::CANVAS_BG);

    // ---- 水平网格 + Y 轴刻度 ----
    let ticks = nice_ticks(min_y, max_y, 5);
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
                format_y_tick(t),
                FontId::proportional(11.0),
                super::theme::TEXT_WEAK,
            );
        }
    }

    // ---- 柱子 ----
    // 悬停检测：当前鼠标所在的 bin index
    let hover_bin_idx: Option<usize> = response.hover_pos().and_then(|hover| {
        if !plot_rect.contains(hover) {
            return None;
        }
        let rel = (hover.x - plot_rect.left()) / bin_w;
        let idx = (state.first_visible as f32 + rel) as isize;
        if idx >= state.first_visible as isize && idx < end as isize {
            Some(idx as usize)
        } else {
            None
        }
    });

    let bar_gap = (bin_w * 0.08).min(3.0).max(0.5);
    for i in state.first_visible..end {
        let y_val = y_values.get(i).copied().flatten().unwrap_or(0.0);
        let y_val = if y_val.is_finite() { y_val } else { 0.0 };
        let bar_left = x_of_bin_left(i) + bar_gap;
        let bar_right = x_of_bin_left(i + 1) - bar_gap;
        let bar_top = y_of(y_val);
        let bar_bottom = y_of(0.0);

        let color = if hover_bin_idx == Some(i) {
            bar_hover_color()
        } else {
            BAR_COLOR
        };

        let bar_rect = Rect::from_min_max(
            Pos2::new(bar_left, bar_top),
            Pos2::new(bar_right, bar_bottom),
        );
        painter.rect_filled(bar_rect, 1.0, color);
    }

    // ---- X 轴标签（采样显示，避免重叠）----
    let label_step = ((visible_count as f32) * 70.0 / plot_rect.width().max(1.0))
        .ceil()
        .max(1.0) as usize;
    let label_color = super::theme::TEXT_WEAK;
    for i in state.first_visible..end {
        if (i - state.first_visible) % label_step != 0 {
            continue;
        }
        let x_center = x_of_bin_left(i) + bin_w / 2.0;
        let label = match x_values.get(i).copied().flatten() {
            Some(v) if v.is_finite() => format!("{:.3}", v),
            _ => format!("#{}", i),
        };
        painter.text(
            Pos2::new(x_center, plot_rect.bottom() + 4.0),
            Align2::CENTER_TOP,
            label,
            FontId::proportional(10.0),
            label_color,
        );
    }

    // ---- 十字光标 + tooltip ----
    if let Some(hover) = response.hover_pos() {
        if plot_rect.contains(hover) {
            draw_dashed_v(&painter, hover.x, plot_rect.top(), plot_rect.bottom(), crosshair_color());
            draw_dashed_h(&painter, hover.y, plot_rect.left(), plot_rect.right(), crosshair_color());
            // 定位 bin
            if let Some(idx) = hover_bin_idx {
                let x_center = x_values.get(idx).copied().flatten();
                let y_val = y_values.get(idx).copied().flatten().unwrap_or(0.0);
                let y_val = if y_val.is_finite() { y_val } else { 0.0 };
                let left = left_values.get(idx).copied().flatten();
                let right = right_values.get(idx).copied().flatten();

                // 如果没有明确的左右边界，围绕 x_center 估算
                let (range_left, range_right) = match (left, right, x_center) {
                    (Some(l), Some(r), _) => (l, r),
                    (_, _, Some(c)) => {
                        // 从相邻 bin 估计宽度
                        let prev = x_values.get(idx.saturating_sub(1)).copied().flatten();
                        let next = x_values.get((idx + 1).min(n - 1)).copied().flatten();
                        let half_w = match (prev, next) {
                            (Some(p), _) => (c - p).abs() / 2.0,
                            (_, Some(nx)) => (nx - c).abs() / 2.0,
                            _ => 0.05,
                        };
                        (c - half_w, c + half_w)
                    }
                    _ => (idx as f64 - 0.5, idx as f64 + 0.5),
                };

                let tip_pos = Pos2::new(hover.x + 12.0, hover.y + 12.0);
                let tip_id = ui.id().with("hist_tip").with(node_id);
                egui::Area::new(tip_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(tip_pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::group(ui.style())
                            .fill(super::theme::CARD_BG)
                            .show(ui, |ui| {
                                ui.set_min_width(140.0);
                                ui.label(egui::RichText::new(format!("Bin #{}", idx)).strong().color(super::theme::TEXT_STRONG));
                                ui.label(format!("范围: [{:.4}, {:.4})", range_left, range_right));
                                ui.label(format!("中心: {:.4}", x_center.unwrap_or((range_left + range_right) / 2.0)));
                                let y_label = if params.y_col == "frequency" {
                                    format!("频率: {:.4} ({:.2}%)", y_val, y_val * 100.0)
                                } else {
                                    format!("{}: {}", params.y_col, y_val)
                                };
                                ui.label(y_label);
                            });
                    });
            }
        }
    }

    write_state(ui, state_id, state);
}

/// 在图区中央显示红色错误提示（列缺失/类型不符时）。
fn render_chart_error(ui: &mut Ui, msg: String) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.colored_label(Color32::from_rgb(231, 76, 60), msg);
        ui.add_space(6.0);
        ui.label("请在右侧「算子运行参数」面板检查 x_col / y_col 配置。");
    });
}

fn format_y_tick(v: f64) -> String {
    if v.abs() < 1e-9 {
        "0".to_string()
    } else if v.abs() >= 1000.0 {
        format!("{:.0}", v)
    } else if v.abs() >= 1.0 {
        format!("{:.2}", v)
    } else {
        format!("{:.3}", v)
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

/// 生成「美观」的 Y 轴刻度。
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
    fn nice_ticks_basic() {
        let t = nice_ticks(0.0, 100.0, 5);
        assert!(!t.is_empty());
        assert!(t[0] >= 0.0);
        assert!(*t.last().unwrap() <= 100.0);
    }

    #[test]
    fn nice_ticks_empty_for_invalid() {
        assert!(nice_ticks(f64::NAN, 10.0, 5).is_empty());
        assert!(nice_ticks(0.0, 10.0, 0).is_empty());
        assert!(nice_ticks(5.0, 5.0, 5).is_empty());
    }

    #[test]
    fn format_y_tick_various() {
        assert_eq!(format_y_tick(0.0), "0");
        assert_eq!(format_y_tick(1500.0), "1500");
        assert_eq!(format_y_tick(3.14159), "3.14");
        assert_eq!(format_y_tick(0.12345), "0.123");
    }
}
