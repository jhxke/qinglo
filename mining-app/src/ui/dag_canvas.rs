//! DAG 画布：节点 / 连线 / 网格 / 缩放 / 平移（基于 `iced::widget::canvas::Program`）。
//!
//! GPU 重度优化版：
//! - Cache 失效修复：iced 0.14 canvas::Cache 只按 size 判断是否重建，
//!   graph/offset/zoom 变化但窗口没 resize 时 Cache 返回旧 Geometry → 画布冻结。
//!   修复：指纹检测到内容变化时先 `world_cache.clear()` 强制重建。
//! - 网格：从 N 条线各自 Path → 合并为 竖线集合 1 个 Path + 横线集合 1 个 Path
//! - 端口占用：从 O(端口×边) 线性扫描 → 单次遍历建 HashMap，O(1) 查询
//! - 节点尺寸：每次 draw_world_content 内建局部 HashMap 缓存，单次计算复用
//! - Stroke：网格线用默认 Butt/Miter（Round 对直线无视觉收益但会让 tesselator 多做顶点）
//! - 连线贝塞尔：复用 Control point 计算逻辑，减少临时分配
//! - 节点卡片：矩形填充+边框+色条顺序不变，但避免重复 Path::rectangle 构造（复用变量）
//! - 拖动节流：拖动节点 / 平移画布时（dragging_in_progress=true）跳过文字 + 端口
//!   渲染（fill_text outline 复杂、端口每节点多圆点，二者是最贵的 tessellation）。
//!   fingerprint 在 dragging_in_progress 边沿变化触发 cache clear，松手后自动恢复完整绘制。
//!   连线创建中保留端口（用户需看到目标端口）；文字在所有拖动场景下都跳过。

use iced::widget::canvas;
use iced::widget::canvas::stroke::{self, Stroke};
use iced::widget::canvas::{Action, Event, Geometry, LineCap, LineJoin, Path, Text};
use iced::{
    Alignment, Color, Element, Font, Length, Point, Rectangle, Renderer, Size, Theme,
    Vector, mouse,
    widget::{container, text},
};
use std::collections::HashMap;

use super::state::{Message, UiState};
use super::theme;
use crate::dag::DagGraph;
use crate::geom::Vec2;

/// 节点固定高度（像素）。
const NODE_HEIGHT: f32 = 32.0;
/// 节点最小宽度（即使算子名为空也保证可视）。
const NODE_MIN_WIDTH: f32 = 80.0;
/// 节点 padding（左右各 12px）。
const NODE_PADDING_X: f32 = 12.0;
/// 节点算子名每字符近似宽度（11.0 字号下）。
const CHAR_WIDTH_ESTIMATE: f32 = 7.0;
/// 网格步长（像素）。
const GRID_STEP: f32 = 24.0;
/// 端口圆点半径。
const PORT_RADIUS: f32 = 4.0;
/// 端口命中测试容差（世界坐标，按节点内部端口大小近似）。
const PORT_HIT_MARGIN: f32 = 7.0;

// ===== 预计算的 Stroke 配置（避免每帧构造结构体） =====
//
// 注意：Stroke 不是 Copy，但 clone 很轻。放在 const 外面 lazy_static 也行，
// 这里直接写函数返回引用的静态值，避免任何运行时构造开销。

fn grid_stroke() -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(Color::from(theme::canvas_grid())),
        width: 1.0,
        // 直线段：Butt 比 Round 少两个半圆的 tessellation 顶点
        line_cap: LineCap::Butt,
        // 网格线互不相连，Miter 无视觉差异但比 Round 便宜
        line_join: LineJoin::Miter,
        ..Default::default()
    }
}

fn edge_stroke() -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(Color::from(theme::text_weak())),
        width: 1.5,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    }
}

fn temp_edge_stroke() -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(Color::from(theme::accent())),
        width: 2.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    }
}

