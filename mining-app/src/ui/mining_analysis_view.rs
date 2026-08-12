use std::sync::mpsc;
use egui::{Align, Area, Color32, Context, Id, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, TopBottomPanel, Ui, Vec2};
use operator_executor_client::protocol::{DagExecutionResult, OperatorCategory, OperatorExecutionStatus};
use crate::dag::{get_all_operator_types, get_operator_categories, Node, OperatorType};
use crate::dag_store;
use super::state::{DagEditorState, DagExecKind, DagExecMessage, DagExecTask, DagTab, JsonDirection, JsonLogEntry, LogCategory, LogLevel, RunLogEntry};
use super::dag_canvas::render_dag_canvas;
use super::operator_params_editor::render_custom_operator_editor;

pub fn render_mining_analysis_view(ui: &mut Ui, editor_state: &mut DagEditorState) {
    // 首次进入时懒加载磁盘建模列表
    if !editor_state.models_loaded {
        editor_state.refresh_models();
    }

    // 左侧建模列表面板（最左，占满全高）
    // 用 exact_width 而非 default_width：exact_width 每帧强制使用指定宽度，不写入
    // egui 的面板宽度记忆。这样窗口缩小被 clamp 压缩显示、但不回写 memory，窗口放大
    // 后仍以 220.0 为准——避免「缩小再放大后宽度不还原」。
    egui::SidePanel::left("models_panel")
        .exact_width(220.0)
        .frame(
            egui::Frame::none()
                .fill(super::theme::SIDEBAR_BG)
                .inner_margin(egui::Margin::same(8.0))
                .rounding(super::theme::CARD_ROUNDING)
                .stroke(egui::Stroke::new(1.0, super::theme::TITLE_BAR_BG)),
        )
        .show_inside(ui, |ui| {
            render_models_panel(ui, editor_state);
        });

    // 算子面板（次左，仅在当前 tab 需要显示时）
    // 先于中央面板渲染，确保中央画布占据算子面板右侧的正确宽度
    let show_op = editor_state
        .active_tab()
        .map(|t| t.show_operator_panel)
        .unwrap_or(false);
    if show_op {
        egui::SidePanel::left("operator_panel")
            .exact_width(240.0)
            .frame(
                egui::Frame::none()
                    .fill(super::theme::SIDEBAR_BG)
                    .inner_margin(egui::Margin::same(8.0))
                    .rounding(super::theme::CARD_ROUNDING)
                    .stroke(egui::Stroke::new(1.0, super::theme::TITLE_BAR_BG)),
            )
            .show_inside(ui, |ui| {
                if let Some(tab) = editor_state.active_tab_mut() {
                    render_operator_panel(ui, tab);
                }
            });
    }

    // 右侧算子参数编辑器（选中自定义算子节点时显示；可由标题栏 × 按钮隐藏）
    // 必须在 CentralPanel 之前声明：egui 中 CentralPanel 会消费所有剩余空间，
    // 若在其之后添加 SidePanel::right，右侧面板将无法预留宽度，转而叠加在画布之上
    // （即「算子参数压在网格上面」的根因）。
    if let Some(idx) = editor_state.active_tab_index {
        let selected_node_id_opt = editor_state.tabs[idx].selected_node_id.clone();
        let is_custom = selected_node_id_opt
            .as_ref()
            .and_then(|id| editor_state.tabs[idx].graph.get_node(id))
            .map(|n| n.operator_type.is_custom())
            .unwrap_or(false);
        let hide_params = editor_state.tabs[idx].hide_params_panel;
        if is_custom && !hide_params {
            if let Some(selected_node_id) = selected_node_id_opt {
                let (modified, close_clicked) = egui::SidePanel::right("custom_operator_editor")
                    .default_width(400.0)
                    .frame(
                        egui::Frame::none()
                            .fill(super::theme::SIDEBAR_BG)
                            .inner_margin(egui::Margin::same(10.0))
                            .rounding(super::theme::CARD_ROUNDING)
                            .stroke(egui::Stroke::new(1.0, super::theme::TITLE_BAR_BG)),
                    )
                    .show_inside(ui, |ui| {
                        // 分离借用 graph / io_registry / custom_op_debug，避免同时持有整个 tab
                        let tab = &mut editor_state.tabs[idx];
                        let graph = &mut tab.graph;
                        let io_registry = &mut tab.io_registry;
                        let debug_state = &mut tab.custom_op_debug;
                        if let Some(node) = graph.get_node_mut(&selected_node_id) {
                            render_custom_operator_editor(ui, node, debug_state, io_registry, &selected_node_id)
                        } else {
                            (false, false)
                        }
                    })
                    .inner;
                if modified {
                    editor_state.tabs[idx].dirty = true;
                }
                if close_clicked {
                    editor_state.tabs[idx].hide_params_panel = true;
                }
            }
        }
    }

    // 底部运行日志面板（可上下拖动顶边调整高度）
    // 同样必须在 CentralPanel 之前声明，才能在左右面板之间正确预留底部高度，
    // 否则会叠加在画布下方。位于左右侧面板之后，因此只横跨中央区域（左右面板仍占满全高）。
    TopBottomPanel::bottom("run_log_panel")
        .default_height(150.0)
        .resizable(true)
        .min_height(80.0)
        .max_height(600.0)
        .frame(
            egui::Frame::none()
                .fill(super::theme::SIDEBAR_BG)
                .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                .rounding(super::theme::CARD_ROUNDING)
                .stroke(egui::Stroke::new(1.0, super::theme::TITLE_BAR_BG)),
        )
        .show_inside(ui, |ui| {
            if let Some(tab) = editor_state.active_tab_mut() {
                render_run_log_panel(ui, tab);
            } else {
                ui.label(RichText::new("无打开的建模").weak());
            }
        });

    // 中央区域：Tab 栏 + DAG 画布（必须最后声明）
    // egui 约定 CentralPanel 永远是最后一个面板：它消费此前所有 SidePanel /
    // TopBottomPanel 预留之后剩下的全部空间。这样画布（网格）就能铺满左右面板与
    // 底部日志之间的中央区域，而不会被算子参数面板或日志面板覆盖。
    egui::CentralPanel::default().show_inside(ui, |ui| {
        // Tab 栏放在中央面板内，确保其宽度正确（算子面板右侧的剩余空间）
        render_tab_bar(ui, editor_state);

        if editor_state.active_tab_index.is_some() {
            // 记录画布区域左上角（Tab 栏下方），用于定位浮动工具栏
            let canvas_origin = ui.cursor().min;

            if let Some(tab) = editor_state.active_tab_mut() {
                render_dag_canvas(ui, tab);
            }

            // 浮动图标工具栏：定位在画布左上角
            // 层级用 Middle（egui::Area 与 Window 的默认层）：作为浮动 Area 仍会盖在
            // CentralPanel 画布内容之上，但数据预览/对话框等 Window 会按 egui 的窗口
            // 堆叠规则压在它上面。切勿用 Foreground——那是给菜单/tooltip 等「永远置顶」
            // 元素用的，会让工具栏盖住所有弹窗。
            Area::new(Id::new("canvas_toolbar"))
                .fixed_pos(egui::pos2(canvas_origin.x + 8.0, canvas_origin.y + 8.0))
                .order(egui::Order::Middle)
                .show(ui.ctx(), |ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgba_unmultiplied(28, 28, 30, 235))
                        .inner_margin(egui::Margin::same(4.0))
                        .rounding(super::theme::FLOAT_ROUNDING)
                        .stroke(egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 18)))
                        .show(ui, |ui| {
                            render_canvas_toolbar(ui, editor_state);
                        });
                });
        } else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("请新建或从左侧打开建模").weak());
            });
        }
    });

    // 数据预览浮动窗口（由算子右键菜单「数据预览」触发）
    if let Some(tab) = editor_state.active_tab_mut() {
        super::data_preview_view::render_data_preview_window(ui, tab);
    }

    // K线图预览浮动窗口（由算子右键菜单「K线图预览」触发，解析节点输出的 K线 DSL）
    if let Some(tab) = editor_state.active_tab_mut() {
        super::kline_chart_view::render_kline_preview_window(ui, tab);
    }

    // 折线图预览浮动窗口（由算子右键菜单「折线图预览」触发，按 date_col/close_col 渲染）
    if let Some(tab) = editor_state.active_tab_mut() {
        super::line_chart_view::render_line_chart_preview_window(ui, tab);
    }

    // 聊天预览浮动窗口（由算子右键菜单「聊天预览」触发，解析节点输出的 chat DSL）
    if let Some(tab) = editor_state.active_tab_mut() {
        super::chat_view::render_chat_preview_window(ui, tab);
    }

    // 直方图预览浮动窗口（由算子右键菜单「直方图预览」触发，按 x_col/y_col 渲染柱状图）
    if let Some(tab) = editor_state.active_tab_mut() {
        super::histogram_view::render_histogram_preview_window(ui, tab);
    }

    // 新建 / 重命名对话框
    render_dialogs(ui, editor_state);

    // 处理上一帧画布/日志面板写入的 pending 执行请求（此时 tab 借用已释放，
    // 可安全持有完整 &mut DagEditorState 触发全局执行任务）
    let pending_all = editor_state
        .active_tab()
        .map(|t| t.pending_run_all)
        .unwrap_or(false);
    let pending_upto = editor_state
        .active_tab()
        .and_then(|t| t.pending_run_up_to.clone());
    if pending_all {
        if let Some(tab) = editor_state.active_tab_mut() {
            tab.pending_run_all = false;
        }
        spawn_run_all(ui.ctx(), editor_state);
    }
    if let Some(node) = pending_upto {
        if let Some(tab) = editor_state.active_tab_mut() {
            tab.pending_run_up_to = None;
        }
        spawn_run_up_to(ui.ctx(), editor_state, &node);
    }
}

// ===== Tab 栏配色与绘制辅助 =====

fn tab_luminance(c: Color32) -> f32 {
    0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32
}

