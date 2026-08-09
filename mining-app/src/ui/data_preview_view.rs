//! 算子输出「数据预览」浮动窗口。
//!
//! 在 `DagEditorState.preview_node_id` 被设置时，渲染一个可拖动/缩放的
//! `egui::Window`，从 `cache/` 目录读取该节点最近一次执行写入的预览数据
//! （前 [`MAX_PREVIEW_ROWS`] 行）并以表格形式展示。
//!
//! 性能注意：
//! - 浮动窗口最多展示前 [`MAX_GUI_RENDER_ROWS`] 行，避免一次性渲染上万 Label；
//! - 表格单元格**不挂任何交互事件**（on_hover_text 等）。200 行 × N 列若每个单元
//!   格都挂 hover 传感器，每帧命中检测开销巨大，会拖慢甚至拖死 UI。

use egui::{Color32, Grid, ScrollArea, Ui};
use operator_executor_client::{ColumnData, DataFrame, DataType, PortData};

use super::state::DagTab;
use crate::data_preview::{self, PreviewData, MAX_PREVIEW_ROWS};

/// 浮动预览窗口实际渲染的最大行数（服务端截断仍为 MAX_PREVIEW_ROWS）。
/// 渲染过多 Label 会导致 egui 布局/绘制开销过大，表现为拖动卡顿。
const MAX_GUI_RENDER_ROWS: usize = 200;

/// 渲染数据预览浮动窗口。`preview_node_id` 为 None 时直接返回。
pub fn render_data_preview_window(ui: &mut Ui, tab: &mut DagTab) {
    let node_id = match tab.preview_node_id.clone() {
        Some(id) => id,
        None => return,
    };

    // 节点可能已被删除：优先用缓存里的名称，其次从图查找，最后回退到 ID。
    let cache = data_preview::load_preview_cache(&node_id);
    let graph_name = tab
        .graph
        .get_node(&node_id)
        .map(|n| n.operator_type.name().to_string());
    let node_name = cache
        .as_ref()
        .map(|c| c.node_name.clone())
        .filter(|n| !n.is_empty())
        .or(graph_name)
        .unwrap_or_else(|| node_id.clone());

    let mut open = true;
    let title = format!("数据预览 - {}", node_name);

    // 限制窗口最大尺寸为屏幕 85%，避免内容过多撑开全屏。
    let screen = ui.ctx().screen_rect();
    let max_w = (screen.width() * 0.85).max(480.0);
    let max_h = (screen.height() * 0.85).max(320.0);
    let default_w = 720.0f32.min(max_w);
    let default_h = 480.0f32.min(max_h);

    egui::Window::new(title)
        .open(&mut open)
        .default_width(default_w)
        .default_height(default_h)
        .max_size(egui::vec2(max_w, max_h))
        .min_width(360.0)
        .min_height(240.0)
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
                Some(data) => render_preview_body(ui, data, &node_id),
            }
        });

    if !open {
        tab.preview_node_id = None;
    }
}

fn render_preview_body(ui: &mut Ui, data: &PreviewData, node_id: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("节点: {}", data.node_name));
        ui.separator();
        ui.label(format!("保存时间: {}", data.saved_at));
        if data.original_row_count > MAX_PREVIEW_ROWS {
            ui.separator();
            // 预览按顺序从首个 DataFrame 累积取数，直到达到 MAX_PREVIEW_ROWS 行。
            let has_df_array = data.outputs.iter().any(|p| {
                matches!(p, PortData::DataFrameArray(_))
            });
            let hint = if has_df_array {
                format!(
                    "原始共 {} 行，按顺序累积展示前 {} 行（跨多个 DataFrame）",
                    data.original_row_count, MAX_PREVIEW_ROWS
                )
            } else {
                format!("原始 {} 行，仅展示前 {} 行", data.original_row_count, MAX_PREVIEW_ROWS)
            };
            ui.colored_label(Color32::from_rgb(220, 180, 80), hint);
        }
    });
    ui.separator();

    if data.outputs.is_empty() {
        ui.label("该算子无输出数据。");
        return;
    }

    if data.outputs.len() == 1 {
        render_port_data(ui, &data.outputs[0], node_id, 0);
    } else {
        for (port_idx, output) in data.outputs.iter().enumerate() {
            egui::CollapsingHeader::new(format!("输出端口 #{}", port_idx))
                .default_open(true)
                .show(ui, |ui| {
                    render_port_data(ui, output, node_id, port_idx);
                });
        }
    }
}

