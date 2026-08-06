use egui::{Painter, Rect, Pos2, Stroke, Shape, Ui, Vec2, Color32};
use crate::dag::{DagGraph, Edge, Node};
use operator_executor_client::protocol::OperatorExecutionStatus;
use super::state::DagTab;

const NODE_WIDTH: f32 = 140.0;
const NODE_HEIGHT: f32 = 60.0;
const PORT_RADIUS: f32 = 6.0;
const PORT_PADDING: f32 = 8.0;

// 输入端口 (节点左侧) 与输出端口 (节点右侧) 使用不同颜色, 便于区分方向.
const INPUT_PORT_COLOR: Color32 = Color32::from_rgb(90, 160, 255);   // 蓝色
const OUTPUT_PORT_COLOR: Color32 = Color32::from_rgb(90, 220, 130);  // 绿色

pub fn render_dag_canvas(ui: &mut Ui, tab: &mut DagTab) {
    let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
    // 记录画布屏幕矩形, 供算子面板计算点击添加位置 (下一帧生效)
    tab.canvas_viewport_rect = Some(rect);

    if response.dragged()
        && tab.dragging_node_id.is_none()
        && tab.dragging_operator.is_none()
    {
        tab.canvas_offset += response.drag_delta();
    }

    // 用裁剪到画布矩形的 painter 绘制画布内容。算子列表 / 运行日志 / 算子参数等
    // 面板先于 CentralPanel 声明，画布内容后绘制；若不裁剪，平移画布时移出边界的
    // 节点与连线会越过画布边界绘制到相邻面板之上（即「算子和线跑到面板上层」）。
    // 裁剪后，所有画布绘制（底色 / 网格 / 节点 / 连线 / 拖拽预览）都被限制在画布内。
    let painter = ui.painter().with_clip_rect(rect);

    // 画布底色与其余功能面板（建模列表 / 算子列表 / 运行日志 / 算子运行参数）统一为
    // SIDEBAR_BG，避免各区域深浅不一造成视觉割裂；网格线（CANVAS_GRID）仍比底色亮，
    // 可清晰辨识。
    painter.rect_filled(
        rect,
        0.0,
        super::theme::SIDEBAR_BG,
    );

    let grid_size = 20.0;
    let grid_color = super::theme::CANVAS_GRID;

    for x in (rect.left().floor() as i32..rect.right().ceil() as i32).step_by(grid_size as usize) {
        painter.line_segment(
            [
                Pos2::new(x as f32, rect.top()),
                Pos2::new(x as f32, rect.bottom()),
            ],
            Stroke::new(1.0, grid_color),
        );
    }

    for y in (rect.top().floor() as i32..rect.bottom().ceil() as i32).step_by(grid_size as usize) {
        painter.line_segment(
            [
                Pos2::new(rect.left(), y as f32),
                Pos2::new(rect.right(), y as f32),
            ],
            Stroke::new(1.0, grid_color),
        );
    }

    let mut node_interactions = Vec::new();
    // 用于 Executing 状态的脉冲动画（节点边框/徽标随时间正弦变化）
    let time = ui.input(|i| i.time) as f32;

    for node in &tab.graph.nodes {
        let node_rect = Rect::from_center_size(
            node.position.to_pos2(),
            Vec2::new(NODE_WIDTH, NODE_HEIGHT),
        );

        let screen_rect = Rect::from_min_max(
            rect.min + (node_rect.min.to_vec2() + tab.canvas_offset) * tab.canvas_zoom,
            rect.min + (node_rect.max.to_vec2() + tab.canvas_offset) * tab.canvas_zoom,
        );

        let is_selected = tab.selected_node_id.as_deref() == Some(&node.id);
        let border_color = if is_selected {
            Color32::YELLOW
        } else {
            Color32::from_rgba_unmultiplied(58, 58, 62, 255)
        };
        let status = tab.io_registry.get_status(&node.id);

        let stroke_w = 2.0 * tab.canvas_zoom;
        let radius = 8.0 * tab.canvas_zoom;

        painter.rect_filled(
            screen_rect,
            radius,
            border_color,
        );

        let inner_radius = (radius - stroke_w).max(0.0);
        painter.rect_filled(
            screen_rect.shrink(stroke_w),
            inner_radius,
            node.operator_type.color(),
        );

        painter.text(
            screen_rect.center(),
            egui::Align2::CENTER_CENTER,
            node.operator_type.name(),
            egui::FontId::default(),
            Color32::WHITE,
        );

        // Executing 状态：节点边框脉冲高亮（黄色，线宽随时间正弦变化），
        // 让用户实时看到当前正在运行哪个算子。
        if status == OperatorExecutionStatus::Executing {
            let pulse = (time * 2.5).sin() * 0.5 + 0.5; // 0..1，周期约 2.5s
            let exec_stroke = Stroke::new(
                (2.5 + 1.5 * pulse) * tab.canvas_zoom,
                Color32::from_rgb(241, 196, 15), // 黄色
            );
            painter.rect_stroke(screen_rect, radius, exec_stroke);
        }

        // 执行状态角标：成功=绿色勾，失败=红色叉，执行中=黄色脉冲圆点。
        // 徽标压在节点右上角外侧（类似通知角标），避开右侧输出端口。
        enum BadgeKind { Success, Fail, Executing }
        let badge = match status {
            OperatorExecutionStatus::Completed => Some((Color32::from_rgb(46, 204, 113), BadgeKind::Success)),
            OperatorExecutionStatus::Failed => Some((Color32::from_rgb(231, 76, 60), BadgeKind::Fail)),
            OperatorExecutionStatus::Executing => Some((Color32::from_rgb(241, 196, 15), BadgeKind::Executing)),
            _ => None,
        };
        if let Some((badge_color, kind)) = badge {
            let badge_r = 8.0 * tab.canvas_zoom;
            let badge_center = screen_rect.right_top()
                + Vec2::new(badge_r * 0.25, -badge_r * 0.25);
            match kind {
                BadgeKind::Success | BadgeKind::Fail => {
                    painter.circle_filled(badge_center, badge_r, badge_color);
                    painter.circle_stroke(
                        badge_center,
                        badge_r,
                        Stroke::new(1.2 * tab.canvas_zoom, Color32::WHITE),
                    );
                    let glyph_stroke = Stroke::new(2.0 * tab.canvas_zoom, Color32::WHITE);
                    if matches!(kind, BadgeKind::Success) {
                        // 勾: 左 → 底 → 右上
                        let p1 = badge_center + Vec2::new(-0.38, 0.0) * badge_r;
                        let p2 = badge_center + Vec2::new(-0.02, 0.32) * badge_r;
                        let p3 = badge_center + Vec2::new(0.45, -0.35) * badge_r;
                        painter.line_segment([p1, p2], glyph_stroke);
                        painter.line_segment([p2, p3], glyph_stroke);
                    } else {
                        // 叉
                        let h = 0.4 * badge_r;
                        painter.line_segment(
                            [badge_center + Vec2::new(-h, -h), badge_center + Vec2::new(h, h)],
                            glyph_stroke,
                        );
                        painter.line_segment(
                            [badge_center + Vec2::new(-h, h), badge_center + Vec2::new(h, -h)],
                            glyph_stroke,
                        );
                    }
                }
                BadgeKind::Executing => {
                    // 脉冲外环 + 中心实心圆，配合节点边框脉冲动画
                    let pulse = (time * 3.0).sin() * 0.5 + 0.5;
                    let ring_r = badge_r * (1.0 + 0.25 * pulse);
                    painter.circle_stroke(
                        badge_center,
                        ring_r,
                        Stroke::new(1.8 * tab.canvas_zoom, badge_color),
                    );
                    painter.circle_filled(badge_center, badge_r * 0.55, badge_color);
                }
            }
        }

        let input_count = node.operator_type.input_count();
        let output_count = node.operator_type.output_count();

        for i in 0..input_count {
            let port_pos = get_port_position(node, i, false);
            let screen_port_pos = rect.min + (port_pos.to_vec2() + tab.canvas_offset) * tab.canvas_zoom;

            let port_color = if tab.connecting_from.is_some() {
                Color32::YELLOW
            } else {
                INPUT_PORT_COLOR
            };

            painter.circle_filled(screen_port_pos, PORT_RADIUS * tab.canvas_zoom, port_color);

            let port_rect = Rect::from_center_size(screen_port_pos, Vec2::new(PORT_RADIUS * 2.0 * tab.canvas_zoom, PORT_RADIUS * 2.0 * tab.canvas_zoom));
            node_interactions.push(NodeInteraction::InputPort {
                node_id: node.id.clone(),
                port_index: i,
                rect: port_rect,
            });
        }

        for i in 0..output_count {
            let port_pos = get_port_position(node, i, true);
            let screen_port_pos = rect.min + (port_pos.to_vec2() + tab.canvas_offset) * tab.canvas_zoom;

            let port_color = if tab.connecting_from.is_some() {
                Color32::YELLOW
            } else {
                OUTPUT_PORT_COLOR
            };

            painter.circle_filled(screen_port_pos, PORT_RADIUS * tab.canvas_zoom, port_color);

            let port_rect = Rect::from_center_size(screen_port_pos, Vec2::new(PORT_RADIUS * 2.0 * tab.canvas_zoom, PORT_RADIUS * 2.0 * tab.canvas_zoom));
            node_interactions.push(NodeInteraction::OutputPort {
                node_id: node.id.clone(),
                port_index: i,
                rect: port_rect,
            });
        }

        node_interactions.push(NodeInteraction::Node {
            node_id: node.id.clone(),
            rect: screen_rect,
            canvas_zoom: tab.canvas_zoom,
        });
    }

    for edge in &tab.graph.edges {
        render_edge(&painter, edge, &tab.graph, rect.min, tab.canvas_zoom, tab.canvas_offset);
    }

    if let Some((source_node_id, source_port, is_output)) = &tab.connecting_from {
        let pointer_pos = ui.input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO));

        let source_pos = if let Some(node) = tab.graph.get_node(source_node_id) {
            get_port_position(node, *source_port, *is_output)
        } else {
            // 源节点已被删除时, 把预览起点钉在鼠标处, 避免画到 (0,0)
            ((pointer_pos - rect.min) / tab.canvas_zoom - tab.canvas_offset).to_pos2()
        };

        let screen_source_pos = rect.min + (source_pos.to_vec2() + tab.canvas_offset) * tab.canvas_zoom;
        painter.line_segment(
            [screen_source_pos, pointer_pos],
            Stroke::new(2.0, Color32::YELLOW),
        );
    }

    // ===== 从算子面板拖拽到画布的处理 =====
    // 拖拽由算子面板在按钮 dragged() 时写入 dragging_operator; 画布负责检测释放并创建节点.
    // 释放事件用全局 primary_released() 判定, 因为拖拽起点在算子按钮上, 画布自身的
    // drag_released() 不会触发.
    if tab.dragging_operator.is_some() {
        let primary_down = ui.input(|i| i.pointer.primary_down());
        let primary_released = ui.input(|i| i.pointer.primary_released());
        let pointer_pos = ui.input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO));
        let hovered = rect.contains(pointer_pos);

        if primary_released {
            if hovered {
                // 在画布上释放: 在鼠标位置创建节点
                let node_pos = (pointer_pos - rect.min) / tab.canvas_zoom - tab.canvas_offset;
                if let Some(op) = tab.dragging_operator.take() {
                    let new_node = crate::dag::Node::new(op, node_pos);
                    tab.graph.add_node(new_node);
                    tab.dirty = true;
                    tab.error_message = None;
                }
            } else {
                // 在画布外释放: 取消拖拽
                tab.dragging_operator = None;
            }
        } else if !primary_down {
            // 状态失效 (例如丢失释放事件): 清理, 避免悬空拖拽
            tab.dragging_operator = None;
        } else if let Some(op) = tab.dragging_operator.as_ref() {
            // 拖拽进行中: 在鼠标处绘制半透明节点预览
            if hovered {
                let preview_size = Vec2::new(NODE_WIDTH, NODE_HEIGHT) * tab.canvas_zoom;
                let preview_rect = Rect::from_center_size(pointer_pos, preview_size);
                let c = op.color();
                let preview_stroke_w = 2.0;
                let preview_radius = 8.0 * tab.canvas_zoom;

                painter.rect_filled(
                    preview_rect,
                    preview_radius,
                    Color32::YELLOW,
                );

                let preview_inner_radius = (preview_radius - preview_stroke_w).max(0.0);
                painter.rect_filled(
                    preview_rect.shrink(preview_stroke_w),
                    preview_inner_radius,
                    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90),
                );
                painter.text(
                    preview_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    op.name(),
                    egui::FontId::default(),
                    Color32::WHITE,
                );
            } else {
                // 不在画布上时, 在鼠标旁绘制小标签提示当前拖拽的算子
                let label_text = format!("拖拽: {}", op.name());
                let galley = painter.layout_no_wrap(
                    label_text,
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
                let label_rect = Rect::from_min_size(
                    pointer_pos + Vec2::new(12.0, 12.0),
                    galley.size() + Vec2::new(8.0, 4.0),
                );
                painter.rect_filled(
                    label_rect,
                    4.0,
                    Color32::from_rgba_unmultiplied(28, 28, 30, 235),
                );
                painter.galley(
                    label_rect.min + Vec2::new(4.0, 2.0),
                    galley,
                    Color32::WHITE,
                );
            }
        }
    }

    // 注意: interact 顺序决定 hit-test 优先级, 后 interact 的区域优先接收事件.
    // 端口 rect 位于节点 rect 内部, 必须让端口在节点之后 interact, 否则节点会抢占端口的点击.
    // 因此先处理所有节点, 再处理所有端口; 连线删除单独处理.

    // 第一轮: 节点 (拖拽 / 选中 / 右键菜单)
    for interaction in &node_interactions {
        if let NodeInteraction::Node { node_id, rect, canvas_zoom } = interaction {
            let response = ui.interact(*rect, egui::Id::new(format!("node_{}", node_id)), egui::Sense::click_and_drag());
            if response.clicked() {
                // 选中不同节点时，重置参数面板的隐藏状态，使新节点的参数重新展示
                if tab.selected_node_id.as_deref() != Some(node_id.as_str()) {
                    tab.hide_params_panel = false;
                }
                tab.selected_node_id = Some(node_id.clone());
                tab.error_message = None;
            }
            if response.dragged() {
                tab.dragging_node_id = Some(node_id.clone());
                update_node_position(tab, &node_id, response.drag_delta() / *canvas_zoom);
                tab.dirty = true;
            }
            // 仅在拖拽真正结束时释放, 避免其他节点的循环把 dragging_node_id 错误清空
            // (那会导致画布在拖节点时也跟随移动)
            if response.drag_released() {
                if tab.dragging_node_id.as_deref() == Some(node_id) {
                    tab.dragging_node_id = None;
                }
            }
            response.context_menu(|ui| {
                if ui.button("运行到此结点").clicked() {
                    ui.close_menu();
                    // 跨借用：闭包内无法持有 &mut DagEditorState，写入 pending 标志，
                    // 由 render_mining_analysis_view 外层统一检查运行中状态并 spawn。
                    tab.pending_run_up_to = Some(node_id.clone());
                }
                ui.separator();
                if ui.button("数据预览").clicked() {
                    ui.close_menu();
                    tab.preview_node_id = Some(node_id.clone());
                }
                if ui.button("K线图预览").clicked() {
                    ui.close_menu();
                    tab.kline_preview_node_id = Some(node_id.clone());
                }
                if ui.button("折线图预览").clicked() {
                    ui.close_menu();
                    tab.line_chart_preview_node_id = Some(node_id.clone());
                }
                ui.separator();
                if ui.button("删除结点").clicked() {
                    ui.close_menu();
                    // 删除结点时，级联失效所有下游节点
                    tab.io_registry.invalidate_downstream(node_id, &tab.graph);
                    tab.graph.remove_node(node_id);
                    // 移除注册表中该节点的记录
                    tab.io_registry.remove_node(node_id);
                    if tab.selected_node_id.as_deref() == Some(node_id) {
                        tab.selected_node_id = None;
                    }
                    tab.dirty = true;
                }
            });
        }
    }

    // 第二轮: 端口 (后 interact, 优先级高于节点, 确保端口可点击)
    for interaction in &node_interactions {
        match interaction {
            NodeInteraction::InputPort { node_id, port_index, rect } => {
                let response = ui.interact(*rect, egui::Id::new(format!("input_port_{}_{}", node_id, port_index)), egui::Sense::click());
                if response.clicked() {
                    handle_port_click(tab, node_id, *port_index, false);
                }
            }
            NodeInteraction::OutputPort { node_id, port_index, rect } => {
                let response = ui.interact(*rect, egui::Id::new(format!("output_port_{}_{}", node_id, port_index)), egui::Sense::click());
                if response.clicked() {
                    handle_port_click(tab, node_id, *port_index, true);
                }
            }
            NodeInteraction::Node { .. } => {}
        }
    }

    // 连线右键删除
    for edge in tab.graph.edges.clone() {
        if let Some(hit_rect) = edge_hit_rect(&tab.graph, edge.id.clone(), rect.min, tab.canvas_zoom, tab.canvas_offset) {
            let response = ui.interact(hit_rect, egui::Id::new(format!("edge_{}", edge.id)), egui::Sense::click());
            response.context_menu(|ui| {
                if ui.button("删除连线").clicked() {
                    ui.close_menu();
                    // 删除连线时，级联失效目标节点及其所有下游节点
                    tab.io_registry.invalidate_downstream(&edge.target_node_id, &tab.graph);
                    tab.graph.remove_edge(&edge.id);
                    tab.dirty = true;
                }
            });
        }
    }

    if let Some(msg) = &tab.error_message {
        let is_error = msg.contains("失败") || msg.contains("错误");
        let color = if is_error { Color32::RED } else { Color32::GREEN };
        ui.colored_label(color, msg);
    }
}