fn tab_darken(c: Color32, t: f32) -> Color32 {
    let f = 1.0 - t;
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

fn tab_blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// 单个 DAG 标签的配色。
struct DagTabColors {
    accent: Color32,
    active_bg: Color32,
    inactive_bg: Color32,
    hover_bg: Color32,
    active_text: Color32,
    inactive_text: Color32,
    divider: Color32,
}

/// 渲染单个 DAG 标签，返回 (是否点击标签体, 是否点击关闭按钮, 标签矩形)。
fn render_dag_tab(
    ui: &mut Ui,
    index: usize,
    width: f32,
    height: f32,
    name: &str,
    dirty: bool,
    is_active: bool,
    colors: &DagTabColors,
) -> (bool, bool, Rect) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();

    // 背景：激活与内容区一致，悬停略提亮，其余沉入标签条底色
    let bg = if is_active {
        colors.active_bg
    } else if hovered {
        colors.hover_bg
    } else {
        colors.inactive_bg
    };
    painter.rect_filled(rect, 0.0, bg);

    // 顶部高亮线：激活为实色，悬停为半透明灰
    let top_strip = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 2.0));
    if is_active {
        painter.rect_filled(top_strip, 0.0, colors.accent);
    } else if hovered {
        painter.rect_filled(top_strip, 0.0, Color32::from_rgba_unmultiplied(170, 170, 174, 80));
    }

    // 非激活标签右侧一条细分隔线，区分相邻标签
    if !is_active {
        painter.line_segment(
            [
                Pos2::new(rect.right() - 0.5, rect.top() + 6.0),
                Pos2::new(rect.right() - 0.5, rect.bottom() - 6.0),
            ],
            Stroke::new(1.0, colors.divider),
        );
    }

    // 文字（过长按字符数截断加省略号，避免溢出到相邻标签）
    let text_color = if is_active { colors.active_text } else { colors.inactive_text };
    let max_chars = 16;
    let display: String = if name.chars().count() > max_chars {
        let mut s: String = name.chars().take(max_chars).collect();
        s.push('…');
        s
    } else {
        name.to_string()
    };
    painter.text(
        Pos2::new(rect.min.x + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        display,
        egui::FontId::proportional(13.0),
        text_color,
    );

    // 关闭按钮 / 未保存圆点
    let close_size = 18.0;
    let close_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 6.0 - close_size * 0.5, rect.center().y),
        Vec2::splat(close_size),
    );
    let close_resp = ui.interact(
        close_rect,
        ui.make_persistent_id(("dag_tab_close", index)),
        Sense::click(),
    );
    let show_close = is_active || hovered || close_resp.hovered();
    if show_close {
        let cx = close_rect.center();
        let s = 4.0;
        if close_resp.hovered() {
            painter.circle_filled(cx, 9.0, colors.hover_bg);
        }
        let icon_color = if is_active || close_resp.hovered() {
            colors.active_text
        } else {
            colors.inactive_text
        };
        let stroke = Stroke::new(1.4, icon_color);
        painter.line_segment([Pos2::new(cx.x - s, cx.y - s), Pos2::new(cx.x + s, cx.y + s)], stroke);
        painter.line_segment([Pos2::new(cx.x - s, cx.y + s), Pos2::new(cx.x + s, cx.y - s)], stroke);
    } else if dirty {
        // 未保存且未悬停时显示圆点（VS Code 风格），提示有未落盘修改
        painter.circle_filled(close_rect.center(), 3.5, colors.accent);
    }

    // 关闭按钮在标签体之后注册交互故处于顶层，优先判定关闭；其余区域点击切换
    let click_close = close_resp.clicked();
    let click_body = response.clicked() && !click_close;
    (click_body, click_close, rect)
}

/// Tab 栏：VS Code 风格的现代标签页切换条。
///
/// - 激活标签背景与内容区一致、顶部带蓝色高亮线，且其正下方不画分隔线，使其与内容区融为一体；
/// - 非激活标签沉入标签条底色，悬停时略微提亮；
/// - 未保存（dirty）且未悬停时，关闭位置显示圆点；悬停或激活时显示 ×。
fn render_tab_bar(ui: &mut Ui, editor_state: &mut DagEditorState) {
    // 标签为空时不渲染
    if editor_state.tabs.is_empty() {
        return;
    }

    let bar_height = 30.0;
    let tab_min_width = 110.0;
    let tab_max_width = 200.0;
    // 标签栏激活指示改用灰色，避免蓝色与整体深黑主题不协调
    let accent = Color32::from_rgb(170, 170, 174);

    let visuals = ui.visuals();
    let panel = visuals.panel_fill;
    let is_dark = tab_luminance(panel) < 128.0;
    // 未选中 tab 背景：参考侧边栏色 (#252526)，与面板 (#1E1E1E) 形成柔和层次，避免纯黑
    let bar_bg = if is_dark {
        let sidebar = super::theme::SIDEBAR_BG;
        tab_blend(panel, sidebar, 0.7)
    } else {
        tab_darken(panel, 0.04)
    };
    let colors = DagTabColors {
        accent,
        active_bg: panel,
        inactive_bg: bar_bg,
        hover_bg: tab_blend(bar_bg, panel, 0.35),
        active_text: visuals.strong_text_color(),
        inactive_text: visuals.weak_text_color(),
        divider: visuals.window_stroke.color,
    };

    // 全部宽度用于 Tab 区
    let tab_area_width = ui.available_width().max(200.0);

    let tab_count = editor_state.tabs.len();
    let tab_width = (tab_area_width / tab_count as f32).clamp(tab_min_width, tab_max_width);

    let mut active_rect: Option<Rect> = None;
    let bar = egui::Frame::none()
        .fill(bar_bg)
        .show(ui, |ui| {
            ui.set_min_height(bar_height);
            ui.set_max_height(bar_height);

            // Tab 区：横向滚动的标签
            ui.horizontal_top(|ui| {
                ui.set_min_height(bar_height);
                ui.allocate_ui_with_layout(
                    Vec2::new(tab_area_width, bar_height),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ScrollArea::horizontal()
                            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                            .show(ui, |ui| {
                                ui.horizontal_top(|ui| {
                                    ui.set_min_height(bar_height);
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    let mut i = 0;
                                    while i < tab_count {
                                        let (name, dirty) = {
                                            let t = &editor_state.tabs[i];
                                            (t.name.clone(), t.dirty)
                                        };
                                        let is_active = editor_state.active_tab_index == Some(i);
                                        let (click_body, click_close, rect) = render_dag_tab(
                                            ui,
                                            i,
                                            tab_width,
                                            bar_height,
                                            &name,
                                            dirty,
                                            is_active,
                                            &colors,
                                        );
                                        if is_active {
                                            active_rect = Some(rect);
                                        }
                                        if click_close {
                                            // 关闭 tab 前释放该 tab 持有的 Debug 会话
                                            if let Some(tab) = editor_state.tabs.get_mut(i) {
                                                release_debug_session_sync(tab);
                                            }
                                            editor_state.close_tab(i);
                                            return;
                                        }
                                        if click_body {
                                            editor_state.switch_to_tab(i);
                                        }
                                        i += 1;
                                    }
                                });
                            });
                    },
                );
            });
        });

    // 标签条底部分隔线
    let bar_rect = bar.response.rect;
    let bottom_y = bar_rect.bottom() - 0.5;
    let stroke = Stroke::new(1.0, colors.divider);
    let painter = ui.painter();
    match active_rect {
        Some(ar) => {
            if ar.left() > bar_rect.left() + 0.5 {
                painter.line_segment(
                    [Pos2::new(bar_rect.left(), bottom_y), Pos2::new(ar.left(), bottom_y)],
                    stroke,
                );
            }
            if ar.right() < bar_rect.right() - 0.5 {
                painter.line_segment(
                    [Pos2::new(ar.right(), bottom_y), Pos2::new(bar_rect.right(), bottom_y)],
                    stroke,
                );
            }
        }
        None => {
            painter.line_segment(
                [Pos2::new(bar_rect.left(), bottom_y), Pos2::new(bar_rect.right(), bottom_y)],
                stroke,
            );
        }
    }
}