fn render_port_data(ui: &mut Ui, data: &PortData, node_id: &str, port_idx: usize) {
    match data {
        PortData::Float(v) => {
            ui.label(format!("Float: {}", v));
        }
        PortData::Int(v) => {
            ui.label(format!("Int: {}", v));
        }
        PortData::String(s) => {
            ui.label(format!("String ({} chars): {}", s.chars().count(), truncate_str(s, 200)));
        }
        PortData::Bool(b) => {
            ui.label(format!("Bool: {}", b));
        }
        PortData::DataFrame(df) => {
            render_dataframe_table(ui, df, &format!("df_{}_{}", node_id, port_idx));
        }
        PortData::DataFrameArray(dfs) => {
            render_dataframe_array(ui, dfs, &format!("df_{}_{}", node_id, port_idx));
        }
    };
}

/// 渲染 `DataFrameArray`：默认仅展示第一个 DataFrame，提供切换控件。
///
/// 数组中可能包含多个 DataFrame，若一次性全部渲染（每个最多
/// [`MAX_GUI_RENDER_ROWS`] 行 × 多列），会产生上万 Label，导致拖动/缩放卡顿。
/// 因此这里只渲染「当前选中」的一个，并用 egui 临时数据跨帧保持选中索引。
/// 服务端预览按顺序从首个 DataFrame 累积取数，直到累计达到
/// [`MAX_PREVIEW_ROWS`] 行，后续 DataFrame 会被舍弃。此处按 DataFrame 分页展示。
fn render_dataframe_array(ui: &mut Ui, dfs: &[DataFrame], grid_id: &str) {
    if dfs.is_empty() {
        ui.label("(空数组，无 DataFrame)");
        return;
    }
    // 仅一个 DataFrame 时无需切换控件。
    if dfs.len() == 1 {
        render_dataframe_table(ui, &dfs[0], grid_id);
        return;
    }

    // 选中索引持久化在 egui 临时数据中（以 grid_id 区分），跨帧保持。
    let id = ui.id().with(grid_id);
    let mut current: usize = ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<usize>(id));
    if current >= dfs.len() {
        current = 0;
    }

    ui.horizontal(|ui| {
        ui.strong(format!("共 {} 个 DataFrame", dfs.len()));
        ui.separator();
        if ui.button("‹").on_hover_text("上一个").clicked() {
            current = if current == 0 { dfs.len() - 1 } else { current - 1 };
        }
        egui::ComboBox::from_id_source(format!("{}_combo", grid_id))
            .selected_text(format!("当前 [{}/{}]", current + 1, dfs.len()))
            .show_ui(ui, |ui| {
                for i in 0..dfs.len() {
                    let df = &dfs[i];
                    ui.selectable_value(
                        &mut current,
                        i,
                        format!("DataFrame [{}] ({} 行 × {} 列)", i, df.row_count, df.columns.len()),
                    );
                }
            });
        if ui.button("›").on_hover_text("下一个").clicked() {
            current = (current + 1) % dfs.len();
        }
        ui.separator();
        // 显示当前 DataFrame 的规模，让用户直观了解每个分页的数据量。
        let df = &dfs[current];
        ui.colored_label(
            Color32::from_rgb(180, 200, 220),
            format!("{} 行 × {} 列", df.row_count, df.columns.len()),
        );
    });

    // 写回选中索引（usize 写入开销可忽略）。
    ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<usize>(id) = current);

    // 仅渲染当前选中的 DataFrame；grid_id 带索引，各 DataFrame 保留独立滚动位置。
    let selected_grid_id = format!("{}_{}", grid_id, current);
    render_dataframe_table(ui, &dfs[current], &selected_grid_id);
}