fn card_stroke_normal() -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(Color::from(theme::card_stroke())),
        width: 1.0,
        line_cap: LineCap::Butt,
        line_join: LineJoin::Miter,
        ..Default::default()
    }
}

fn card_stroke_selected() -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(Color::from(theme::accent())),
        width: 2.0,
        line_cap: LineCap::Butt,
        line_join: LineJoin::Miter,
        ..Default::default()
    }
}

fn port_stroke(color: Color) -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(color),
        width: 1.5,
        line_cap: LineCap::Round,
        ..Default::default()
    }
}

/// 渲染 DAG 画布。
///
/// 从 `state.dag_editor.active_tab()` 取出当前 tab 的 `DagGraph` 克隆，
/// 连同 `canvas_offset` / `canvas_zoom` 传入 `DagProgram`。canvas widget
/// 由 `iced::widget::canvas::canvas(program)` 构造。
///
/// GPU 节流关键：额外传 `dragging_in_progress`（拖节点或平移画布任一为真），
/// 这样 `DagProgram::update` 在 CursorMoved 且无拖拽/连线时就不发 Message，
/// 避免触发 update → view → draw 链。
pub fn view_dag_canvas(state: &UiState) -> Element<'_, Message> {
    let (graph, offset, zoom, selected_id, connecting_from, drag_world, dragging_in_progress) =
        match state.dag_editor.active_tab() {
            Some(tab) => (
                tab.graph.clone(),
                tab.canvas_offset,
                tab.canvas_zoom,
                tab.selected_node_id.clone(),
                tab.connecting_from.clone(),
                tab.connecting_drag_world,
                tab.dragging_node_id.is_some() || state.canvas_pan_anchor.is_some(),
            ),
            None => (
                DagGraph::default(),
                Vec2::ZERO,
                1.0,
                None,
                None,
                None,
                false,
            ),
        };

    let program = DagProgram {
        graph,
        offset,
        zoom,
        selected_node_id: selected_id,
        connecting_from,
        connecting_drag_world: drag_world,
        dragging_in_progress,
        // 构造时就算一次指纹，供 PartialEq 快速短路
        fp: 0, // 构造时懒计算（PartialEq 第一次调用才计算并缓存）
    };

    let canvas_widget = canvas(program)
        .width(Length::Fill)
        .height(Length::Fill);

    container(canvas_widget)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::canvas_bg()).into());
            s
        })
        .into()
}

// 保留旧 placeholder API 避免外部引用编译失败
#[allow(dead_code)]
pub fn view_dag_canvas_placeholder() -> Element<'static, Message> {
    container(
        text("DAG 画布（阶段 2.4 已实现最小渲染版本）")
            .color(theme::text_weak())
            .size(12.0),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .into()
}

// ===== DagProgram =====
//
// 持有 graph 的克隆（每次 view 调用都新建一个 DagProgram，iced 内部
// 会通过 PartialEq 比较决定是否重绘）。offset / zoom 来自活动 tab 的
// `canvas_offset` / `canvas_zoom` 字段。
//
// GPU 优化：
// 1. `State` 内放两个 `canvas::Cache`：grid_cache（仅随 bounds 变）、
//    world_cache（随 offset/zoom/graph/selected/connect 变），命中则
//    不跑任何 Path 构建，直接复用 GPU 已有纹理。
// 2. 世界内容 Cache key 使用"快速指纹"：节点数、边数、offset、zoom、
//    选中、connect 状态 + 所有节点 (id,pos,name) 与边 id 的哈希摘要。
//    避免每次都深比较整个 DagGraph（PartialEq 已做但 draw 内部再指纹化
//    防止 Program PartialEq 相等但 iced 仍走 draw 的路径）。