enum NodeInteraction {
    InputPort { node_id: String, port_index: usize, rect: Rect },
    OutputPort { node_id: String, port_index: usize, rect: Rect },
    Node { node_id: String, rect: Rect, canvas_zoom: f32 },
}

pub fn handle_port_click(tab: &mut DagTab, node_id: &str, port_index: usize, is_output: bool) {
    if let Some((source_node_id, source_port, source_is_output)) = tab.connecting_from.take() {
        // 不允许连接到同一个节点 (自环)
        if source_node_id == node_id {
            tab.error_message = Some("不能连接到同一节点".to_string());
            return;
        }

        // 必须是 一输出 → 一输入; 根据源端口类型和当前端口类型决定边的方向
        // 之前用 `_` 忽略了源端口的 is_output, 导致两次同类型端口点击时
        // 把输出端口索引当作输入端口索引使用, 创建出指向不存在端口的连线.
        let edge = match (source_is_output, is_output) {
            (true, false) => Some(Edge::new(
                source_node_id,
                source_port,
                node_id.to_string(),
                port_index,
            )),
            (false, true) => Some(Edge::new(
                node_id.to_string(),
                port_index,
                source_node_id,
                source_port,
            )),
            (true, true) => {
                tab.error_message = Some("无法连接两个输出端口, 请点击一个输入端口".to_string());
                None
            }
            (false, false) => {
                tab.error_message = Some("无法连接两个输入端口, 请点击一个输出端口".to_string());
                None
            }
        };

        if let Some(edge) = edge {
            let target_node_id = edge.target_node_id.clone();
            match tab.graph.add_edge(edge) {
                Ok(()) => {
                    // 添加连线时，级联失效目标节点及其所有下游节点
                    tab.io_registry.invalidate_downstream(&target_node_id, &tab.graph);
                    tab.error_message = None;
                    tab.dirty = true;
                }
                Err(e) => tab.error_message = Some(e),
            }
        }
    } else {
        tab.connecting_from = Some((node_id.to_string(), port_index, is_output));
    }
}

