//! 算子输出「数据预览」浮动窗口。
//!
//! 在 `DagEditorState.preview_node_id` 被设置时，渲染一个可拖动/缩放的
//! `egui::Window`，从 `cache/` 目录读取该节点最近一次执行写入的预览数据
//! （前 [`MAX_PREVIEW_ROWS`] 行）并以表格形式展示。
//!
//! Debug 模式下（`tab.debug_mode && tab.debug_session_id.is_some()`），预览窗口
//! 不再读本地缓存，而是直接向服务端分页查询完整输出数据：先查 meta 获取各端口
//! 的类型/行数/页数，再按用户选择的端口+页码查询实际数据切片。这样可以在不
//! 截断的情况下浏览大数据量，方便调试。
//!
//! 性能注意：
//! - 浮动窗口最多展示前 [`MAX_GUI_RENDER_ROWS`] 行，避免一次性渲染上万 Label；
//! - 表格单元格**不挂任何交互事件**（on_hover_text 等）。200 行 × N 列若每个单元
//!   格都挂 hover 传感器，每帧命中检测开销巨大，会拖慢甚至拖死 UI。

use egui::{Color32, Grid, ScrollArea, Ui};
use operator_executor_client::{ColumnData, DataFrame, DataType, PortData};
use operator_executor_client::runtime_client::DebugNodeMeta;

use super::state::{DagTab, DebugPreviewState};
use crate::data_preview::{self, PreviewData, MAX_PREVIEW_ROWS};

/// 浮动预览窗口实际渲染的最大行数（服务端截断仍为 MAX_PREVIEW_ROWS）。
/// 渲染过多 Label 会导致 egui 布局/绘制开销过大，表现为拖动卡顿。
const MAX_GUI_RENDER_ROWS: usize = 200;

/// Debug 模式下 DataFrame 端口分页的每页行数（与服务端 PREVIEW_ROW_LIMIT 一致）。
const DEBUG_PAGE_SIZE: usize = 200;

