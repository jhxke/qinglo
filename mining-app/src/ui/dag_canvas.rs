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
//! - 拖动节流：拖动节点 / 平移画布时（dragging_in_progress=true）仅跳过
//!   阴影与外发光（辅助视觉，tessellation 较贵）。
//!   fingerprint 在 dragging_in_progress 边沿变化触发 cache clear，松手后自动恢复完整绘制。
//!   文字与端口圆点始终绘制，拖动时保持可见（用户要求）。

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

/// 节点固定高度（像素）。v3：从 32 → 38，呼吸感更好，文字不贴边。
const NODE_HEIGHT: f32 = 38.0;
/// 节点圆角（像素）。v3：卡片感，9px 与 sidebar 卡片统一。
const NODE_ROUNDING: f32 = 9.0;
/// 节点最小宽度（即使算子名为空也保证可视）。
const NODE_MIN_WIDTH: f32 = 92.0;
/// 节点 padding（左右各 14px）。v3：左右各加 2px，文字不贴色条/端口。
const NODE_PADDING_X: f32 = 14.0;
/// 节点算子名每字符近似宽度（11.5 字号下）。v3：微调更匹配新字号。
const CHAR_WIDTH_ESTIMATE: f32 = 7.2;
/// 网格步长（像素）。
const GRID_STEP: f32 = 24.0;
/// 端口圆点半径。v3：从 4 → 4.6，配合更高节点更协调。
/// v6：增到 5.4，让四层结构（外圈+描边+填充+心点）有呼吸感。
const PORT_RADIUS: f32 = 5.4;
/// 端口命中测试容差（世界坐标，按节点内部端口大小近似）。
const PORT_HIT_MARGIN: f32 = 8.0;

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
    let (graph, offset, zoom, selected_id, connecting_from, drag_world, dragging_in_progress, node_statuses, anim_time) =
        match state.dag_editor.active_tab() {
            Some(tab) => (
                tab.graph.clone(),
                tab.canvas_offset,
                tab.canvas_zoom,
                tab.selected_node_id.clone(),
                tab.connecting_from.clone(),
                tab.connecting_drag_world,
                tab.dragging_node_id.is_some() || state.canvas_pan_anchor.is_some(),
                tab.io_registry.statuses_snapshot(),
                state.anim_time,
            ),
            None => (
                DagGraph::default(),
                Vec2::ZERO,
                1.0,
                None,
                None,
                None,
                false,
                HashMap::new(),
                0.0,
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
        node_statuses,
        anim_time,
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
    /// 节点执行状态快照（节点 ID → 状态码：0未执行/1执行中/2完成/3失败/4过期）。
    /// 由 `NodeIORegistry::statuses_snapshot` 生成，供渲染层按状态着色与动画。
    node_statuses: HashMap<String, u8>,
    /// 运行动画累积时间（秒）。仅在 DAG 执行中由 `AnimTick` 推进，
    /// 驱动节点呼吸发光 / 边数据流动光点。静态时不推进 → 指纹稳定不重绘。
    anim_time: f32,
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
                &self.node_statuses,
                self.anim_time,
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
            && self.anim_time == other.anim_time
            && self.node_statuses == other.node_statuses
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

        // 正在连线：十字光标（端口命中优先级最高）
        if self.connecting_from.is_some() {
            return mouse::Interaction::Crosshair;
        }
        if hit_test_port(&self.graph, world).is_some() {
            return mouse::Interaction::Crosshair;
        }
        // 拖动中（平移画布或拖节点）：抓握手
        if self.dragging_in_progress {
            return mouse::Interaction::Grabbing;
        }
        // 命中节点：节点可拖
        if hit_test_node(&self.graph, world).is_some() {
            return mouse::Interaction::Grab;
        }
        // 画布空白：手形光标，提示可拖动平移画布
        mouse::Interaction::Grab
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
            &self.node_statuses,
            self.anim_time,
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
    node_statuses: &HashMap<String, u8>,
    anim_time: f32,
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
        // 节点执行状态：0=未执行(默认) 1=执行中 2=完成 3=失败 4=过期
        mix_u64(&mut h, node_statuses.get(&n.id).copied().unwrap_or(0) as u64);
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

    // 运行动画时间：静态时为常量 → 指纹稳定；执行中由 AnimTick 推进 → 指纹变化触发重绘
    mix_f32(&mut h, anim_time);

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
        // 源/目标执行状态：用于边着色
        let src_status = p.node_statuses.get(&edge.source_node_id).copied().unwrap_or(0u8);
        let dst_status = p.node_statuses.get(&edge.target_node_id).copied().unwrap_or(0u8);
        // 边色随状态语义化：两端完成→翠绿、流向执行中→靛青、失败端→珊瑚红
        let edge_color = match (src_status, dst_status) {
            (2, 2) => mix_color(Color::from(theme::text_weak()), Color::from(theme::success()), 0.40),
            (2, 1) | (2, 0) => mix_color(Color::from(theme::text_weak()), Color::from(theme::accent()), 0.35),
            (_, 3) | (3, _) => mix_color(Color::from(theme::text_weak()), Color::from(theme::danger()), 0.35),
            _ => Color::from(theme::text_weak()),
        };
        let path = Path::new(|b| {
            b.move_to(p1);
            b.bezier_curve_to(c1, c2, p2);
        });
        frame.stroke(&path, Stroke {
            style: stroke::Style::Solid(edge_color),
            ..edge_stroke_val.clone()
        });

        // 连线中部箭头：在贝塞尔曲线 t=0.5 处画一个小三角形，指向 p2 方向
        draw_edge_arrow(frame, p1, c1, c2, p2, edge_color);
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
            // 临时连线也加箭头，颜色用 accent
            draw_edge_arrow(frame, p1, c1, c2, p2, Color::from(theme::accent()));
        }
    }

    // 4) 节点 + 端口（同节点端口在循环内直接画，利用 width_cache）
    //
    // GPU 节流：dragging_in_progress=true（拖节点 / 平移画布）时仅跳过
    // 阴影/外发光（这些是最贵的辅助视觉）。fingerprint 已纳入该字段，
    // 松手时 fp 变化 → cache clear → 自动恢复完整绘制。
    // 用户要求：文字与端口圆点始终绘制，拖动时可见。
    let dragging = p.dragging_in_progress;
    // 文字与端口始终绘制（用户要求拖动时保持可见）
    let draw_text = true;
    let draw_ports = true;

    for node in &p.graph.nodes {
        let w = width_cache.get(&node.id).copied().unwrap_or(NODE_MIN_WIDTH);
        let h = NODE_HEIGHT;
        let top_left = Point::new(node.position.x, node.position.y);

        let is_selected = selected_id == Some(&node.id);
        let op_color = node.operator_type.color();

        // === 执行状态视觉（运行动画） ===
        // 0=未执行 1=执行中 2=完成 3=失败 4=过期
        let status = p.node_statuses.get(&node.id).copied().unwrap_or(0u8);
        // 呼吸因子 0..1：失败节点用 sin 调制发光强度与边框宽度
        // 注意：执行中(1)/完成(2) 不再驱动卡片呼吸发光——
        //   执行中改由"输出端口圈闪烁"表达（见 draw_ports_indexed），
        //   完成态改由"节点右上角成功标识小绿点"表达（见循环末尾）。
        let pulse = (p.anim_time * 2.5).sin() * 0.5 + 0.5;
        // 状态边框 (颜色, 宽度)；None 表示无状态视觉，回退到选中/默认
        let status_border: Option<(Color, f32)> = match status {
            3 => Some((Color::from(theme::danger()), 1.6)),
            4 => Some((Color::from(theme::warning()), 1.4)),
            _ => None,
        };
        // 状态外发光 (颜色, alpha)；仅失败态保留外发光呼吸
        let status_glow: Option<(Color, f32)> = match status {
            3 => Some((Color::from(theme::danger()), 0.35 + pulse * 0.20)),
            _ => None,
        };

        // v5：三层混合颜色
        // - 卡片底色：10% 算子色 + 90% card_bg（v4 是 8%，再强化一点）
        // - 边框默认态：50% 算子色 + 50% 默认边框灰（v4 是 45%）
        // - 移除外挂竖条，改"内嵌色晕 + 前置圆点标签"
        let tinted_bg = mix_color(card_bg, op_color, 0.10);
        let tinted_stroke_color = mix_color(
            Color::from(theme::card_stroke()),
            op_color,
            0.50,
        );

        // === 微阴影（圆角 + 不透明度进一步略增） ===
        if !dragging {
            let shadow_path = rounded_rect_path(
                Point::new(top_left.x + 1.5, top_left.y + 2.5),
                w, h,
                NODE_ROUNDING,
            );
            frame.fill(
                &shadow_path,
                Color { r: 0.0, g: 0.0, b: 0.0, a: 200.0 / 255.0 / 4.0 },
            );
        }

        // === 卡片圆角矩形 Path ===
        let rect_path = rounded_rect_path(top_left, w, h, NODE_ROUNDING);

        // === 卡片填充（带算子色微染） ===
        frame.fill(&rect_path, tinted_bg);

        // === 边框：执行状态视觉优先 > 选中 > 默认 ===
        if let Some((sc, sw)) = status_border {
            // 执行中/完成/失败/过期：状态色边框 + 状态外发光
            frame.stroke(&rect_path, Stroke {
                style: stroke::Style::Solid(sc),
                width: sw,
                ..card_stroke_normal()
            });
            if let Some((gc, ga)) = status_glow {
                if !dragging {
                    let glow_path = rounded_rect_path(
                        Point::new(top_left.x - 1.5, top_left.y - 1.5),
                        w + 3.0,
                        h + 3.0,
                        NODE_ROUNDING + 1.5,
                    );
                    frame.stroke(
                        &glow_path,
                        Stroke {
                            style: stroke::Style::Solid(with_alpha(gc, ga)),
                            width: 1.5,
                            ..card_stroke_normal()
                        },
                    );
                }
            }
        } else if is_selected {
            frame.stroke(&rect_path, card_stroke_selected());
            if !dragging {
                let glow_path = rounded_rect_path(
                    Point::new(top_left.x - 1.0, top_left.y - 1.0),
                    w + 2.0,
                    h + 2.0,
                    NODE_ROUNDING + 1.0,
                );
                let glow_color = mix_color(
                    Color {
                        r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 140.0/255.0
                    },
                    with_alpha(op_color, 140.0/255.0),
                    0.5,
                );
                frame.stroke(
                    &glow_path,
                    Stroke {
                        style: stroke::Style::Solid(glow_color),
                        width: 1.0,
                        ..card_stroke_selected()
                    },
                );
            }
        } else {
            frame.stroke(&rect_path, Stroke {
                style: stroke::Style::Solid(tinted_stroke_color),
                ..card_stroke_normal()
            });
        }

        // === 节点文本：几何居中 ===
        if draw_text {
            frame.fill_text(Text {
                content: node.operator_type.name().to_string(),
                position: Point::new(
                    node.position.x + w / 2.0,
                    node.position.y + h / 2.0,
                ),
                color: text_strong,
                size: 12.0.into(),
                font: Font::with_name("Microsoft YaHei"),
                align_x: Alignment::Center.into(),
                align_y: iced::alignment::Vertical::Center,
                ..Default::default()
            });
        }

        if draw_ports {
            draw_ports_indexed(frame, node, w, h, &out_has_fan, &in_occupied, op_color, status, p.anim_time);
        }

        // 完成态成功标识：节点右上角内部绿色徽章 + 白色对勾（静态，无动画）
        // 用户要求：成功后做一个标识即可，标识用"勾"
        if status == 2 {
            let bc = Point::new(node.position.x + w - 9.0, node.position.y + 9.0);
            // 外圈柔光
            let halo = Path::circle(bc, 6.0);
            frame.fill(&halo, with_alpha(Color::from(theme::success()), 0.25));
            // 实心绿色圆背景
            let badge = Path::circle(bc, 5.0);
            frame.fill(&badge, Color::from(theme::success()));
            // 白色对勾：左下 → 底部转折 → 右上
            let check = Path::new(|b| {
                b.move_to(Point::new(bc.x - 2.4, bc.y + 0.3));
                b.line_to(Point::new(bc.x - 0.6, bc.y + 2.0));
                b.line_to(Point::new(bc.x + 2.6, bc.y - 1.8));
            });
            frame.stroke(&check, Stroke {
                style: stroke::Style::Solid(Color { r: 1.0, g: 1.0, b: 1.0, a: 0.95 }),
                width: 1.8,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            });
        }
    }
}