pub fn update_node_position(tab: &mut DagTab, node_id: &str, delta: Vec2) {
    if let Some(node_mut) = tab.graph.get_node_mut(node_id) {
        node_mut.position += delta;
    }
}

fn render_edge(
    painter: &Painter,
    edge: &Edge,
    graph: &DagGraph,
    canvas_offset: Pos2,
    canvas_zoom: f32,
    view_offset: Vec2,
) {
    if let Some(source_node) = graph.get_node(&edge.source_node_id) {
        if let Some(target_node) = graph.get_node(&edge.target_node_id) {
            let start = get_port_position(source_node, edge.source_port, true);
            let end = get_port_position(target_node, edge.target_port, false);

            let screen_start = canvas_offset + (start.to_vec2() + view_offset) * canvas_zoom;
            let screen_end = canvas_offset + (end.to_vec2() + view_offset) * canvas_zoom;

            let mid_x = (screen_start.x + screen_end.x) / 2.0;
            let control1 = Pos2::new(mid_x, screen_start.y);
            let control2 = Pos2::new(mid_x, screen_end.y);

            // 曲线完整绘制到目标端口
            let curve = Shape::CubicBezier(egui::epaint::CubicBezierShape::from_points_stroke(
                [screen_start, control1, control2, screen_end],
                false,
                Color32::TRANSPARENT,
                Stroke::new(2.0 * canvas_zoom, Color32::from_rgba_unmultiplied(180, 180, 180, 255)),
            ));
            painter.add(curve);

            // 在贝塞尔曲线中点处绘制有向箭头, 表示 source -> target 的方向.
            // 三次贝塞尔曲线: B(t) = (1-t)^3·P0 + 3(1-t)^2·t·P1 + 3(1-t)·t^2·P2 + t^3·P3
            //   B(0.5)  = 0.125·P0 + 0.375·P1 + 0.375·P2 + 0.125·P3
            //   B'(0.5) = 0.75·(P1-P0) + 1.5·(P2-P1) + 0.75·(P3-P2)
            let p0 = screen_start.to_vec2();
            let p1 = control1.to_vec2();
            let p2 = control2.to_vec2();
            let p3 = screen_end.to_vec2();
            let mid_point = (p0 * 0.125 + p1 * 0.375 + p2 * 0.375 + p3 * 0.125).to_pos2();
            let tangent_vec = (p1 - p0) * 0.75 + (p2 - p1) * 1.5 + (p3 - p2) * 0.75;
            let tangent_len = tangent_vec.length();
            let tangent = if tangent_len > 1e-6 {
                tangent_vec / tangent_len
            } else {
                Vec2::X
            };

            // 箭头尺寸随画布缩放; 视觉中心落在曲线中点
            let arrow_size = 8.0 * canvas_zoom;
            let arrow_color = Color32::from_rgba_unmultiplied(180, 180, 180, 255);
            let arrow_tip = mid_point + tangent * (arrow_size * 0.5);
            let arrow_base = mid_point - tangent * (arrow_size * 0.5);
            let perp = Vec2::new(-tangent.y, tangent.x);
            let half_width = arrow_size * 0.5;
            let base_left = arrow_base + perp * half_width;
            let base_right = arrow_base - perp * half_width;

            let arrow = Shape::convex_polygon(
                vec![arrow_tip, base_left, base_right],
                arrow_color,
                Stroke::NONE,
            );
            painter.add(arrow);
        }
    }
}