/// 画布浮动图标工具栏：横向排列的纯图标按钮（算子面板 · 验证 · 清空 · 保存）。
fn render_canvas_toolbar(ui: &mut Ui, editor_state: &mut DagEditorState) {
    let has_tab = editor_state.active_tab_index.is_some();

    let visuals = ui.visuals();
    let icon_color = visuals.weak_text_color();
    let hover_bg = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    let active_bg = Color32::from_rgba_unmultiplied(56, 130, 245, 40);

    let btn_size = 30.0;
    let gap = 2.0;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;

        // ---- 算子面板切换 ----
        if has_tab {
            let on = editor_state.active_tab().unwrap().show_operator_panel;
            let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(btn_size, btn_size), Sense::click());
            let painter = ui.painter();
            let bg = if on { active_bg } else if resp.hovered() { hover_bg } else { Color32::TRANSPARENT };
            if bg != Color32::TRANSPARENT {
                painter.rect_filled(rect, 5.0, bg);
            }
            let col = if on { super::theme::ACCENT } else { icon_color };
            let stroke = Stroke::new(1.4, col);
            // 侧边栏图标：方框 + 中间竖线
            painter.rect_stroke(rect.shrink(6.0), 2.0, stroke);
            painter.line_segment(
                [Pos2::new(rect.center().x, rect.min.y + 8.0), Pos2::new(rect.center().x, rect.max.y - 8.0)],
                stroke,
            );
            resp = resp.on_hover_text(if on { "隐藏算子面板" } else { "显示算子面板" });
            if resp.clicked() {
                if let Some(tab) = editor_state.active_tab_mut() {
                    tab.show_operator_panel = !on;
                }
            }
        }

        // 分隔线
        if has_tab {
            ui.add_space(gap);
            let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, btn_size - 6.0), Sense::hover());
            ui.painter().line_segment(
                [r.center_top(), r.center_bottom()],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
            );
            ui.add_space(gap);
        }

        // ---- 验证 ----
        if has_tab {
            let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(btn_size, btn_size), Sense::click());
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, 5.0, hover_bg);
            }
            let cx = rect.center().x;
            let cy = rect.center().y;
            // 验证图标用浅蓝色，与「验证通过」的温和确认反馈配色呼应
            // （区别于执行成功的绿色强确认，表示结构合法但不涉及运行结果）
            let verify_color = Color32::from_rgb(140, 200, 255);
            let stroke = Stroke::new(1.6, verify_color);
            // 对勾
            painter.line_segment([Pos2::new(cx - 6.0, cy), Pos2::new(cx - 2.0, cy + 5.0)], stroke);
            painter.line_segment([Pos2::new(cx - 2.0, cy + 5.0), Pos2::new(cx + 7.0, cy - 5.0)], stroke);
            resp = resp.on_hover_text("验证 DAG（检查连线完整性）");
            if resp.clicked() {
                if let Some(tab) = editor_state.active_tab_mut() {
                    let errors = tab.graph.validate();
                    tab.error_message = if errors.is_empty() {
                        Some("DAG 验证通过！".to_string())
                    } else {
                        Some(format!("验证失败:\n{}", errors.join("\n")))
                    };
                }
            }
        }

        // ---- 清空 ----
        if has_tab {
            let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(btn_size, btn_size), Sense::click());
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, 5.0, hover_bg);
            }
            let cx = rect.center().x;
            let cy = rect.center().y;
            let stroke = Stroke::new(1.4, Color32::from_rgb(231, 76, 60));
            // 扫帚🧹：手柄 + 束口 + 梯形扫帚头 + 底部刷毛
            // 与「删除」图标（回收站）区分开：清空用扫帚（清扫），删除用回收站（丢弃）
            let handle_top = cy - 10.0;
            let head_top = cy - 2.0;     // 手柄底 = 扫帚头顶
            let head_bot = cy + 5.0;     // 扫帚头底（刷毛起点）
            let tip_bot = cy + 8.5;      // 刷毛末端
            let half_narrow = 1.5;       // 束口半宽
            let half_wide = 6.0;         // 扫帚头底部半宽
            // 手柄（细长竖线）
            painter.line_segment(
                [Pos2::new(cx, handle_top), Pos2::new(cx, head_top)],
                stroke,
            );
            // 束口（手柄与扫帚头连接处的短水平线）
            painter.line_segment(
                [Pos2::new(cx - half_narrow, head_top), Pos2::new(cx + half_narrow, head_top)],
                stroke,
            );
            // 扫帚头：梯形（上窄下宽）
            painter.line_segment(
                [Pos2::new(cx - half_narrow, head_top), Pos2::new(cx - half_wide, head_bot)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(cx + half_narrow, head_top), Pos2::new(cx + half_wide, head_bot)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(cx - half_wide, head_bot), Pos2::new(cx + half_wide, head_bot)],
                stroke,
            );
            // 刷毛：底部数条短竖线，表现扫帚毛
            painter.line_segment(
                [Pos2::new(cx - 4.5, head_bot), Pos2::new(cx - 4.5, tip_bot)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(cx - 1.5, head_bot), Pos2::new(cx - 1.5, tip_bot)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(cx + 1.5, head_bot), Pos2::new(cx + 1.5, tip_bot)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(cx + 4.5, head_bot), Pos2::new(cx + 4.5, tip_bot)],
                stroke,
            );
            resp = resp.on_hover_text("清空当前 DAG（移除所有节点和连线）");
            if resp.clicked() {
                editor_state.show_clear_confirm_dialog = true;
            }
        }

        // ---- 保存 ----
        if has_tab {
            let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(btn_size, btn_size), Sense::click());
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, 5.0, hover_bg);
            }
            let r = rect.center();
            // 保存按钮使用蓝色（主题强调色），与其他工具栏图标区分，强调「安全写入」语义
            let save_color = super::theme::ACCENT;
            let stroke = Stroke::new(1.5, save_color);
            let s = 7.5; // 略放大，提升辨识度
            // 软盘图标：外框 + 顶部标签 + 底部主体（按 s 等比缩放）
            painter.rect_stroke(
                Rect::from_min_size(Pos2::new(r.x - s, r.y - s), Vec2::new(s * 2.0, s * 2.0)),
                1.0,
                stroke,
            );
            // 顶部金属标签（约 s 宽、s/2 高，居中略偏上）
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(r.x - s * 0.5, r.y - s * 0.75),
                    Vec2::new(s, s * 0.5),
                ),
                0.5,
                save_color,
            );
            // 底部主体标签（略宽于 s，居中略偏下）
            painter.rect_filled(
                Rect::from_min_size(
                    Pos2::new(r.x - s * 0.583, r.y + s * 0.167),
                    Vec2::new(s * 1.167, s * 0.583),
                ),
                0.5,
                save_color,
            );
            resp = resp.on_hover_text("保存当前建模到磁盘");
            if resp.clicked() {
                editor_state.save_active_tab();
                if let Some(tab) = editor_state.active_tab_mut() {
                    tab.add_action_log("建模已保存".to_string(), LogLevel::Success);
                }
            }
        }

        // 分隔线：执行 DAG 作为主操作，与前面的工具按钮分组
        if has_tab {
            ui.add_space(gap);
            let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, btn_size - 6.0), Sense::hover());
            ui.painter().line_segment(
                [r.center_top(), r.center_bottom()],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
            );
            ui.add_space(gap);
        }

        // ---- Debug 模式开关（虫子图标，切换后执行 DAG 会保留服务端数据供分页查询）----
        if has_tab {
            let debug_on = editor_state.active_tab().map_or(false, |t| t.debug_mode);
            let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(btn_size, btn_size), Sense::click());
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, 5.0, hover_bg);
            }
            // Debug 模式开启时用橙色高亮背景，关闭时仅描边
            if debug_on {
                painter.rect_filled(rect, 5.0, Color32::from_rgba_unmultiplied(255, 165, 0, 40));
            }
            // 虫子图标：圆头 + 身体 + 6 条腿
            let bug_color = if debug_on {
                Color32::from_rgb(255, 165, 0)
            } else {
                Color32::from_rgb(160, 160, 170)
            };
            let cx = rect.center().x;
            let cy = rect.center().y;
            let s = 4.0; // 缩放因子
            // 头部（小圆）
            painter.circle_filled(Pos2::new(cx, cy - s * 1.4), s * 0.5, bug_color);
            // 身体（椭圆，用圆近似）
            painter.circle_filled(Pos2::new(cx, cy + s * 0.3), s * 0.9, bug_color);
            // 腿（左右各 3 条）
            let stroke = Stroke::new(1.2, bug_color);
            for i in 0..3 {
                let y_off = (i as f32 - 1.0) * s * 0.5;
                let body_y = cy + s * 0.3 + y_off;
                // 左腿
                painter.line_segment(
                    [Pos2::new(cx - s * 0.8, body_y), Pos2::new(cx - s * 1.8, body_y - s * 0.3)],
                    stroke,
                );
                // 右腿
                painter.line_segment(
                    [Pos2::new(cx + s * 0.8, body_y), Pos2::new(cx + s * 1.8, body_y - s * 0.3)],
                    stroke,
                );
            }
            let hover_text = if debug_on {
                "Debug 模式已开启（点击关闭）：执行后数据保留在服务端，预览支持分页查询"
            } else {
                "Debug 模式已关闭（点击开启）：执行后保留服务端数据供分页调试"
            };
            resp = resp.on_hover_text(hover_text);
            if resp.clicked() {
                if let Some(tab) = editor_state.active_tab_mut() {
                    tab.debug_mode = !tab.debug_mode;
                    // 关闭 Debug 模式时立即释放服务端会话
                    if !tab.debug_mode {
                        release_debug_session_sync(tab);
                        tab.debug_preview = None;
                        tab.add_action_log(
                            "Debug 模式已关闭，服务端会话已释放".to_string(),
                            LogLevel::Info,
                        );
                    } else {
                        tab.add_action_log(
                            "Debug 模式已开启，下次执行将保留服务端数据".to_string(),
                            LogLevel::Info,
                        );
                    }
                }
            }
        }

        // ---- 执行 DAG（绿色三角形 ▶，主操作，置于末尾）----
        if has_tab {
            let running = editor_state.dag_exec_task.is_some();
            let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(btn_size, btn_size), Sense::click());
            let painter = ui.painter();
            if resp.hovered() && !running {
                painter.rect_filled(rect, 5.0, hover_bg);
            }
            // 绿色三角形（播放图标），强调「开始执行」语义；执行中略暗以示忙碌
            let run_color = if running {
                Color32::from_rgb(80, 180, 100)
            } else {
                Color32::from_rgb(46, 204, 113)
            };
            let cx = rect.center().x;
            let cy = rect.center().y;
            let h = 7.0; // 半高
            let w = 8.0; // 半宽
            let points = vec![
                Pos2::new(cx - w * 0.6, cy - h),
                Pos2::new(cx - w * 0.6, cy + h),
                Pos2::new(cx + w, cy),
            ];
            painter.add(egui::Shape::convex_polygon(points, run_color, Stroke::NONE));
            resp = resp.on_hover_text(if running { "正在执行 DAG..." } else { "执行 DAG（运行整张图）" });
            if resp.clicked() && !running {
                if let Some(tab) = editor_state.active_tab_mut() {
                    tab.pending_run_all = true;
                }
            }
        }

        // ---- 状态提示 ----
        if let Some(msg) = editor_state
            .active_tab()
            .and_then(|t| t.error_message.as_ref())
        {
            let is_error = msg.contains("失败") || msg.contains("错误");
            // 验证通过用浅蓝色，表示温和确认（区别于执行成功的绿色强确认）；
            // 验证失败的报文含「失败」字样，仍走红色错误分支
            let is_validation = msg.contains("验证");
            let color = if is_error {
                Color32::from_rgb(231, 76, 60)
            } else if is_validation {
                Color32::from_rgb(140, 200, 255)
            } else {
                Color32::from_rgb(80, 200, 120)
            };
            ui.add_space(8.0);
            let display: String = if msg.chars().count() > 16 {
                msg.chars().take(16).collect::<String>() + "…"
            } else {
                msg.clone()
            };
            ui.label(RichText::new(display).small().color(color));
        }
    });
}