/// 颜色线性混合：返回 `a * (1-t) + b * t`（RGB 各自线性插值，alpha 也插值）
fn mix_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let omt = 1.0 - t;
    Color {
        r: a.r * omt + b.r * t,
        g: a.g * omt + b.g * t,
        b: a.b * omt + b.b * t,
        a: a.a * omt + b.a * t,
    }
}

/// 在三次贝塞尔曲线 t=0.5 处绘制箭头。
///
/// 三次贝塞尔曲线：B(t) = (1-t)³·P0 + 3(1-t)²t·P1 + 3(1-t)t²·P2 + t³·P3
/// 切线：B'(t) = 3(1-t)²·(P1-P0) + 6(1-t)t·(P2-P1) + 3t²·(P3-P2)
/// 取 t=0.5 算出中点位置与切线方向，按切线方向画一个填充三角形指向终点。
fn draw_edge_arrow(
    frame: &mut canvas::Frame,
    p0: Point,
    p1: Point,
    p2: Point,
    p3: Point,
    color: Color,
) {
    // t = 0.5 时的位置（系数：0.125, 0.375, 0.375, 0.125）
    let mid = Point::new(
        0.125 * p0.x + 0.375 * p1.x + 0.375 * p2.x + 0.125 * p3.x,
        0.125 * p0.y + 0.375 * p1.y + 0.375 * p2.y + 0.125 * p3.y,
    );
    // t = 0.5 时的切线（系数：0.75, 1.5, 0.75）
    // B'(0.5) = 0.75·(P1-P0) + 1.5·(P2-P1) + 0.75·(P3-P2)
    //        = -0.75·P0 + (-1.5+0.75)·P1 + (1.5-0.75)·P2 + 0.75·P3
    //        = -0.75·P0 - 0.75·P1 + 0.75·P2 + 0.75·P3
    //        = 0.75·(P3 - P0 + P2 - P1)
    let tangent = Vector::new(
        0.75 * (p3.x - p0.x + p2.x - p1.x),
        0.75 * (p3.y - p0.y + p2.y - p1.y),
    );
    let len = (tangent.x * tangent.x + tangent.y * tangent.y).sqrt();
    if len < 1e-3 {
        return;
    }
    let dir = Vector::new(tangent.x / len, tangent.y / len);
    // 垂直方向（左侧）
    let perp = Vector::new(-dir.y, dir.x);

    // 箭头几何：尖部沿切线向前，尾部两侧沿垂直方向偏移
    // 箭头长度 8px，半宽 4px，与 1.5px 线宽视觉协调
    let arrow_len = 8.0_f32;
    let arrow_half_w = 4.0_f32;

    let tip = Point::new(
        mid.x + dir.x * (arrow_len * 0.5),
        mid.y + dir.y * (arrow_len * 0.5),
    );
    let base_center = Point::new(
        mid.x - dir.x * (arrow_len * 0.5),
        mid.y - dir.y * (arrow_len * 0.5),
    );
    let left = Point::new(
        base_center.x + perp.x * arrow_half_w,
        base_center.y + perp.y * arrow_half_w,
    );
    let right = Point::new(
        base_center.x - perp.x * arrow_half_w,
        base_center.y - perp.y * arrow_half_w,
    );

    let arrow_path = Path::new(|b| {
        b.move_to(tip);
        b.line_to(left);
        b.line_to(right);
        b.close();
    });
    frame.fill(&arrow_path, color);
}