/// 计算连线在屏幕坐标下的命中区域 (用于右键删除).
/// 以贝塞尔曲线起终点的包围盒为基础, 上下各留一定厚度便于点击.
fn edge_hit_rect(
    graph: &DagGraph,
    edge_id: String,
    view_min: Pos2,
    canvas_zoom: f32,
    view_offset: Vec2,
) -> Option<Rect> {
    let edge = graph.edges.iter().find(|e| e.id == edge_id)?;
    let source_node = graph.get_node(&edge.source_node_id)?;
    let target_node = graph.get_node(&edge.target_node_id)?;

    let start = get_port_position(source_node, edge.source_port, true);
    let end = get_port_position(target_node, edge.target_port, false);

    let screen_start = view_min + (start.to_vec2() + view_offset) * canvas_zoom;
    let screen_end = view_min + (end.to_vec2() + view_offset) * canvas_zoom;

    let min_x = screen_start.x.min(screen_end.x);
    let max_x = screen_start.x.max(screen_end.x);
    let min_y = screen_start.y.min(screen_end.y);
    let max_y = screen_start.y.max(screen_end.y);

    let padding = 6.0;
    Some(Rect::from_min_max(
        Pos2::new(min_x - padding, min_y - padding),
        Pos2::new(max_x + padding, max_y + padding),
    ))
}

fn get_port_position(node: &Node, port_index: usize, is_output: bool) -> Pos2 {
    let node_rect = Rect::from_center_size(
        node.position.to_pos2(),
        Vec2::new(NODE_WIDTH, NODE_HEIGHT),
    );

    let total_ports = if is_output {
        node.operator_type.output_count()
    } else {
        node.operator_type.input_count()
    };

    if total_ports == 0 {
        return node.position.to_pos2();
    }

    let start_y = node_rect.top() + PORT_PADDING + PORT_RADIUS;
    let end_y = node_rect.bottom() - PORT_PADDING - PORT_RADIUS;
    let y_step = if total_ports > 1 {
        (end_y - start_y) / (total_ports - 1) as f32
    } else {
        0.0
    };

    let y = start_y + port_index as f32 * y_step;
    let x = if is_output {
        node_rect.right()
    } else {
        node_rect.left()
    };

    Pos2::new(x, y)
}