/// 绘制小型垃圾桶图标（用于建模列表项的删除按钮）。
///
/// 以 `center` 为中心绘制把手 + 盖子 + 梯形桶身 + 中线，整体约 13px 高，
/// 与列表项 48px 高度协调。颜色由调用方决定（默认弱色、悬停红色）。
fn paint_trash_icon(painter: &egui::Painter, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.3, color);
    // 把手（顶部小拱）
    painter.line_segment(
        [Pos2::new(c.x - 2.5, c.y - 7.5), Pos2::new(c.x - 2.5, c.y - 6.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(c.x - 2.5, c.y - 6.0), Pos2::new(c.x + 2.5, c.y - 6.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(c.x + 2.5, c.y - 6.0), Pos2::new(c.x + 2.5, c.y - 7.5)],
        stroke,
    );
    // 盖子
    painter.line_segment(
        [Pos2::new(c.x - 5.0, c.y - 5.5), Pos2::new(c.x + 5.0, c.y - 5.5)],
        stroke,
    );
    // 桶身（梯形：上宽下窄）
    painter.line_segment(
        [Pos2::new(c.x - 4.0, c.y - 4.0), Pos2::new(c.x - 3.0, c.y + 5.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(c.x + 4.0, c.y - 4.0), Pos2::new(c.x + 3.0, c.y + 5.0)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(c.x - 3.0, c.y + 5.0), Pos2::new(c.x + 3.0, c.y + 5.0)],
        stroke,
    );
    // 桶身中线
    painter.line_segment(
        [Pos2::new(c.x, c.y - 3.0), Pos2::new(c.x, c.y + 3.5)],
        stroke,
    );
}

/// 左侧建模列表面板：新建按钮 + 磁盘历史列表，点击打开/切换，右键重命名/删除。
fn render_models_panel(ui: &mut Ui, editor_state: &mut DagEditorState) {
    ui.heading("建模列表");
    ui.add_space(4.0);
    if ui.button("+ 新建建模").clicked() {
        editor_state.show_new_model_dialog = true;
        editor_state.new_model_name_input = String::new();
    }
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    // 克隆列表，避免循环中长时间持有 editor_state.models 不可变借用
    let mut models = editor_state.models.clone();
    // 前端展示按名称排序，确保列表顺序稳定不随更新时间变动。
    // 以 name 为主键、id 为次键做全序排序，同名建模也不会因磁盘读取顺序而抖动。
    models.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    ScrollArea::vertical().show(ui, |ui| {
        if models.is_empty() {
            ui.label(RichText::new("暂无建模，点击「新建建模」创建").weak().small());
            ui.add_space(8.0);
        }
        for meta in &models {
            let is_open = editor_state.tabs.iter().any(|t| t.model_id == meta.id);
            let is_active = editor_state
                .active_tab()
                .map(|t| t.model_id == meta.id)
                .unwrap_or(false);

            let (rect, response) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), 48.0),
                egui::Sense::click(),
            );
            let painter = ui.painter();
            let bg = if is_active {
                // 选中态：实心蓝底，确保白色文字清晰无模糊
                super::theme::ACCENT
            } else if response.hovered() {
                Color32::from_rgba_unmultiplied(255, 255, 255, 10)
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 3)
            };

            if is_active {
                // 选中态：实心蓝底，文字直接绘制在不透明蓝上，清晰锐利
                painter.rect_filled(rect, 6.0, bg);
            } else {
                painter.rect_filled(rect, 6.0, bg);
            }

            // 名称：按宽度截断，右侧留出删除图标的空间避免重叠
            let name_font = egui::FontId::proportional(13.0);
            let name_max_w = (rect.width() - 38.0).max(40.0);
            let name_display = fit_text_to_width(ui, &meta.name, name_font.clone(), name_max_w);
            painter.text(
                Pos2::new(rect.min.x + 10.0, rect.min.y + 7.0),
                egui::Align2::LEFT_TOP,
                &name_display,
                name_font,
                Color32::WHITE,
            );
            let time_str = dag_store::format_timestamp(meta.updated_at);
            let sub = if is_open {
                format!("已打开 · {}", time_str)
            } else {
                time_str
            };
            let sub_color = if is_active {
                Color32::from_rgb(205, 205, 208)
            } else {
                Color32::from_rgb(140, 140, 140)
            };
            painter.text(
                Pos2::new(rect.min.x + 10.0, rect.min.y + 28.0),
                egui::Align2::LEFT_TOP,
                &sub,
                egui::FontId::proportional(11.0),
                sub_color,
            );

            // 右侧删除图标：常驻显示（弱色），悬停变红；点击弹出确认对话框
            let trash_c = Pos2::new(rect.right() - 14.0, rect.center().y);
            let trash_rect = Rect::from_center_size(trash_c, Vec2::splat(22.0));
            let trash_resp = ui.interact(
                trash_rect,
                ui.make_persistent_id(("model_delete_icon", meta.id.clone())),
                Sense::click(),
            );
            let trash_color = if trash_resp.hovered() {
                Color32::from_rgb(231, 76, 60)
            } else if is_active {
                Color32::from_rgb(220, 220, 224)
            } else if response.hovered() {
                Color32::from_rgb(180, 180, 180)
            } else {
                Color32::from_rgb(110, 110, 110)
            };
            let trash_clicked = trash_resp.clicked();
            // on_hover_cursor 消费 trash_resp（self），须在 clicked() 之后再调用
            trash_resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            paint_trash_icon(painter, trash_c, trash_color);
            if trash_clicked {
                editor_state.request_delete_model(&meta.id, &meta.name);
            }
            // 点击列表项体（非删除图标）：打开或切换到该建模
            if response.clicked() && !trash_clicked {
                if let Some(pos) = editor_state.find_tab_by_model(&meta.id) {
                    editor_state.switch_to_tab(pos);
                } else if let Some(rec) = dag_store::load_model(&meta.id) {
                    editor_state.open_model(rec);
                }
            }
            response.context_menu(|ui| {
                if ui.button("重命名").clicked() {
                    ui.close_menu();
                    editor_state.rename_target_id = Some(meta.id.clone());
                    editor_state.rename_input = meta.name.clone();
                }
                if ui.button("删除").clicked() {
                    ui.close_menu();
                    editor_state.request_delete_model(&meta.id, &meta.name);
                }
            });
            ui.add_space(4.0);
        }
    });
}

fn render_operator_panel(ui: &mut Ui, tab: &mut DagTab) {
    ui.add(
        egui::TextEdit::singleline(&mut tab.operator_search_filter)
            .hint_text("搜索算子...")
            .frame(false)
            .margin(Vec2::new(6.0, 4.0))
            .desired_width(ui.available_width())
    );
    ui.separator();

    // 获取层级化算子分类
    let categories = get_operator_categories();

    // 如果有分类，按分类展示
    if !categories.is_empty() {
        ScrollArea::vertical().show(ui, |ui| {
            let filter = tab.operator_search_filter.to_lowercase();
            let has_filter = !filter.is_empty();

            // 搜索模式：扁平化显示所有匹配的算子
            if has_filter {
                let all_ops = get_all_operator_types();
                let filtered: Vec<_> = all_ops
                    .into_iter()
                    .filter(|op| {
                        op.name().to_lowercase().contains(&filter)
                            || op.description().to_lowercase().contains(&filter)
                    })
                    .collect();

                if filtered.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("未找到匹配的算子").weak().small());
                    });
                } else {
                    for op_type in filtered {
                        render_operator_card(ui, op_type, tab);
                        ui.add_space(2.0);
                    }
                }
            } else {
                // 层级模式：按分类树展示
                for category in &categories {
                    render_category_tree(ui, category, tab);
                    ui.add_space(2.0);
                }
            }
        });
    } else {
        // 无分类时回退到扁平列表
        let all_ops = get_all_operator_types();

        ScrollArea::vertical().show(ui, |ui| {
            let filtered_ops: Vec<_> = if tab.operator_search_filter.is_empty() {
                all_ops
            } else {
                let filter = tab.operator_search_filter.to_lowercase();
                all_ops.into_iter()
                    .filter(|op| op.name().to_lowercase().contains(&filter)
                        || op.description().to_lowercase().contains(&filter))
                    .collect()
            };

            if filtered_ops.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("未找到匹配的算子").weak().small());
                });
            } else {
                for op_type in filtered_ops {
                    render_operator_card(ui, op_type, tab);
                    ui.add_space(2.0);
                }
            }
        });
    }

    ui.separator();
    ui.add_space(4.0);

    ui.label(RichText::new("拖拽添加 · 点击中心添加 · Del 删除节点").small().weak());

    if let Some(msg) = &tab.error_message {
        let is_error = msg.contains("失败") || msg.contains("错误");
        // 与工具栏状态提示配色一致：验证通过用浅蓝（温和确认），其余成功用绿色
        let is_validation = msg.contains("验证");
        let color = if is_error {
            Color32::from_rgb(231, 76, 60)
        } else if is_validation {
            Color32::from_rgb(140, 200, 255)
        } else {
            Color32::from_rgb(80, 200, 120)
        };
        ui.colored_label(color, msg);
    }
}

/// 递归统计分类下（含全部子分类）的算子总数，用于目录头右侧的计数徽章。
fn count_operators_recursive(category: &OperatorCategory) -> usize {
    category.operators.len()
        + category
            .subcategories
            .iter()
            .map(count_operators_recursive)
            .sum::<usize>()
}