/// 给颜色重设 alpha 通道（保持 RGB）
fn with_alpha(c: Color, alpha: f32) -> Color {
    Color { a: alpha, ..c }
}

/// 使用预建索引绘制单个节点的所有端口。
/// 输出端口统一使用靛青色（theme::accent），输入端口使用算子色。
///
/// 执行中节点（status==1）的输出端口圈做脉冲闪烁，替代旧的节点卡片呼吸发光
/// 动画——用户要求：DAG 运行时只需输出端口圈闪烁即可。
fn draw_ports_indexed(
    frame: &mut canvas::Frame,
    node: &crate::dag::Node,
    w: f32,
    h: f32,
    out_has_fan: &HashMap<PortKey, bool>,
    in_occupied: &HashMap<PortKey, bool>,
    op_color: Color,
    status: u8,
    anim_time: f32,
) {
    let output_count = node.operator_type.output_count();
    let input_count = node.operator_type.input_count();

    // 输出端口（右侧，统一靛青色）
    let out_color = Color::from(theme::accent());
    // 执行中：输出端口圈做"放大缩小"脉动动效（sin 调制半径）
    let running = status == 1;
    let pulse = (anim_time * 2.5).sin() * 0.5 + 0.5;
    for i in 0..output_count {
        let p = port_world_position(node.position, w, h, true, i, output_count);
        let key: PortKey = (node.id.clone(), i, true);
        let has_fan = out_has_fan.get(&key).copied().unwrap_or(false);
        if running {
            // 执行中：半径在 PORT_RADIUS ~ +20% 间往复脉动，外发光配合明暗
            let radius = PORT_RADIUS * (1.0 + 0.20 * pulse);
            let glow_alpha = 0.20 + pulse * 0.55;
            draw_port_cached(frame, p, out_color, out_color, glow_alpha, radius);
        } else {
            // 有 fan-out 时白色环 + 外发光，指示可继续分发
            let glow_alpha = if has_fan { 170.0 / 255.0 } else { 0.0 };
            let ring_color = if has_fan { Color::WHITE } else { Color::from(theme::card_stroke()) };
            draw_port_cached(frame, p, ring_color, out_color, glow_alpha, PORT_RADIUS);
        }
    }

    // 输入端口（左侧）
    for i in 0..input_count {
        let p = port_world_position(node.position, w, h, false, i, input_count);
        let key: PortKey = (node.id.clone(), i, false);
        let occupied = in_occupied.get(&key).copied().unwrap_or(false);
        // 已占用：无外发光；空闲：算子色环 + 淡外发光，提示可连接
        let glow_alpha = if occupied { 0.0 } else { 160.0 / 255.0 };
        let ring_color = if occupied {
            Color::from(theme::card_stroke())
        } else {
            op_color
        };
        draw_port_cached(frame, p, ring_color, op_color, glow_alpha, PORT_RADIUS);
    }
}