fn render_dataframe_table(ui: &mut Ui, df: &DataFrame, grid_id: &str) {
    ui.horizontal(|ui| {
        ui.label(format!("{} 行 × {} 列", df.row_count, df.columns.len()));
        if df.row_count > MAX_GUI_RENDER_ROWS {
            ui.separator();
            ui.colored_label(
                Color32::from_rgb(160, 180, 220),
                format!("UI 仅渲染前 {} 行（避免卡顿）", MAX_GUI_RENDER_ROWS),
            );
        }
    });
    if df.columns.is_empty() || df.row_count == 0 {
        ui.label("(空表)");
        return;
    }

    let render_rows = df.row_count.min(MAX_GUI_RENDER_ROWS);
    let avail_w = ui.available_width();
    let scroll_id = format!("scroll_{}", grid_id);

    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(4.0, 2.0))
        .show(ui, |ui| {
            // ---- 表头 + 数据（单 Grid 保证列对齐）----
            ScrollArea::both()
                .id_source(scroll_id)
                .max_height(340.0)
                .max_width(avail_w)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Grid::new(grid_id)
                        .striped(true)
                        .min_col_width(64.0)
                        .max_col_width(160.0)
                        .spacing(egui::vec2(6.0, 2.0))
                        .show(ui, |ui| {
                            // ---- 表头行（加粗 + 彩色）----
                            // 不挂任何交互事件：单元格数量大，每个都加 Sense::hover
                            // 会导致每帧上万次命中检测，拖慢/拖死 UI。
                            for col in &df.columns {
                                let name = truncate_str(&col.name, 60);
                                ui.label(
                                    egui::RichText::new(name)
                                        .strong()
                                        .color(Color32::from_rgb(220, 220, 230)),
                                );
                            }
                            ui.end_row();

                            // ---- 数据行 ----
                            // 同样不挂交互事件：render_rows × columns 个单元格若每个都
                            // on_hover_text，会注册海量 hover 传感器，是卡顿/卡死的主因。
                            for row_idx in 0..render_rows {
                                for col in &df.columns {
                                    ui.label(format_cell(col, row_idx));
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    ui.add_space(6.0);
}

/// 单元格显示截断字符数。超出部分直接截断（不再挂 tooltip 事件），避免把列撑得过宽。
const CELL_DISPLAY_CHARS: usize = 80;

/// 将单元格格式化为显示字符串（按 [`CELL_DISPLAY_CHARS`] 截断并附加省略号）。
///
/// 注意：不再返回完整内容供 tooltip 使用。表格单元格数量可达
/// `render_rows × columns`（最多 200 × N），为每个单元格挂 `on_hover_text`
/// 会注册大量 hover 传感器，每帧命中检测开销巨大，是 UI 卡顿/卡死的主因。
fn format_cell(col: &ColumnData, idx: usize) -> String {
    if col.is_null(idx) {
        return "NULL".to_string();
    }
    let raw = match col.data_type {
        DataType::Float64 => col.get_f64(idx).map(format_float).unwrap_or_default(),
        DataType::Int64 => col.get_i64(idx).map(|v| v.to_string()).unwrap_or_default(),
        DataType::String => col.get_string(idx).unwrap_or("").to_string(),
        DataType::Bool => col.get_bool(idx).map(|v| v.to_string()).unwrap_or_default(),
        DataType::Null => "NULL".to_string(),
    };
    truncate_str(&raw, CELL_DISPLAY_CHARS)
}

/// 浮点数格式化：保留 6 位小数并去除无意义尾零，处理特殊值。
fn format_float(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v > 0.0 { "inf".to_string() } else { "-inf".to_string() }
    } else {
        let s = format!("{:.6}", v);
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        if trimmed.is_empty() || trimmed == "-" {
            "0".to_string()
        } else {
            trimmed.to_string()
        }
    }
}

/// 按字符数截断字符串并附加省略号。
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}