/// 递归渲染算子分类树（支持任意深度的子分类）。
///
/// 采用自定义目录头以强化「文件夹」语义：左侧旋转箭头 + 文件夹图标 + 分类名，
/// 右侧计数徽章；默认折叠，点击整行切换展开。约定 `name` 为空的分类是「根算子」
/// 容器（lib_dir 直接下的算子），不渲染目录头，直接平铺算子卡片。
fn render_category_tree(ui: &mut Ui, category: &OperatorCategory, tab: &mut DagTab) {
    // 空名分类：根算子，直接平铺，不渲染目录头
    if category.name.is_empty() && category.subcategories.is_empty() {
        render_operators_filtered(ui, &category.operators, tab);
        return;
    }

    // 目录头持久化 id：用「分类名 + 算子数 + 子分类数」组合，避免同名分类冲突
    let category_id = format!(
        "cat_{}_{}_{}",
        category.name,
        category.operators.len(),
        category.subcategories.len()
    );
    let id = ui.make_persistent_id(&category_id);
    // 默认折叠
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);

    // —— 自定义目录头（整行可点击切换）——
    let header_h = 26.0;
    let (header_rect, header_resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), header_h),
        egui::Sense::click(),
    );
    if header_resp.clicked() {
        state.toggle(ui);
    }
    let openness = state.openness(ui.ctx());
    let is_open = openness > 0.5;
    let hovered = header_resp.hovered();

    let painter = ui.painter();
    // 行背景：悬停 / 展开时轻微抬升，强化目录层级感
    let bg_alpha = if is_open { 16 } else if hovered { 12 } else { 0 };
    if bg_alpha > 0 {
        painter.add(egui::Shape::rect_filled(
            header_rect,
            super::theme::WIDGET_ROUNDING,
            Color32::from_rgba_unmultiplied(255, 255, 255, bg_alpha),
        ));
    }

    // 旋转箭头：折叠时指向右，展开时指向下，随 openness 平滑旋转
    let arrow_center = egui::pos2(header_rect.min.x + 10.0, header_rect.center().y);
    let arrow_color = if hovered || is_open {
        super::theme::TEXT_HOVER
    } else {
        super::theme::TEXT_WEAK
    };
    paint_chevron(painter, arrow_center, openness, arrow_color);

    // 分类名
    let name_color = if hovered || is_open {
        super::theme::TEXT_STRONG
    } else {
        super::theme::TEXT_HOVER
    };
    painter.text(
        egui::pos2(header_rect.min.x + 22.0, header_rect.center().y),
        egui::Align2::LEFT_CENTER,
        &category.name,
        egui::FontId::proportional(13.0),
        name_color,
    );

    // 右侧计数徽章：显示该分类（含子分类）下的算子总数
    let count = count_operators_recursive(category);
    let count_text = count.to_string();
    let badge_font = egui::FontId::proportional(10.0);
    let text_w = ui.fonts(|f| {
        f.layout_no_wrap(count_text.clone(), badge_font.clone(), Color32::WHITE)
            .size()
            .x
    });
    let badge_w = text_w.max(8.0) + 8.0;
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(
            header_rect.max.x - badge_w - 6.0,
            header_rect.center().y - 7.0,
        ),
        Vec2::new(badge_w, 14.0),
    );
    painter.add(egui::Shape::rect_filled(
        badge_rect,
        7.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, 14),
    ));
    painter.text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        count_text.as_str(),
        badge_font,
        super::theme::TEXT_WEAK,
    );

    // —— 展开内容（含子分类递归）；show_body_indented 自动缩进体现层级 ——
    state.show_body_indented(&header_resp, ui, |ui| {
        render_operators_filtered(ui, &category.operators, tab);
        for subcat in &category.subcategories {
            render_category_tree(ui, subcat, tab);
            ui.add_space(1.0);
        }
    });
}

/// 绘制旋转箭头：openness=0 指向右（折叠），openness=1 指向下（展开）。
fn paint_chevron(painter: &egui::Painter, center: Pos2, openness: f32, color: Color32) {
    let s = 3.5;
    let mut points = vec![
        egui::pos2(center.x - s, center.y - s),
        egui::pos2(center.x + s, center.y),
        egui::pos2(center.x - s, center.y + s),
    ];
    let rotation = egui::emath::Rot2::from_angle(egui::remap(
        openness,
        0.0..=1.0,
        0.0..=std::f32::consts::TAU / 4.0,
    ));
    for p in &mut points {
        *p = center + rotation * (*p - center);
    }
    painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
}

/// 按搜索过滤渲染算子列表
fn render_operators_filtered(
    ui: &mut Ui,
    ops: &[operator_executor_client::protocol::OperatorInfo],
    tab: &mut DagTab,
) {
    let filter = tab.operator_search_filter.to_lowercase();
    let has_filter = !filter.is_empty();
    for op_info in ops {
        let op_type = crate::dag::operator_info_to_type(op_info);
        if !has_filter
            || op_type.name().to_lowercase().contains(&filter)
            || op_type.description().to_lowercase().contains(&filter)
            || op_type.summary().to_lowercase().contains(&filter)
        {
            render_operator_card(ui, op_type, tab);
            ui.add_space(2.0);
        }
    }
}

/// 渲染单个算子卡片。
///
/// 当算子有摘要（`summary`）时采用两行布局：第一行为算子名称，第二行为摘要文字；
/// 无摘要时回退到原来的紧凑单行布局。摘要文字会按可用宽度截断并加省略号。
fn render_operator_card(ui: &mut Ui, op_type: OperatorType, tab: &mut DagTab) {
    let op_color = op_type.color();
    let input_count = op_type.input_defs().len();
    let output_count = op_type.output_defs().len();
    let summary = op_type.summary();
    let has_summary = !summary.is_empty();

    let row_height = if has_summary { 46.0 } else { 32.0 };
    let (card_rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), row_height),
        egui::Sense::click_and_drag()
    );

    let painter = ui.painter();
    if response.hovered() {
        painter.add(egui::Shape::rect_filled(
            card_rect,
            4.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
        ));
        // 左侧彩色边条
        painter.add(egui::Shape::rect_filled(
            egui::Rect::from_min_max(
                card_rect.min,
                egui::Pos2::new(card_rect.min.x + 3.0, card_rect.max.y),
            ),
            0.0,
            op_color,
        ));
    }

    // 第一行基准 y（名称与颜色点对齐到此行中心）
    let first_line_y = if has_summary {
        card_rect.min.y + 15.0
    } else {
        card_rect.center().y
    };

    // 左侧颜色指示点
    let dot_rect = egui::Rect::from_center_size(
        egui::Pos2::new(card_rect.min.x + 12.0, first_line_y),
        egui::Vec2::new(8.0, 8.0),
    );
    painter.add(egui::Shape::rect_filled(dot_rect, 2.0, op_color));

    // 算子名称（第一行）
    let name_font = egui::FontId::proportional(13.0);
    painter.text(
        egui::Pos2::new(card_rect.min.x + 26.0, first_line_y),
        egui::Align2::LEFT_CENTER,
        op_type.name(),
        name_font,
        egui::Color32::from_rgb(230, 230, 230),
    );

    // 端口标签（右侧，垂直居中于整张卡片）
    let port_info = if input_count > 0 || output_count > 0 {
        format!("{}入{}出", input_count, output_count)
    } else {
        "无端口".to_string()
    };

    let tag_font = egui::FontId::proportional(10.0);
    let tag_w = 52.0;
    let tag_rect = egui::Rect::from_min_max(
        egui::Pos2::new(card_rect.max.x - tag_w - 6.0, card_rect.min.y + 4.0),
        egui::Pos2::new(card_rect.max.x - 6.0, card_rect.max.y - 4.0),
    );
    painter.add(egui::Shape::rect_filled(
        tag_rect,
        3.0,
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6),
    ));
    painter.text(
        tag_rect.center(),
        egui::Align2::CENTER_CENTER,
        port_info,
        tag_font,
        egui::Color32::from_rgb(130, 130, 130),
    );

    // 摘要（第二行，小号弱色，按宽度截断）
    if has_summary {
        let sum_font = egui::FontId::proportional(11.0);
        // 可用宽度：从名称起点到端口标签左侧之间
        let sum_max_w = (tag_rect.min.x - 8.0) - (card_rect.min.x + 26.0);
        let fitted = fit_text_to_width(ui, summary, sum_font.clone(), sum_max_w);
        painter.text(
            egui::Pos2::new(card_rect.min.x + 26.0, card_rect.min.y + 33.0),
            egui::Align2::LEFT_CENTER,
            fitted,
            sum_font,
            egui::Color32::from_rgb(150, 150, 150),
        );
    }

    // 点击: 在画布可视区中心添加节点
    if response.clicked() {
        let center = canvas_viewport_center(tab);
        let new_node = Node::new(op_type.clone(), center);
        tab.graph.add_node(new_node);
        tab.error_message = None;
        tab.dirty = true;
    }

    // 开始拖拽: 记录算子类型
    if response.dragged() && tab.dragging_operator.is_none() {
        tab.dragging_operator = Some(op_type.clone());
    }

    // 悬停提示：显示完整描述（与卡片上的摘要互补）
    let desc = op_type.description();
    if !desc.is_empty() {
        response.on_hover_text(desc);
    }
}