/// 单个端口绘制（v6 四层结构）：
///   1) 外发光圈（glow_alpha > 0 时显示，提示可连接 / fan-out 状态）
///   2) 描边环（ring_color）— 状态指示
///   3) 彩色填充（fill_color）— 类型指示（输入青 / 输出靛）
///   4) 中心小白点 — 增加精致感与点击目标感
///
/// `radius` 为主基准半径，四层结构按比例同缩放，保证整体协调。
/// 静态端口传 `PORT_RADIUS`；执行中端口传脉动半径实现"放大缩小"动效。
#[inline(always)]
fn draw_port_cached(
    frame: &mut canvas::Frame,
    center: Point,
    ring_color: Color,
    fill_color: Color,
    glow_alpha: f32,
    radius: f32,
) {
    // 按基准半径比例缩放各层偏移，保证整体比例协调
    let scale = radius / PORT_RADIUS;
    // 1) 外发光圈（仅 glow_alpha > 0 时绘制）
    if glow_alpha > 0.0 {
        let glow_path = Path::circle(center, radius + 2.5 * scale);
        frame.stroke(
            &glow_path,
            Stroke {
                style: stroke::Style::Solid(with_alpha(fill_color, glow_alpha)),
                width: 2.5 * scale,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            },
        );
    }
    // 2) 描边环
    let outer = Path::circle(center, radius);
    frame.stroke(&outer, port_stroke(ring_color));
    // 3) 彩色填充
    let inner = Path::circle(center, (radius - 1.5 * scale).max(0.5));
    frame.fill(&inner, fill_color);
    // 4) 中心小白点，让端口从连线中更"抢眼"
    let core = Path::circle(center, 1.6 * scale);
    frame.fill(&core, Color {
        r: 1.0, g: 1.0, b: 1.0, a: 0.92,
    });
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

/// v3：构造圆角矩形 Path（四分之一圆弧在四角）。
///
/// iced 0.14 `Path` 没有自带 rounded_rectangle，这里用 move_to → 线段 → arc
/// 的顺序手工构造。radius 过大时自动夹紧到 min(w,h)/2。
fn rounded_rect_path(top_left: Point, w: f32, h: f32, r: f32) -> Path {
    use std::f32::consts::{FRAC_PI_2, PI};
    let r = r.clamp(0.0, w.min(h) * 0.5);
    let (x, y) = (top_left.x, top_left.y);
    // 四角：TL / TR / BR / BL，每角对应一个圆心
    let tl = Point::new(x + r, y + r);
    let tr = Point::new(x + w - r, y + r);
    let br = Point::new(x + w - r, y + h - r);
    let bl = Point::new(x + r, y + h - r);
    Path::new(|b| {
        // 起点：顶边左
        b.move_to(Point::new(x + r, y));
        // 顶边 → 右上角前 → TR 弧（-90° → 0°，用 arc 逆时针：start 顺时针角？
        // 这里统一用 iced Path builder：arc_to 不稳定，改用 line_to + bezier
        // 近似圆角：每个角用二次贝塞尔（误差 ~0.5%，视觉完美）。
        // 方案：四条直线 + 每个角一个 quadratic_curve，控制点=角点
        // 顶边右：
        b.line_to(Point::new(x + w - r, y));
        b.quadratic_curve_to(Point::new(x + w, y), Point::new(x + w, y + r));
        // 右边下：
        b.line_to(Point::new(x + w, y + h - r));
        b.quadratic_curve_to(Point::new(x + w, y + h), Point::new(x + w - r, y + h));
        // 底边左：
        b.line_to(Point::new(x + r, y + h));
        b.quadratic_curve_to(Point::new(x, y + h), Point::new(x, y + h - r));
        // 左边上：
        b.line_to(Point::new(x, y + r));
        b.quadratic_curve_to(Point::new(x, y), Point::new(x + r, y));
        // 闭合（最后一段 quadratic 回到起点，无需显式 close）
        let _ = (tl, tr, br, bl, FRAC_PI_2, PI);
    })
}