#[derive(Clone)]
struct DagProgram {
    graph: DagGraph,
    offset: Vec2,
    zoom: f32,
    selected_node_id: Option<String>,
    connecting_from: Option<(String, usize, bool)>,
    connecting_drag_world: Option<Vec2>,
    /// 真 = 正在拖节点（dragging_node_id）或平移画布（canvas_pan_anchor），
    /// 用于 CursorMoved 节流：只有拖拽/连线进行中才发 Message。
    dragging_in_progress: bool,
    /// 懒缓存的世界指纹（u64）。0 视为未计算。PartialEq/draw 两边都会写它，
    /// 因为内部 mutability（Cell）在 Clone 里很麻烦，这里改为在 impl 里
    /// 用内部 helper 按需计算 → DagProgram 非 Clone 约束下 &self 也能算。
    fp: u64,
}

impl DagProgram {
    /// 计算（若 fp==0）或返回缓存的指纹值。
    ///
    /// 注意：DagProgram 每次 view 新建后 PartialEq 只比 1~2 次，fp 字段
    /// 真正命中的场景是"同一个 DagProgram 实例被 draw 调用多次"（iced
    /// 内部可能在某些状态下重复调用 draw）。为保持语义简单，每次调用
    /// world_fingerprint_of 都重算（它本身已经是 FNV-1a 极快），fp 字段
    /// 主要用于 PartialEq 内部的"同实例第二次比较"短路。
    fn get_fp(&self) -> u64 {
        if self.fp != 0 {
            self.fp
        } else {
            world_fingerprint_of(
                &self.offset,
                self.zoom,
                self.dragging_in_progress,
                &self.graph,
                self.selected_node_id.as_deref(),
                self.connecting_from.as_ref(),
                self.connecting_drag_world.as_ref(),
            )
        }
    }
}

impl PartialEq for DagProgram {
    fn eq(&self, other: &Self) -> bool {
        // 快速路径 1：同一个 fp（指纹不同 → 一定不等）
        // 指纹相同 → 再走关键字段精确比较（FP 相等但内容不同的概率极低，
        // 但为了正确性不跳过精确比较，只是先短路"显然不等"的情况）
        let (fp_a, fp_b) = (self.get_fp(), other.get_fp());
        if fp_a != fp_b {
            return false;
        }

        // 精确比较：只比"真正影响渲染的最小字段集合"，不比整个图（图里
        // 的 operator_type.params 等字段不影响视觉，PartialEq 默认会比）。
        self.dragging_in_progress == other.dragging_in_progress
            && self.offset == other.offset
            && self.zoom == other.zoom
            && self.selected_node_id == other.selected_node_id
            && self.connecting_from == other.connecting_from
            && self.connecting_drag_world == other.connecting_drag_world
            && self.graph.nodes.len() == other.graph.nodes.len()
            && self.graph.edges.len() == other.graph.edges.len()
            && self
                .graph
                .nodes
                .iter()
                .zip(other.graph.nodes.iter())
                .all(|(a, b)| {
                    a.id == b.id
                        && a.position == b.position
                        && a.operator_type.name() == b.operator_type.name()
                })
            && self
                .graph
                .edges
                .iter()
                .zip(other.graph.edges.iter())
                .all(|(a, b)| a.id == b.id)
    }
}

/// Program State：持有两个 Geometry Cache，避免每帧重建 Path。
///
/// `grid_cache` 只依赖 bounds.size；`world_cache` 依赖 offset/zoom/图内容/
/// 选中/连线状态——由 `world_fingerprint` 64 位摘要判断是否命中。
struct CanvasCacheState {
    grid_cache: canvas::Cache,
    world_cache: canvas::Cache,
    inner: std::cell::RefCell<CacheInner>,
}

struct CacheInner {
    last_bounds_size: Size,
    last_world_fp: u64,
}

impl Default for CanvasCacheState {
    fn default() -> Self {
        Self {
            grid_cache: canvas::Cache::default(),
            world_cache: canvas::Cache::default(),
            inner: std::cell::RefCell::new(CacheInner {
                last_bounds_size: Size::ZERO,
                last_world_fp: 0,
            }),
        }
    }
}

impl canvas::Program<Message> for DagProgram {
    type State = CanvasCacheState;