/// 将文本按指定字体截断到不超过 `max_w` 的宽度，超出部分以省略号 `…` 收尾。
///
/// 用于算子卡片摘要等需要单行展示且宽度受限的场景。宽度不足时返回空字符串。
fn fit_text_to_width(ui: &Ui, text: &str, font_id: egui::FontId, max_w: f32) -> String {
    if max_w <= 8.0 {
        return String::new();
    }
    let measure = |t: &str| -> f32 {
        ui.fonts(|f| {
            f.layout_no_wrap(t.into(), font_id.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
    };
    if measure(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let ellipsis = "…";
    // 二分查找最长可放下的前缀（带省略号）
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + ellipsis;
        if measure(&candidate) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        ellipsis.to_string()
    } else {
        chars[..lo].iter().collect::<String>() + ellipsis
    }
}

/// 计算画布可视区域中心在画布坐标系下的位置.
/// 依赖上一帧画布刷新的 `canvas_viewport_rect`; 若尚无 (首帧) 则回退到 (100, 100).
fn canvas_viewport_center(tab: &DagTab) -> Vec2 {
    if let Some(rect) = tab.canvas_viewport_rect {
        (rect.center() - rect.min) / tab.canvas_zoom - tab.canvas_offset
    } else {
        Vec2::new(100.0, 100.0)
    }
}

/// 渲染运行日志面板。执行入口已移至画布浮动工具栏的绿色三角形按钮。
///
/// 面板分为三个子标签页：
/// - **提醒**：用户点击保存、验证、清空等 UI 操作的直接反馈；
/// - **算子运行**：服务端 DAG 执行进度与节点结果回填日志；
/// - **通信报文**：客户端 ↔ 服务端的 JSON 请求 / 响应原文，便于排查协议问题。
fn render_run_log_panel(ui: &mut Ui, tab: &mut DagTab) {
    // ===== 顶部工具栏 =====
    ui.horizontal(|ui| {
        ui.heading("运行日志");
        ui.add_space(16.0);

        if ui.button("重置执行状态").clicked() {
            tab.io_registry.clear();
            tab.add_action_log(
                "已重置所有节点执行状态，下次执行将重新计算".to_string(),
                LogLevel::Info,
            );
        }

        ui.add_space(8.0);

        // 仅清空当前激活子页的日志，避免误清其他类别
        let clear_label = match tab.active_log_category {
            LogCategory::Action => "清空提醒",
            LogCategory::Runtime => "清空运行",
            LogCategory::Json => "清空报文",
        };
        if ui.button(clear_label).clicked() {
            tab.clear_active_logs();
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (a, r, j) = (
                tab.action_logs.len(),
                tab.runtime_logs.len(),
                tab.json_logs.len(),
            );
            ui.label(
                RichText::new(format!("提醒 {} · 算子运行 {} · 通信 {}", a, r, j))
                    .small()
                    .weak(),
            );
        });
    });

    ui.separator();

    // ===== 子标签栏：三类日志分别计数 =====
    ui.horizontal(|ui| {
        let n_action = tab.action_logs.len();
        let n_runtime = tab.runtime_logs.len();
        let n_json = tab.json_logs.len();
        ui.selectable_value(
            &mut tab.active_log_category,
            LogCategory::Action,
            RichText::new(format!("提醒 ({})", n_action)).strong(),
        );
        ui.selectable_value(
            &mut tab.active_log_category,
            LogCategory::Runtime,
            RichText::new(format!("算子运行 ({})", n_runtime)).strong(),
        );
        ui.selectable_value(
            &mut tab.active_log_category,
            LogCategory::Json,
            RichText::new(format!("通信报文 ({})", n_json)).strong(),
        );
    });

    ui.separator();

    // ===== 日志内容：按激活分类渲染 =====
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match tab.active_log_category {
            LogCategory::Action => render_text_logs(
                ui,
                &tab.action_logs,
                "暂无提醒日志（保存、验证、清空等操作反馈将显示在这里）",
            ),
            LogCategory::Runtime => render_text_logs(
                ui,
                &tab.runtime_logs,
                "暂无算子运行日志，点击「执行 DAG」开始运行",
            ),
            LogCategory::Json => render_json_logs(ui, &tab.json_logs),
        });
}

/// 渲染纯文本日志（提醒 / 算子运行两类共用）：时间 + 级别前缀 + 消息，按级别着色。
fn render_text_logs(ui: &mut Ui, logs: &[RunLogEntry], empty_hint: &str) {
    if logs.is_empty() {
        ui.label(RichText::new(empty_hint).weak().small());
        return;
    }
    for log in logs {
        let color = match log.level {
            LogLevel::Info => Color32::WHITE,
            LogLevel::Success => Color32::from_rgb(46, 204, 113),
            LogLevel::Warning => Color32::from_rgb(241, 196, 15),
            LogLevel::Error => Color32::from_rgb(231, 76, 60),
        };
        let level_prefix = match log.level {
            LogLevel::Info => "[INFO] ",
            LogLevel::Success => "[OK] ",
            LogLevel::Warning => "[WARN] ",
            LogLevel::Error => "[ERR] ",
        };
        ui.label(
            RichText::new(format!("{} {} {}", log.timestamp, level_prefix, log.message))
                .font(egui::FontId::monospace(11.0))
                .color(color),
        );
    }
}

/// 渲染 JSON 通信报文：倒序展示（最新在最上方），每条用折叠头 + 等宽 JSON 正文。
///
/// 头部包含方向箭头（↑ 请求 / ↓ 响应）、时间、标题与字节数；展开后显示美化 JSON，
/// 并提供「复制」按钮把原文写入系统剪贴板，便于外部排查。
fn render_json_logs(ui: &mut Ui, logs: &[JsonLogEntry]) {
    if logs.is_empty() {
        ui.label(RichText::new("暂无通信报文，执行 DAG 后将记录请求与响应的 JSON").weak().small());
        return;
    }
    for (i, log) in logs.iter().enumerate().rev() {
        let (arrow, dir_text, dir_color) = match log.direction {
            JsonDirection::Send => ("↑", "请求", Color32::from_rgb(56, 130, 245)),
            JsonDirection::Receive => ("↓", "响应", Color32::from_rgb(46, 204, 113)),
        };
        let header = format!(
            "{} {} · {} · {} 字节",
            arrow,
            log.timestamp,
            log.title,
            log.payload.len()
        );
        let header_id = format!("json_log_{}_{}", i, log.timestamp);
        egui::CollapsingHeader::new(
            RichText::new(header)
                .color(dir_color)
                .font(egui::FontId::proportional(12.0)),
        )
        .id_source(header_id)
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("[{}]", dir_text))
                        .small()
                        .color(dir_color),
                );
                if ui.button("复制").clicked() {
                    ui.ctx().copy_text(log.payload.clone());
                }
            });
            ui.label(
                RichText::new(log.payload.as_str())
                    .font(egui::FontId::monospace(11.0))
                    .color(Color32::from_rgb(210, 210, 210)),
            );
        });
    }
}

/// 新建 / 重命名建模对话框。
fn render_dialogs(ui: &mut Ui, editor_state: &mut DagEditorState) {
    // 新建建模对话框
    if editor_state.show_new_model_dialog {
        let mut open = true;
        egui::Window::new("新建建模")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                ui.label("请输入建模名称：");
                ui.add_space(4.0);
                ui.text_edit_singleline(&mut editor_state.new_model_name_input);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let name = editor_state.new_model_name_input.trim().to_string();
                    if ui.add_enabled(!name.is_empty(), egui::Button::new("确定")).clicked() {
                        editor_state.create_model(&name);
                        editor_state.show_new_model_dialog = false;
                    }
                    if ui.button("取消").clicked() {
                        editor_state.show_new_model_dialog = false;
                    }
                });
            });
        if !open {
            editor_state.show_new_model_dialog = false;
        }
    }

    // 重命名对话框
    if let Some(target_id) = editor_state.rename_target_id.clone() {
        let mut open = true;
        egui::Window::new("重命名建模")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                ui.label("请输入新的建模名称：");
                ui.add_space(4.0);
                ui.text_edit_singleline(&mut editor_state.rename_input);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let name = editor_state.rename_input.trim().to_string();
                    if ui.add_enabled(!name.is_empty(), egui::Button::new("确定")).clicked() {
                        editor_state.rename_model(&target_id, &name);
                        editor_state.rename_target_id = None;
                    }
                    if ui.button("取消").clicked() {
                        editor_state.rename_target_id = None;
                    }
                });
            });
        if !open {
            editor_state.rename_target_id = None;
        }
    }

    // 清空确认对话框
    if editor_state.show_clear_confirm_dialog {
        let mut open = true;
        egui::Window::new("确认清空")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                ui.label("确定要清空当前建模的所有节点和连线吗？");
                ui.label(RichText::new("此操作不可撤销。").weak().small());
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(
                        editor_state.active_tab_index.is_some(),
                        egui::Button::new("确认清空")
                    ).clicked() {
                        if let Some(tab) = editor_state.active_tab_mut() {
                            tab.graph = crate::dag::DagGraph::new();
                            tab.selected_node_id = None;
                            tab.error_message = None;
                            tab.io_registry.clear();
                            tab.dirty = true;
                            tab.add_action_log("已清空当前建模".to_string(), LogLevel::Info);
                        }
                        editor_state.show_clear_confirm_dialog = false;
                    }
                    if ui.button("取消").clicked() {
                        editor_state.show_clear_confirm_dialog = false;
                    }
                });
            });
        if !open {
            editor_state.show_clear_confirm_dialog = false;
        }
    }

    // 删除建模确认对话框
    if editor_state.show_delete_model_dialog {
        let mut open = true;
        let target_name = editor_state
            .delete_model_target_name
            .clone()
            .unwrap_or_else(|| "未命名".to_string());
        egui::Window::new("确认删除建模")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                ui.label(format!("确定要删除建模「{}」吗？", target_name));
                ui.label(
                    RichText::new("删除后将从列表移除；磁盘文件改名为 .deleted 保留，可手动改回 .json 恢复。")
                        .weak()
                        .small(),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("确认删除").clicked() {
                        if let Some(id) = editor_state.delete_model_target_id.take() {
                            // 删除建模前释放对应 tab 持有的 Debug 会话
                            if let Some(pos) = editor_state.find_tab_by_model(&id) {
                                if let Some(tab) = editor_state.tabs.get_mut(pos) {
                                    release_debug_session_sync(tab);
                                }
                            }
                            editor_state.delete_model(&id);
                        }
                        editor_state.show_delete_model_dialog = false;
                        editor_state.delete_model_target_name = None;
                    }
                    if ui.button("取消").clicked() {
                        editor_state.show_delete_model_dialog = false;
                        editor_state.delete_model_target_id = None;
                        editor_state.delete_model_target_name = None;
                    }
                });
            });
        if !open {
            editor_state.show_delete_model_dialog = false;
            editor_state.delete_model_target_id = None;
            editor_state.delete_model_target_name = None;
        }
    }
}

/// 生成带本地时间（UTC+8）的 DAG 流程名，用于落盘文件名和日志展示。
fn format_dag_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let total = secs + 8 * 3600;
    let hh = (total / 3600) % 24;
    let mm = (total / 60) % 60;
    let ss = total % 60;
    format!("mining_{:02}{:02}{:02}", hh, mm, ss)
}

/// 同步释放 tab 当前持有的 Debug 会话（若存在）。
///
/// 在 UI 线程调用：通过全局 `RUNTIME_CLIENT` 发送 `EndDebugSession`，服务端释放
/// 对应会话内存。失败仅打印日志（best-effort）——会话 ID 在客户端会被无条件清空，
/// 即便服务端没收到释放请求，下次同 ID 会话也会被 `begin_debug_session` 覆盖。
fn release_debug_session_sync(tab: &mut DagTab) {
    if let Some(sid) = tab.debug_session_id.take() {
        // 走全局 runtime client 的 best-effort 释放；连接断开等错误仅日志记录
        let _ = crate::operator_executor::with_runtime_client(|client| {
            client.end_debug_session(&sid)
        });
    }
}

/// 释放所有 tab 持有的 Debug 会话。
///
/// 用于切换视图（离开挖掘分析页）/ 应用退出等场景，避免服务端内存泄漏。
/// 失败仅打印日志，不阻断调用方流程。
pub fn release_all_debug_sessions(editor_state: &mut DagEditorState) {
    for tab in &mut editor_state.tabs {
        release_debug_session_sync(tab);
    }
}