/// 渲染数据预览浮动窗口。`preview_node_id` 为 None 时直接返回。
pub fn render_data_preview_window(ui: &mut Ui, tab: &mut DagTab) {
    let node_id = match tab.preview_node_id.clone() {
        Some(id) => id,
        None => {
            // 预览窗口关闭时清空 Debug 预览状态，下次打开重新查询
            tab.debug_preview = None;
            return;
        }
    };

    // 判断是否走 Debug 模式预览（服务端分页查询）
    let debug_active = tab.debug_mode && tab.debug_session_id.is_some();
    let session_id = tab.debug_session_id.clone();

    // 节点可能已被删除：优先用缓存里的名称，其次从图查找，最后回退到 ID。
    let cache = if debug_active { None } else { data_preview::load_preview_cache(&node_id) };
    let graph_name = tab
        .graph
        .get_node(&node_id)
        .map(|n| n.operator_type.name().to_string());
    let node_name = if debug_active {
        graph_name.clone().unwrap_or_else(|| node_id.clone())
    } else {
        cache
            .as_ref()
            .map(|c| c.node_name.clone())
            .filter(|n| !n.is_empty())
            .or(graph_name)
            .unwrap_or_else(|| node_id.clone())
    };

    let mut open = true;
    let title = if debug_active {
        format!("数据预览 [Debug] - {}", node_name)
    } else {
        format!("数据预览 - {}", node_name)
    };

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
            if debug_active {
                render_debug_preview_body(ui, tab, &node_id, &session_id.unwrap(), &node_name);
            } else {
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
            }
        });

    if !open {
        tab.preview_node_id = None;
        tab.debug_preview = None;
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

// ============================================================================
// Debug 模式：服务端分页查询
// ============================================================================

/// 向服务端查询 Debug 会话中某节点的输出元信息。
fn query_meta_from_server(
    session_id: &str,
    node_id: &str,
) -> Result<DebugNodeMeta, String> {
    crate::operator_executor::with_runtime_client(|client| {
        client.query_debug_node_meta(session_id, node_id, DEBUG_PAGE_SIZE)
    })
    .map_err(|e| e.to_string())
}

/// 向服务端查询 Debug 会话中某节点指定端口、指定页的数据切片。
fn query_page_from_server(
    session_id: &str,
    node_id: &str,
    port_idx: usize,
    page_idx: usize,
) -> Result<Option<PortData>, String> {
    crate::operator_executor::with_runtime_client(|client| {
        client.query_debug_node_page(
            session_id,
            node_id,
            port_idx,
            page_idx,
            DEBUG_PAGE_SIZE,
        )
    })
    .map(|p| p.page_data)
    .map_err(|e| e.to_string())
}

/// Debug 模式下数据预览主体：向服务端分页查询完整输出。
///
/// 流程：
/// 1. 初始化 `debug_preview` 状态（节点 ID 变化时重新初始化）
/// 2. 首次打开时从服务端查询 meta（端口数、类型、行数、页数）
/// 3. 渲染端口选择器（多端口时）+ 页码导航
/// 4. 若缓存的页与当前选择不匹配，向服务端查询新页数据
/// 5. 渲染当前页数据
fn render_debug_preview_body(
    ui: &mut Ui,
    tab: &mut DagTab,
    node_id: &str,
    session_id: &str,
    node_name: &str,
) {
    // ---- 1. 初始化 / 重置状态 ----
    let needs_init = tab
        .debug_preview
        .as_ref()
        .map_or(true, |s| s.node_id != node_id);
    if needs_init {
        tab.debug_preview = Some(DebugPreviewState {
            node_id: node_id.to_string(),
            ..Default::default()
        });
    }

    // ---- 2. 查询 meta（仅一次，失败后记录 error 不再重试）----
    let need_meta = tab
        .debug_preview
        .as_ref()
        .map_or(true, |s| s.meta.is_none() && s.error.is_none());
    if need_meta {
        match query_meta_from_server(session_id, node_id) {
            Ok(meta) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.meta = Some(meta);
                    state.error = None;
                }
            }
            Err(e) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.error = Some(format!("查询节点元信息失败: {}", e));
                }
            }
        }
    }

    // ---- 3. 渲染 ----
    // 分离借用：先取出需要的字段值（Clone），再渲染，避免跨调用借用冲突
    let meta_opt = tab.debug_preview.as_ref().and_then(|s| s.meta.clone());
    let error_opt = tab.debug_preview.as_ref().and_then(|s| s.error.clone());

    // 顶部信息栏
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("节点: {}", node_name));
        ui.separator();
        ui.colored_label(
            Color32::from_rgb(100, 200, 255),
            format!("Debug 会话: {}…{}", &session_id[..8], &session_id[session_id.len()-4..]),
        );
    });
    ui.separator();

    // 错误展示
    if let Some(err) = &error_opt {
        ui.colored_label(Color32::from_rgb(231, 76, 60), err);
        ui.add_space(8.0);
        if ui.button("重试").clicked() {
            if let Some(state) = &mut tab.debug_preview {
                state.meta = None;
                state.error = None;
            }
        }
        return;
    }

    let meta = match &meta_opt {
        Some(m) => m,
        None => {
            ui.label("正在查询节点元信息...");
            return;
        }
    };

    // 节点不在 Debug 会话中（端口数为 0）
    if meta.port_types.is_empty() {
        ui.colored_label(
            Color32::from_rgb(220, 180, 80),
            "该节点不在 Debug 会话中（可能未执行或执行失败）。请先在 Debug 模式下执行该算子。",
        );
        return;
    }

    // ---- 端口选择器 ----
    let port_count = meta.port_types.len();
    let mut current_port = tab
        .debug_preview
        .as_ref()
        .map_or(0, |s| s.current_port_idx);
    if current_port >= port_count {
        current_port = 0;
        if let Some(state) = &mut tab.debug_preview {
            state.current_port_idx = 0;
        }
    }

    if port_count > 1 {
        ui.horizontal(|ui| {
            ui.strong("输出端口:");
            egui::ComboBox::from_id_source("debug_port_combo")
                .selected_text(format!(
                    "#{} ({})",
                    current_port,
                    meta.port_types.get(current_port).map_or("?", |s| s.as_str())
                ))
                .show_ui(ui, |ui| {
                    for i in 0..port_count {
                        let ptype = meta.port_types.get(i).map_or("?", |s| s.as_str());
                        let rows = meta.port_row_counts.get(i).copied().unwrap_or(0);
                        let pages = meta.port_page_counts.get(i).copied().unwrap_or(0);
                        ui.selectable_value(
                            &mut current_port,
                            i,
                            format!("#{} {} ({} 行 / {} 页)", i, ptype, rows, pages),
                        );
                    }
                });
        });
        ui.separator();
        if let Some(state) = &mut tab.debug_preview {
            state.current_port_idx = current_port;
        }
    }

    // ---- 页码导航 ----
    let page_count = meta
        .port_page_counts
        .get(current_port)
        .copied()
        .unwrap_or(1);
    let port_type = meta
        .port_types
        .get(current_port)
        .map_or("?", |s| s.as_str());
    let port_rows = meta
        .port_row_counts
        .get(current_port)
        .copied()
        .unwrap_or(0);

    let mut current_page = tab
        .debug_preview
        .as_ref()
        .and_then(|s| s.current_pages.get(&current_port).copied())
        .unwrap_or(0);
    if current_page >= page_count {
        current_page = 0;
    }

    // DataFrameArray 端口：page_idx = DataFrame 下标
    // DataFrame 端口：page_idx = 行页号
    // 标量端口：page_count = 1，不显示导航
    let is_scalar = !matches!(port_type, "DataFrameArray" | "DataFrame");

    if !is_scalar && page_count > 1 {
        ui.horizontal(|ui| {
            ui.strong(if port_type == "DataFrameArray" {
                format!("DataFrame 切换 (共 {} 个)", page_count)
            } else {
                format!("行分页 (共 {} 页)", page_count)
            });
            ui.separator();
            if ui.button("‹").on_hover_text("上一页").clicked() {
                current_page = if current_page == 0 { page_count - 1 } else { current_page - 1 };
            }
            ui.label(format!("[{}/{}]", current_page + 1, page_count));
            if ui.button("›").on_hover_text("下一页").clicked() {
                current_page = (current_page + 1) % page_count;
            }
            ui.separator();
            if port_type == "DataFrame" {
                ui.colored_label(
                    Color32::from_rgb(180, 200, 220),
                    format!("总 {} 行 / 每页 {} 行", port_rows, DEBUG_PAGE_SIZE),
                );
            } else {
                ui.colored_label(
                    Color32::from_rgb(180, 200, 220),
                    format!("共 {} 个 DataFrame", page_count),
                );
            }
        });
        ui.separator();
    }

    // 写回当前页码
    if let Some(state) = &mut tab.debug_preview {
        state.current_pages.insert(current_port, current_page);
    }

    // ---- 4. 查询页数据（缓存不匹配时）----
    let cache_valid = tab
        .debug_preview
        .as_ref()
        .and_then(|s| s.cached_page.as_ref())
        .map_or(false, |(p, pg, _)| *p == current_port && *pg == current_page);

    if !cache_valid {
        match query_page_from_server(session_id, node_id, current_port, current_page) {
            Ok(data) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.cached_page = Some((current_port, current_page, data));
                }
            }
            Err(e) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.cached_page = None;
                    state.error = Some(format!("查询页数据失败: {}", e));
                }
                ui.colored_label(Color32::from_rgb(231, 76, 60), format!("查询页数据失败: {}", e));
                return;
            }
        }
    }

    // ---- 5. 渲染当前页数据 ----
    let cached_data = tab
        .debug_preview
        .as_ref()
        .and_then(|s| s.cached_page.as_ref())
        .map(|(_, _, d)| d.clone());

    match cached_data {
        None => {
            ui.label("正在查询页数据...");
        }
        Some(None) => {
            ui.colored_label(
                Color32::from_rgb(220, 180, 80),
                "该页无数据（端口或页号越界）。",
            );
        }
        Some(Some(data)) => {
            render_debug_port_data(ui, &data, node_id, current_port);
        }
    }
}

/// Debug 模式下渲染单个端口当前页的数据。
///
/// 服务端对 DataFrame / DataFrameArray 端口都返回 `PortData::DataFrame`（单个
/// DataFrame 切片），对标量端口返回原标量。复用 [`render_port_data`] 的渲染逻辑。
fn render_debug_port_data(ui: &mut Ui, data: &PortData, node_id: &str, port_idx: usize) {
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
            render_dataframe_table(ui, df, &format!("debug_df_{}_{}", node_id, port_idx));
        }
        PortData::DataFrameArray(dfs) => {
            // 服务端不应返回 DataFrameArray（已拆为单个 DataFrame），但做兜底处理
            render_dataframe_array(ui, dfs, &format!("debug_dfa_{}_{}", node_id, port_idx));
        }
    }
}