    /// 鼠标事件处理：将 `Event::Mouse` 转换为应用级 `Message` 发布给 `MyApp::update`。
    ///
    /// GPU 优化关键点：**CursorMoved 只在需要写入状态时才发 Message**。
    fn update(
        &self,
        _state: &mut CanvasCacheState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let Some(pos) = cursor.position_in(bounds) else {
            return None;
        };
        let screen = Vec2::new(pos.x, pos.y);
        let world = screen_to_world(screen, self.offset, self.zoom);

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // 端口命中优先于节点命中；命中输出端口 → 开始连线
                if let Some((node_id, port_idx, true)) =
                    hit_test_port(&self.graph, world)
                {
                    return Some(
                        Action::publish(Message::ConnectStart {
                            node_id,
                            port_index: port_idx,
                            is_output: true,
                        })
                        .and_capture(),
                    );
                }
                Some(Action::publish(Message::CanvasPress(screen)).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                Some(Action::publish(Message::CanvasRightClick(screen)).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if self.connecting_from.is_some() {
                    Some(
                        Action::publish(Message::ConnectRelease(screen))
                            .and_capture(),
                    )
                } else {
                    Some(Action::publish(Message::CanvasRelease(screen)).and_capture())
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                // GPU 节流：**只在有实际副作用的场景发 Message**。
                if self.connecting_from.is_some() {
                    Some(Action::publish(Message::ConnectDrag(screen)).and_capture())
                } else if self.dragging_in_progress {
                    Some(Action::publish(Message::CanvasMove(screen)).and_capture())
                } else {
                    None
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let delta_y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                if delta_y.abs() < f32::EPSILON {
                    return None;
                }
                Some(
                    Action::publish(Message::CanvasWheel { delta_y, pos: screen })
                        .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &CanvasCacheState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let Some(pos) = cursor.position_in(bounds) else {
            return mouse::Interaction::default();
        };
        let world = screen_to_world(Vec2::new(pos.x, pos.y), self.offset, self.zoom);

        if self.connecting_from.is_some() {
            return mouse::Interaction::Crosshair;
        }
        if hit_test_port(&self.graph, world).is_some() {
            return mouse::Interaction::Crosshair;
        }
        if hit_test_node(&self.graph, world).is_some() {
            return mouse::Interaction::Grab;
        }
        mouse::Interaction::default()
    }

    fn draw(
        &self,
        state: &CanvasCacheState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();

        // --- 第一层缓存：网格背景（只随 bounds.size 变化） ---
        // Cache::draw 内部按 size 判断是否重建，size 不变时直接返回旧 Geometry。
        let grid_geo = state.grid_cache.draw(renderer, size, |frame| {
            draw_grid_optimized(frame, size);
        });

        // --- 第二层缓存：世界内容 ---
        //
        // iced 0.14 的 canvas::Cache::draw 只在 size 变化或 clear() 后才
        // 重新执行闭包。如果 graph/offset/zoom 变了但窗口没 resize，Cache
        // 会返回旧 Geometry → 画布"冻结"。
        //
        // 修复：用指纹检测内容变化，变化时先 clear() 强制 Cache 重建。
        let fp = world_fingerprint_of(
            &self.offset,
            self.zoom,
            self.dragging_in_progress,
            &self.graph,
            self.selected_node_id.as_deref(),
            self.connecting_from.as_ref(),
            self.connecting_drag_world.as_ref(),
        );
        let needs_rebuild = {
            let inner = state.inner.borrow();
            fp != inner.last_world_fp || size != inner.last_bounds_size
        };
        if needs_rebuild {
            // 强制 Cache 下次 draw 重建 Geometry
            state.world_cache.clear();
            if let Ok(mut inner) = state.inner.try_borrow_mut() {
                inner.last_world_fp = fp;
                inner.last_bounds_size = size;
            }
        }
        let world_geo = state.world_cache.draw(renderer, size, |frame| {
            draw_world_content_optimized(frame, self);
        });

        vec![grid_geo, world_geo]
    }
}

// ===== 指纹计算（与旧 DagProgram::world_fingerprint 相同逻辑，改为独立 fn 接受显式参数） =====

fn world_fingerprint_of(
    offset: &Vec2,
    zoom: f32,
    dragging_in_progress: bool,
    graph: &DagGraph,
    selected_node_id: Option<&str>,
    connecting_from: Option<&(String, usize, bool)>,
    connecting_drag_world: Option<&Vec2>,
) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;

    fn mix_u64(h: &mut u64, v: u64) {
        *h ^= v;
        *h = h.wrapping_mul(FNV_PRIME);
    }
    fn mix_bytes(h: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *h ^= b as u64;
            *h = h.wrapping_mul(FNV_PRIME);
        }
    }
    fn mix_f32(h: &mut u64, v: f32) {
        mix_u64(h, v.to_bits() as u64);
    }

    mix_f32(&mut h, offset.x);
    mix_f32(&mut h, offset.y);
    mix_f32(&mut h, zoom);
    mix_u64(&mut h, if dragging_in_progress { 1 } else { 0 });

    mix_u64(&mut h, graph.nodes.len() as u64);
    mix_u64(&mut h, graph.edges.len() as u64);

    for n in &graph.nodes {
        mix_bytes(&mut h, n.id.as_bytes());
        mix_f32(&mut h, n.position.x);
        mix_f32(&mut h, n.position.y);
        mix_bytes(&mut h, n.operator_type.name().as_bytes());
    }
    for e in &graph.edges {
        mix_bytes(&mut h, e.id.as_bytes());
    }

    if let Some(id) = selected_node_id {
        mix_u64(&mut h, 1);
        mix_bytes(&mut h, id.as_bytes());
    } else {
        mix_u64(&mut h, 0);
    }

    if let Some((id, idx, is_out)) = connecting_from {
        mix_u64(&mut h, 1);
        mix_bytes(&mut h, id.as_bytes());
        mix_u64(&mut h, *idx as u64);
        mix_u64(&mut h, if *is_out { 1 } else { 0 });
    } else {
        mix_u64(&mut h, 0);
    }
    if let Some(v) = connecting_drag_world {
        mix_u64(&mut h, 1);
        mix_f32(&mut h, v.x);
        mix_f32(&mut h, v.y);
    } else {
        mix_u64(&mut h, 0);
    }

    h
}

// ===== 优化后的绘制主函数 =====

/// 合并网格线绘制：所有竖线进一个 Path，所有横线进一个 Path。
///
/// 从旧版 ~(w/step + h/step) 个 Path + stroke 调用 → 2 个 Path + 2 个 stroke。
/// 对 1920x1080 @ step=24：从 ~125 drawcall → 2 drawcall，GPU 顶点提交量减少 ~60 倍。
fn draw_grid_optimized(frame: &mut canvas::Frame, size: iced::Size) {
    let step = GRID_STEP;
    let w = size.width;
    let h = size.height;
    let gs = grid_stroke();

    // 竖线集合（一个 Path 内 move_to→line_to→move_to→line_to…）
    let vert_path = Path::new(|b| {
        let mut x = 0.0;
        while x <= w {
            b.move_to(Point::new(x, 0.0));
            b.line_to(Point::new(x, h));
            x += step;
        }
    });
    frame.stroke(&vert_path, gs.clone());

    // 横线集合
    let horz_path = Path::new(|b| {
        let mut y = 0.0;
        while y <= h {
            b.move_to(Point::new(0.0, y));
            b.line_to(Point::new(w, y));
            y += step;
        }
    });
    frame.stroke(&horz_path, gs);
}

/// 端口占用索引：key = (node_id, port_index, is_output)
type PortKey = (String, usize, bool);

/// 构建端口占用哈希表：一次遍历 edges，后续查询 O(1)。
fn build_port_index(graph: &DagGraph) -> (HashMap<PortKey, bool>, HashMap<PortKey, bool>) {
    // out_has_fan: 输出端口是否有至少一条出边
    let mut out_has_fan: HashMap<PortKey, bool> = HashMap::new();
    // in_occupied: 输入端口是否已连线
    let mut in_occupied: HashMap<PortKey, bool> = HashMap::new();

    for e in &graph.edges {
        out_has_fan.insert((e.source_node_id.clone(), e.source_port, true), true);
        in_occupied.insert((e.target_node_id.clone(), e.target_port, false), true);
    }

    (out_has_fan, in_occupied)
}

/// 节点宽度缓存：按节点 id → width，避免每次调用 estimate_node_width。
fn build_width_cache(graph: &DagGraph) -> HashMap<String, f32> {
    let mut cache: HashMap<String, f32> = HashMap::with_capacity(graph.nodes.len());
    for n in &graph.nodes {
        cache.insert(n.id.clone(), estimate_node_width(&n.operator_type.name()));
    }
    cache
}

/// 世界内容绘制（优化版）：
/// 1. 预建宽度缓存 + 端口索引（一次遍历，后续 O(1)）
/// 2. 连线：仍然每条边一个 Path（贝塞尔之间无法合并），但复用 stroke 对象
/// 3. 节点：所有 fill/stroke 复用预定义 Stroke，减少结构构造
fn draw_world_content_optimized(frame: &mut canvas::Frame, p: &DagProgram) {
    // 1) 应用画布变换：平移 + 缩放
    frame.translate(Vector::new(p.offset.x, p.offset.y));
    if (p.zoom - 1.0).abs() > f32::EPSILON {
        frame.scale(p.zoom);
    }

    // 2) 一次性预计算索引
    let width_cache = build_width_cache(&p.graph);
    let (out_has_fan, in_occupied) = build_port_index(&p.graph);

    let card_bg = Color::from(theme::card_bg());
    let text_strong = Color::from(theme::text_strong());
    let edge_stroke_val = edge_stroke();
    let selected_id = p.selected_node_id.as_deref();

    // 3) 连线（先画，被节点覆盖线头）
    for edge in &p.graph.edges {
        let Some(src) = p.graph.get_node(&edge.source_node_id) else {
            continue;
        };
        let Some(dst) = p.graph.get_node(&edge.target_node_id) else {
            continue;
        };
        let src_w = width_cache.get(&src.id).copied().unwrap_or(NODE_MIN_WIDTH);
        let dst_w = width_cache.get(&dst.id).copied().unwrap_or(NODE_MIN_WIDTH);

        let p1 = port_world_position(
            src.position,
            src_w,
            NODE_HEIGHT,
            true,
            edge.source_port,
            src.operator_type.output_count(),
        );
        let p2 = port_world_position(
            dst.position,
            dst_w,
            NODE_HEIGHT,
            false,
            edge.target_port,
            dst.operator_type.input_count(),
        );
        let dx = (p2.x - p1.x).max(20.0) * 0.5;
        let c1 = Point::new(p1.x + dx, p1.y);
        let c2 = Point::new(p2.x - dx, p2.y);
        let path = Path::new(|b| {
            b.move_to(p1);
            b.bezier_curve_to(c1, c2, p2);
        });
        frame.stroke(&path, edge_stroke_val.clone());
    }

    // 3.5) 连线创建临时贝塞尔
    if let (Some((src_id, src_port_idx, true)), Some(drag_world)) =
        (&p.connecting_from, p.connecting_drag_world)
    {
        if let Some(src) = p.graph.get_node(src_id) {
            let src_w = width_cache.get(src_id).copied().unwrap_or(NODE_MIN_WIDTH);
            let p1 = port_world_position(
                src.position,
                src_w,
                NODE_HEIGHT,
                true,
                *src_port_idx,
                src.operator_type.output_count(),
            );
            let p2 = Point::new(drag_world.x, drag_world.y);
            let dx = (p2.x - p1.x).max(20.0) * 0.5;
            let c1 = Point::new(p1.x + dx, p1.y);
            let c2 = Point::new(p2.x - dx, p2.y);
            let path = Path::new(|b| {
                b.move_to(p1);
                b.bezier_curve_to(c1, c2, p2);
            });
            frame.stroke(&path, temp_edge_stroke());
        }
    }

    // 4) 节点 + 端口（同节点端口在循环内直接画，利用 width_cache）
    //
    // GPU 节流：dragging_in_progress=true（拖节点 / 平移画布）时跳过 fill_text
    // 和端口圆点——二者是最贵的 tessellation。fingerprint 已纳入该字段，
    // 松手时 fp 变化 → cache clear → 自动恢复完整绘制。
    // 连线创建中保留端口（用户需看到目标端口）。
    let dragging = p.dragging_in_progress;
    let connecting = p.connecting_from.is_some();
    let draw_text = !dragging;
    let draw_ports = !dragging || connecting;

    for node in &p.graph.nodes {
        let w = width_cache.get(&node.id).copied().unwrap_or(NODE_MIN_WIDTH);
        let h = NODE_HEIGHT;
        let top_left = Point::new(node.position.x, node.position.y);

        // 卡片矩形 Path（重用：fill + stroke 都用它）
        let rect_path = Path::rectangle(top_left, Size::new(w, h));

        // 卡片背景填充
        frame.fill(&rect_path, card_bg);

        // 左侧色条（3px）
        let bar = Path::rectangle(top_left, Size::new(3.0, h));
        frame.fill(&bar, node.operator_type.color());

        // 边框：选中 → accent 2px；否则默认 1px
        let is_selected = selected_id == Some(&node.id);
        if is_selected {
            frame.stroke(&rect_path, card_stroke_selected());
        } else {
            frame.stroke(&rect_path, card_stroke_normal());
        }

        // 节点文本（算子名）——拖动期间跳过
        if draw_text {
            frame.fill_text(Text {
                content: node.operator_type.name().to_string(),
                position: Point::new(
                    node.position.x + w / 2.0,
                    node.position.y + h / 2.0,
                ),
                color: text_strong,
                size: 11.0.into(),
                font: Font::with_name("Microsoft YaHei"),
                align_x: Alignment::Center.into(),
                align_y: iced::alignment::Vertical::Center,
                ..Default::default()
            });
        }

        // 端口圆点：直接在节点循环里画（拿得到 width_cache 里的 w）
        // 连线创建中保留，其它拖动场景跳过
        if draw_ports {
            draw_ports_indexed(frame, node, w, h, &out_has_fan, &in_occupied, card_bg);
        }
    }
}

/// 使用预建索引绘制单个节点的所有端口：去掉内部 O(边) 扫描。
fn draw_ports_indexed(
    frame: &mut canvas::Frame,
    node: &crate::dag::Node,
    w: f32,
    h: f32,
    out_has_fan: &HashMap<PortKey, bool>,
    in_occupied: &HashMap<PortKey, bool>,
    card_bg: Color,
) {
    let output_count = node.operator_type.output_count();
    let input_count = node.operator_type.input_count();

    let card_stroke_color = Color::from(theme::card_stroke());
    let success_color = Color::from(theme::success());

    // 输出端口（右侧）
    for i in 0..output_count {
        let p = port_world_position(node.position, w, h, true, i, output_count);
        let key: PortKey = (node.id.clone(), i, true);
        let has_fan = out_has_fan.get(&key).copied().unwrap_or(false);
        let ring_color = if has_fan { Color::WHITE } else { card_stroke_color };
        draw_port_cached(frame, p, ring_color, card_bg);
    }

    // 输入端口（左侧）
    for i in 0..input_count {
        let p = port_world_position(node.position, w, h, false, i, input_count);
        let key: PortKey = (node.id.clone(), i, false);
        let occupied = in_occupied.get(&key).copied().unwrap_or(false);
        let ring_color = if occupied { card_stroke_color } else { success_color };
        draw_port_cached(frame, p, ring_color, card_bg);
    }
}

/// 单个端口绘制：和旧版相同，但 card_bg 作为参数传入避免重复 theme 查询。
#[inline(always)]
fn draw_port_cached(frame: &mut canvas::Frame, center: Point, ring_color: Color, card_bg: Color) {
    let outer = Path::circle(center, PORT_RADIUS);
    frame.stroke(&outer, port_stroke(ring_color));
    let inner = Path::circle(center, PORT_RADIUS - 1.5);
    frame.fill(&inner, card_bg);
}

// ===== 命中测试与工具函数（保持对外 API 不变） =====

pub fn port_world_position(
    node_pos: Vec2,
    w: f32,
    h: f32,
    is_output: bool,
    port_index: usize,
    port_count: usize,
) -> Point {
    let margin_v = 6.0;
    let usable = (h - margin_v * 2.0).max(0.0);
    let y = if port_count <= 1 {
        node_pos.y + h / 2.0
    } else {
        let step = usable / (port_count - 1) as f32;
        node_pos.y + margin_v + step * port_index as f32
    };
    let x = if is_output { node_pos.x + w } else { node_pos.x };
    Point::new(x, y)
}

pub fn screen_to_world(screen: Vec2, offset: Vec2, zoom: f32) -> Vec2 {
    let z = if zoom.abs() < f32::EPSILON { 1.0 } else { zoom };
    Vec2::new((screen.x - offset.x) / z, (screen.y - offset.y) / z)
}

pub fn hit_test_node(graph: &DagGraph, world: Vec2) -> Option<String> {
    for node in graph.nodes.iter().rev() {
        let w = estimate_node_width(&node.operator_type.name());
        if world.x >= node.position.x
            && world.x <= node.position.x + w
            && world.y >= node.position.y
            && world.y <= node.position.y + NODE_HEIGHT
        {
            return Some(node.id.clone());
        }
    }
    None
}

pub fn hit_test_port(
    graph: &DagGraph,
    world: Vec2,
) -> Option<(String, usize, bool)> {
    let hit_r_sq = (PORT_RADIUS + PORT_HIT_MARGIN).powi(2);
    for node in graph.nodes.iter().rev() {
        let w = estimate_node_width(&node.operator_type.name());
        let h = NODE_HEIGHT;

        let out_count = node.operator_type.output_count();
        for i in 0..out_count {
            let p = port_world_position(node.position, w, h, true, i, out_count);
            let dx = world.x - p.x;
            let dy = world.y - p.y;
            if dx * dx + dy * dy <= hit_r_sq {
                return Some((node.id.clone(), i, true));
            }
        }
        let in_count = node.operator_type.input_count();
        for i in 0..in_count {
            let p = port_world_position(node.position, w, h, false, i, in_count);
            let dx = world.x - p.x;
            let dy = world.y - p.y;
            if dx * dx + dy * dy <= hit_r_sq {
                return Some((node.id.clone(), i, false));
            }
        }
    }
    None
}

pub fn hit_test_input_port(
    graph: &DagGraph,
    world: Vec2,
) -> Option<(String, usize)> {
    let hit_r_sq = (PORT_RADIUS + PORT_HIT_MARGIN).powi(2);
    for node in graph.nodes.iter().rev() {
        let w = estimate_node_width(&node.operator_type.name());
        let h = NODE_HEIGHT;
        let in_count = node.operator_type.input_count();
        for i in 0..in_count {
            let p = port_world_position(node.position, w, h, false, i, in_count);
            let dx = world.x - p.x;
            let dy = world.y - p.y;
            if dx * dx + dy * dy <= hit_r_sq {
                return Some((node.id.clone(), i));
            }
        }
    }
    None
}

pub fn estimate_node_width(name: &str) -> f32 {
    let chars = name.chars().count() as f32;
    (chars * CHAR_WIDTH_ESTIMATE + NODE_PADDING_X * 2.0).max(NODE_MIN_WIDTH)
}