/// 启动后台执行整张 DAG（「执行 DAG」按钮入口）。
///
/// 主线程仅做轻量准备（日志、重置 registry、克隆 graph、建通道），阻塞的
/// TCP 调用交给工作线程；结果通过 mpsc 通道回传，由 [`poll_dag_exec_task`]
/// 每帧轮询并回填，避免阻塞 egui 主线程导致界面卡顿。
fn spawn_run_all(ctx: &Context, editor_state: &mut DagEditorState) {
    // 全局只允许一个执行任务（runtime 客户端为全局 Mutex）
    if editor_state.dag_exec_task.is_some() {
        if let Some(tab) = editor_state.active_tab_mut() {
            tab.add_action_log("已有任务在执行中，请等待完成".to_string(), LogLevel::Warning);
        }
        return;
    }

    let (graph_clone, model_id, dag_name, debug_session_id) = {
        let tab = match editor_state.active_tab_mut() {
            Some(t) => t,
            None => return,
        };
        tab.add_runtime_log("开始执行 DAG...".to_string(), LogLevel::Info);
        let node_count = tab.graph.nodes.len();
        if node_count == 0 {
            tab.add_runtime_log("DAG 为空，无节点可执行".to_string(), LogLevel::Warning);
            return;
        }
        tab.add_runtime_log(format!("DAG 包含 {} 个节点", node_count), LogLevel::Info);
        // 服务端会重新执行整个流程，先重置本地执行状态，避免残留缓存与新结果不一致
        tab.io_registry.clear();
        let dag_name = format_dag_name();

        // Debug 模式：生成新会话 ID，先释放旧会话（若存在）。
        // 旧会话用同步 TCP 调用释放（快），新会话 ID 存入 tab 供预览窗口查询。
        let debug_session_id = if tab.debug_mode {
            release_debug_session_sync(tab);
            // 清空旧预览状态，下次打开预览时重新查询新会话的 meta
            tab.debug_preview = None;
            let sid = uuid::Uuid::new_v4().to_string();
            tab.debug_session_id = Some(sid.clone());
            tab.add_runtime_log(
                format!("Debug 模式已启用，会话 ID: {}", sid),
                LogLevel::Info,
            );
            Some(sid)
        } else {
            // 非 Debug 模式也清理可能残留的旧会话
            release_debug_session_sync(tab);
            None
        };

        // 记录即将下发到服务端的 DAG 定义 JSON（请求方向），便于排查协议问题。
        // 在 UI 线程内构造一次报文快照，工作线程仍走 execute_dag_on_server 自带的下发流程，
        // 避免修改 operator_executor 的接口；构造为纯内存操作，开销可忽略。
        let dag_def = crate::operator_executor::build_dag_definition(&tab.graph, &dag_name);
        let dag_json = serde_json::to_string_pretty(&dag_def).unwrap_or_else(|_| {
            serde_json::to_string(&dag_def).unwrap_or_else(|_| "<序列化失败>".to_string())
        });
        tab.add_json_log(
            JsonDirection::Send,
            "下发 DAG 执行请求".to_string(),
            dag_json,
        );

        tab.add_runtime_log("已将流程下发到服务端解析执行".to_string(), LogLevel::Info);
        (tab.graph.clone(), tab.model_id.clone(), dag_name, debug_session_id)
    };

    let (tx, rx) = mpsc::channel::<DagExecMessage>();
    let ctx_clone = ctx.clone();

    std::thread::spawn(move || {
        // 阻塞的 TCP 下发 + 服务端执行在此工作线程完成；流式接收每个节点进度 + chunk
        let res = crate::operator_executor::execute_dag_on_server_streaming_debug(
            &graph_clone,
            &dag_name,
            debug_session_id.as_deref(),
            |p| {
                // 每条进度立即推给 UI 线程并唤醒重绘，实现「运行到哪个算子」的实时反馈
                let _ = tx.send(DagExecMessage::NodeProgress(p.clone()));
                ctx_clone.request_repaint();
            },
            |node_id, chunk| {
                // 流式 chunk（如 chat DSL 快照）：落盘预览缓存供聊天预览窗口逐 token 刷新
                let node_name = graph_clone
                    .get_node(node_id)
                    .map(|n| n.operator_type.name().to_string())
                    .unwrap_or_else(|| node_id.to_string());
                if let Err(e) = crate::data_preview::save_preview_from_truncated(
                    node_id,
                    &node_name,
                    std::slice::from_ref(chunk),
                    0,
                ) {
                    eprintln!("流式 chunk 缓存失败 (节点 {}): {}", node_id, e);
                }
                let _ = tx.send(DagExecMessage::StreamChunk {
                    node_id: node_id.to_string(),
                    chunk: chunk.clone(),
                });
                ctx_clone.request_repaint();
            },
        );
        // 发送最终结果；忽略发送错误（UI 端已丢弃任务时 channel 已关闭）
        let _ = tx.send(DagExecMessage::Finished(res));
        // 唤醒 UI 线程轮询结果
        ctx_clone.request_repaint();
    });

    editor_state.dag_exec_task = Some(DagExecTask {
        kind: DagExecKind::RunAll,
        rx,
        model_id,
    });
}

/// 启动后台「运行到此结点」（右键菜单入口）。
///
/// 克隆 graph 交给工作线程执行目标节点的上游子图；结果回传后由
/// [`poll_dag_exec_task`] 调用 [`crate::operator_executor::apply_dag_execution_result`]
/// 回填 registry。
pub fn spawn_run_up_to(
    ctx: &Context,
    editor_state: &mut DagEditorState,
    target_node_id: &str,
) {
    if editor_state.dag_exec_task.is_some() {
        if let Some(tab) = editor_state.active_tab_mut() {
            tab.add_action_log("已有任务在执行中，请等待完成".to_string(), LogLevel::Warning);
        }
        return;
    }

    let (graph_clone, model_id, target, debug_session_id) = {
        let tab = match editor_state.active_tab_mut() {
            Some(t) => t,
            None => return,
        };
        tab.add_runtime_log(format!("开始运行到节点 {}...", target_node_id), LogLevel::Info);

        // Debug 模式：生成新会话 ID，先释放旧会话（若存在）。
        let debug_session_id = if tab.debug_mode {
            release_debug_session_sync(tab);
            // 清空旧预览状态，下次打开预览时重新查询新会话的 meta
            tab.debug_preview = None;
            let sid = uuid::Uuid::new_v4().to_string();
            tab.debug_session_id = Some(sid.clone());
            tab.add_runtime_log(
                format!("Debug 模式已启用，会话 ID: {}", sid),
                LogLevel::Info,
            );
            Some(sid)
        } else {
            release_debug_session_sync(tab);
            None
        };

        // 记录即将下发的子图 DAG 定义 JSON（请求方向）。
        // 与 execute_dag_up_to_detached 内部构造保持一致：取目标节点的上游子图（含自身）。
        // 这里在 UI 线程做一次纯内存构造用于日志展示，不触碰服务端调用路径。
        match tab.graph.get_ancestors(target_node_id) {
            Ok(ancestors) => {
                let ancestor_set: std::collections::HashSet<String> =
                    ancestors.iter().cloned().collect();
                let dag_name = format!("upto_{}", target_node_id);
                let subset = crate::operator_executor::build_dag_definition_subset(
                    &tab.graph,
                    &dag_name,
                    &ancestor_set,
                );
                let subset_json = serde_json::to_string_pretty(&subset).unwrap_or_else(|_| {
                    serde_json::to_string(&subset)
                        .unwrap_or_else(|_| "<序列化失败>".to_string())
                });
                tab.add_json_log(
                    JsonDirection::Send,
                    format!("下发子图执行请求（运行到节点 {}）", target_node_id),
                    subset_json,
                );
            }
            Err(e) => {
                tab.add_runtime_log(
                    format!("构造子图失败，跳过请求报文记录: {}", e),
                    LogLevel::Warning,
                );
            }
        }

        (tab.graph.clone(), tab.model_id.clone(), target_node_id.to_string(), debug_session_id)
    };

    let (tx, rx) = mpsc::channel::<DagExecMessage>();
    let ctx_clone = ctx.clone();
    let target_for_thread = target.clone();

    std::thread::spawn(move || {
        let res = crate::operator_executor::execute_dag_up_to_detached_streaming_debug(
            &graph_clone,
            &target_for_thread,
            debug_session_id.as_deref(),
            |p| {
                let _ = tx.send(DagExecMessage::NodeProgress(p.clone()));
                ctx_clone.request_repaint();
            },
            |node_id, chunk| {
                // 流式 chunk（如 chat DSL 快照）：落盘预览缓存供聊天预览窗口逐 token 刷新
                let node_name = graph_clone
                    .get_node(node_id)
                    .map(|n| n.operator_type.name().to_string())
                    .unwrap_or_else(|| node_id.to_string());
                if let Err(e) = crate::data_preview::save_preview_from_truncated(
                    node_id,
                    &node_name,
                    std::slice::from_ref(chunk),
                    0,
                ) {
                    eprintln!("流式 chunk 缓存失败 (节点 {}): {}", node_id, e);
                }
                let _ = tx.send(DagExecMessage::StreamChunk {
                    node_id: node_id.to_string(),
                    chunk: chunk.clone(),
                });
                ctx_clone.request_repaint();
            },
        );
        let _ = tx.send(DagExecMessage::Finished(res));
        ctx_clone.request_repaint();
    });

    editor_state.dag_exec_task = Some(DagExecTask {
        kind: DagExecKind::RunUpTo { target_node_id: target },
        rx,
        model_id,
    });
}

/// 将「执行 DAG」的最终结果回填到发起任务的 tab 的 registry / 预览缓存 / 日志。
///
/// 按 `model_id` 定位 tab：即便用户在执行期间切换到别的 tab，结果也回填到发起
/// 任务的那个 tab；若该 tab 已被关闭，结果丢弃并打印日志。
fn apply_run_all_result(editor_state: &mut DagEditorState, model_id: &str, result: DagExecutionResult) {
    let tab_idx = match editor_state.find_tab_by_model(model_id) {
        Some(i) => i,
        None => {
            eprintln!("DAG 执行完成但发起的 tab 已关闭 (model_id={})", model_id);
            return;
        }
    };

    // 先把服务端返回的完整执行结果序列化为 JSON（响应方向）记入通信报文日志。
    // 在可变借用 tab 之前完成序列化，避免借用冲突。
    let result_json = serde_json::to_string_pretty(&result).unwrap_or_else(|_| {
        serde_json::to_string(&result).unwrap_or_else(|_| "<序列化失败>".to_string())
    });
    let tab = &mut editor_state.tabs[tab_idx];
    tab.add_json_log(
        JsonDirection::Receive,
        "DAG 执行结果".to_string(),
        result_json,
    );

    for nr in &result.node_results {
        // 去重：已通过进度帧回填过终态的节点跳过，避免重复落盘预览和重复日志
        if matches!(
            tab.io_registry.get_status(&nr.node_id),
            OperatorExecutionStatus::Completed | OperatorExecutionStatus::Failed
        ) {
            continue;
        }
        match nr.execution_result.status {
            OperatorExecutionStatus::Completed => {
                let output_len = nr.outputs.len();
                // 落盘预览缓存（服务端已截断的 outputs + 真实行数），失败不影响整体结果
                if let Err(e) = crate::data_preview::save_preview_from_truncated(
                    &nr.node_id,
                    &nr.operator_name,
                    &nr.outputs,
                    nr.output_row_count,
                ) {
                    eprintln!("缓存预览数据失败 (节点 {}): {}", nr.node_id, e);
                }
                tab.io_registry.set_result(
                    &nr.node_id,
                    Vec::new(),
                    nr.outputs.clone(),
                    nr.execution_result.clone(),
                );
                // 算子真正执行过才会有 duration_ms（服务端 completed() 必带）；
                // 用 Option 兜底，避免协议字段缺失时崩溃。
                let duration_part = nr.execution_result.duration_ms
                    .map(|ms| format!("，耗时 {} ms", ms))
                    .unwrap_or_default();
                tab.add_runtime_log(
                    format!("节点 {} ({}) 执行成功，输出端口数: {}{}", nr.node_id, nr.operator_name, output_len, duration_part),
                    LogLevel::Success,
                );
            }
            OperatorExecutionStatus::Failed => {
                let error_msg = nr.execution_result.error_message.clone()
                    .unwrap_or_else(|| "执行失败（未知原因）".to_string());
                tab.io_registry.set_failed(
                    &nr.node_id,
                    Vec::new(),
                    nr.execution_result.clone(),
                );
                // 执行前就失败（如 DLL 未找到、输入端口缺失）duration_ms 为 None，
                // 此时不下发耗时；算子执行过才失败才显示耗时，与 protocol.rs 的 failed() 一致。
                let duration_part = nr.execution_result.duration_ms
                    .map(|ms| format!(" (耗时 {} ms)", ms))
                    .unwrap_or_default();
                tab.add_runtime_log(
                    format!("节点 {} ({}) 执行失败: {}{}", nr.node_id, nr.operator_name, error_msg, duration_part),
                    LogLevel::Error,
                );
            }
            other => {
                tab.add_runtime_log(
                    format!("节点 {} ({}) 状态: {}", nr.node_id, nr.operator_name, other.to_str()),
                    LogLevel::Info,
                );
            }
        }
    }

    match result.status {
        OperatorExecutionStatus::Completed => {
            tab.add_runtime_log(
                format!("执行完成，总耗时 {} ms", result.total_duration_ms),
                LogLevel::Success,
            );
        }
        _ => {
            let err = result.error_message.unwrap_or_else(|| "执行失败".to_string());
            tab.add_runtime_log(
                format!("执行失败: {} (已完成 {} 个节点)", err, result.node_results.len()),
                LogLevel::Error,
            );
        }
    }
}

/// 每帧轮询后台 DAG 执行任务，回填结果到发起任务的 tab（在 `update()` 顶部调用）。
///
/// 放在 `update()` 顶部而非具体视图内，确保无论当前处于哪个视图都能及时
/// 排空工作线程消息、回填 registry 并释放 runtime 客户端 Mutex。结果按任务
/// 记录的 `model_id` 定位 tab 回填，切换 tab 不影响回填目标。
pub fn poll_dag_exec_task(ctx: &Context, editor_state: &mut DagEditorState) {
    // take() 出任务，避免在借用 task.rx 期间又可变借用 editor_state
    let task = match editor_state.dag_exec_task.take() {
        Some(t) => t,
        None => return,
    };
    let model_id = task.model_id.clone();

    // 排空消息，直到收到 Finished / 通道空 / 通道断开
    let (kind, res) = loop {
        match task.rx.try_recv() {
            Ok(DagExecMessage::Log(msg, level)) => {
                if let Some(idx) = editor_state.find_tab_by_model(&model_id) {
                    editor_state.tabs[idx].add_runtime_log(msg, level);
                }
            }
            Ok(DagExecMessage::StreamChunk { node_id, chunk: _ }) => {
                // 流式 chunk 的预览缓存已在工作线程落盘（save_preview_from_truncated），
                // request_repaint 也已调用。UI 线程无需额外处理——聊天预览窗口每帧
                // 重读缓存文件即可看到逐 token 刷新的「打字机」效果。
                // 此处仅排空消息，避免积压。可选：记录调试日志。
                let _ = node_id; // 避免未使用警告
                // 不 break，继续排空后续消息
            }
            Ok(DagExecMessage::NodeProgress(nr)) => {
                // 服务端推送的单节点进度：立即回填 registry 并记录日志，实现实时可视化
                if let Some(idx) = editor_state.find_tab_by_model(&model_id) {
                    let tab = &mut editor_state.tabs[idx];
                    match nr.execution_result.status {
                        OperatorExecutionStatus::Executing => {
                            tab.io_registry.set_executing(&nr.node_id, Vec::new());
                            tab.add_runtime_log(
                                format!("开始执行节点 {} ({})", nr.node_id, nr.operator_name),
                                LogLevel::Info,
                            );
                        }
                        OperatorExecutionStatus::Completed => {
                            // 复用 apply_dag_node_result 完成落盘预览 + set_result
                            let _ = crate::operator_executor::apply_dag_node_result(
                                &tab.graph,
                                &nr,
                                &mut tab.io_registry,
                            );
                            let duration_part = nr.execution_result.duration_ms
                                .map(|ms| format!("，耗时 {} ms", ms))
                                .unwrap_or_default();
                            tab.add_runtime_log(
                                format!(
                                    "节点 {} ({}) 执行成功，输出端口数: {}{}",
                                    nr.node_id, nr.operator_name, nr.outputs.len(), duration_part
                                ),
                                LogLevel::Success,
                            );
                        }
                        OperatorExecutionStatus::Failed => {
                            tab.io_registry.set_failed(
                                &nr.node_id,
                                Vec::new(),
                                nr.execution_result.clone(),
                            );
                            let error_msg = nr.execution_result.error_message.clone()
                                .unwrap_or_else(|| "执行失败（未知原因）".to_string());
                            let duration_part = nr.execution_result.duration_ms
                                .map(|ms| format!(" (耗时 {} ms)", ms))
                                .unwrap_or_default();
                            tab.add_runtime_log(
                                format!(
                                    "节点 {} ({}) 执行失败: {}{}",
                                    nr.node_id, nr.operator_name, error_msg, duration_part
                                ),
                                LogLevel::Error,
                            );
                        }
                        _ => {}
                    }
                }
                // 不 break，继续排空后续消息
            }
            Ok(DagExecMessage::Finished(res)) => {
                break (task.kind.clone(), res);
            }
            Err(mpsc::TryRecvError::Empty) => {
                // 还在执行，放回任务并请求持续刷新（保持「执行中」指示动画/持续轮询）
                editor_state.dag_exec_task = Some(task);
                ctx.request_repaint();
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // 工作线程异常退出且未发 Finished：落错误日志并清空任务
                if let Some(idx) = editor_state.find_tab_by_model(&model_id) {
                    editor_state.tabs[idx].add_runtime_log(
                        "执行任务异常终止（工作线程已断开）".to_string(),
                        LogLevel::Error,
                    );
                }
                return;
            }
        }
    };

    // 收到 Finished，按 kind 分派回填（task 已 take 出来且不回填 → 等价清空）
    match kind {
        DagExecKind::RunAll => match res {
            Ok(result) => apply_run_all_result(editor_state, &model_id, result),
            Err(e) => {
                if let Some(idx) = editor_state.find_tab_by_model(&model_id) {
                    editor_state.tabs[idx].add_runtime_log(format!("DAG 执行失败: {}", e), LogLevel::Error);
                }
            }
        },
        DagExecKind::RunUpTo { target_node_id } => match res {
            Ok(result) => match editor_state.find_tab_by_model(&model_id) {
                Some(idx) => {
                    // 先把服务端返回的子图执行结果序列化为 JSON（响应方向）记入通信报文日志。
                    // 在可变借用 tab 之前完成序列化，避免借用冲突。
                    let result_json = serde_json::to_string_pretty(&result).unwrap_or_else(|_| {
                        serde_json::to_string(&result).unwrap_or_else(|_| "<序列化失败>".to_string())
                    });
                    editor_state.tabs[idx].add_json_log(
                        JsonDirection::Receive,
                        format!("子图执行结果（运行到节点 {}）", target_node_id),
                        result_json,
                    );

                    // 分离借用：先 apply（借 tab.graph + tab.io_registry），再设 error_message / 日志
                    let apply_err = {
                        let tab = &mut editor_state.tabs[idx];
                        crate::operator_executor::apply_dag_execution_result(
                            &tab.graph,
                            &result,
                            &mut tab.io_registry,
                        )
                        .err()
                    };
                    match apply_err {
                        Some(e) => {
                            let tab = &mut editor_state.tabs[idx];
                            tab.error_message = Some(format!("运行失败: {}", e));
                            tab.add_runtime_log(format!("运行失败: {}", e), LogLevel::Error);
                        }
                        None => {
                            let name = editor_state.tabs[idx]
                                .graph
                                .get_node(&target_node_id)
                                .map(|n| n.operator_type.name())
                                .unwrap_or("未知")
                                .to_string();
                            let tab = &mut editor_state.tabs[idx];
                            tab.error_message =
                                Some(format!("运行成功！已执行到节点 \"{}\"", name));
                            tab.add_runtime_log(
                                format!("运行到节点 {} 完成", target_node_id),
                                LogLevel::Success,
                            );
                        }
                    }
                }
                None => {
                    eprintln!("运行到此结点完成但 tab 已关闭 (model_id={})", model_id);
                }
            },
            Err(e) => {
                if let Some(idx) = editor_state.find_tab_by_model(&model_id) {
                    let tab = &mut editor_state.tabs[idx];
                    tab.error_message = Some(format!("运行失败: {}", e));
                    tab.add_runtime_log(format!("运行失败: {}", e), LogLevel::Error);
                }
            }
        },
    }
}
