//! 自绘矢量图标模块：基于 `iced::widget::canvas` 在 24×24 设计坐标系中绘制
//! 全套应用图标，零外部资源依赖，匹配用户偏好（轻量 / 2D / 无字体文件）。
//!
//! 设计原则：
//! - 每个图标都是少量 Path 的 fill/stroke 组合，几何干净，与现有暗色主题一致
//! - 通过 `Icon { kind, color, stroke_width }` 携带渲染参数；Program 的
//!   `State` 内部用 `RefCell` 持有"上次 (color, size)"指纹，配合 `Cache` 命中判断：
//!   颜色或尺寸变化时先 `clear()` 强制重建，避免 hover/激活态切换时画面冻结
//! - `view_icon(kind, color, size)` 是对外唯一入口，返回固定尺寸的 canvas Element，
//!   调用方按需在外层 `container` 中做对齐/留白

use std::cell::RefCell;

use iced::widget::canvas;
use iced::widget::canvas::stroke::{self, Stroke};
use iced::widget::canvas::{Action, Event, Geometry, LineCap, LineJoin, Path};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use super::state::Message;

// ===== 设计画布尺寸（所有图标在 24×24 坐标系中绘制） =====
const DESIGN_SIZE: f32 = 24.0;
/// 默认描边宽度（参考 Feather/Lucide 系列的 1.5~2.0 px 描边美学）
const DEFAULT_STROKE: f32 = 1.6;

/// 图标种类枚举。每个变体对应 `draw_*` 分发函数中的一段几何绘制。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconKind {
    /// 挖掘（活动栏）：三根递增柱状图，象征数据分析 / 数据挖掘
    Mining,
    /// 算子（活动栏）：齿轮，象征算子/运算单元
    Operator,
    /// 设置（活动栏）：三档滑杆 + 圆钮，象征配置项
    Settings,
    /// 保存（悬浮工具栏）：软盘，象征写入磁盘
    Save,
    /// 执行 DAG（悬浮工具栏）：实心播放三角形，主操作
    Run,
    /// 调试切换（悬浮工具栏）：虫子，象征 debug
    Debug,
    /// 建模列表项图标：折角文档，象征一份建模文件
    Model,
    /// 重命名按钮：铅笔，象征编辑
    Pencil,
    /// 删除按钮：垃圾桶（避免使用 ✕ 叉号）
    Trash,
    /// 算子面板搜索框：放大镜
    Search,
    /// 新建模按钮：加号
    Plus,
    /// 空状态装饰：菱形轮廓
    Diamond,
    /// 新建模对话框头部：四角星（实心），象征"新建/魔法"
    Sparkle,
    /// 重命名对话框头部：铅笔（与 Pencil 一致，留给语义清晰的调用方）
    Edit,
    /// 删除确认对话框头部：三角警告 + 感叹号
    Warning,
}

/// 自绘图标的渲染参数：种类 + 颜色 + 描边宽度。
#[derive(Clone, Copy)]
pub struct Icon {
    pub kind: IconKind,
    pub color: Color,
    pub stroke_width: f32,
}

impl PartialEq for Icon {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.color == other.color
            && self.stroke_width == other.stroke_width
    }
}

/// Canvas Program 的内部状态：`Cache` + 上次绘制指纹（颜色 / 尺寸）。
///
/// iced 0.14 canvas 的 `State` 生命周期绑定到 widget 实例，Program 每帧被
/// 重新构造（携带新 color/kind），但 State 跨帧保留。因此需要在 `draw` 中
/// 比较新旧指纹并按需 `cache.clear()`，避免 hover/激活态切换时颜色不刷新。
pub struct IconState {
    cache: canvas::Cache,
    inner: RefCell<IconFingerprint>,
}

#[derive(Clone, Copy, PartialEq)]
struct IconFingerprint {
    last_color: Color,
    last_size: Size,
}

impl Default for IconState {
    fn default() -> Self {
        Self {
            cache: canvas::Cache::default(),
            inner: RefCell::new(IconFingerprint {
                last_color: Color::TRANSPARENT,
                last_size: Size::ZERO,
            }),
        }
    }
}

impl canvas::Program<Message> for Icon {
    type State = IconState;

    fn update(
        &self,
        _state: &mut IconState,
        _event: &Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        // 图标不响应任何鼠标 / 键盘事件，事件全部交给上层处理
        None
    }

    fn draw(
        &self,
        state: &IconState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let size = bounds.size();
        // 指纹检测：颜色或尺寸任一变化即清缓存重建
        let needs_rebuild = {
            let inner = state.inner.borrow();
            inner.last_color != self.color || inner.last_size != size
        };
        if needs_rebuild {
            state.cache.clear();
            if let Ok(mut inner) = state.inner.try_borrow_mut() {
                inner.last_color = self.color;
                inner.last_size = size;
            }
        }

        let geo = state.cache.draw(renderer, size, |frame| {
            // 缩放：把 24×24 设计坐标映射到当前 canvas 像素尺寸
            let scale = (size.width.min(size.height) / DESIGN_SIZE).max(0.01);
            frame.scale(scale);
            draw_icon_kind(frame, self.kind, self.color, self.stroke_width);
        });
        vec![geo]
    }
}

/// 对外入口：构造固定尺寸的图标 Element。
///
/// 调用方按需在外层 `container` 中做对齐 / 留白 / 背景色块。
pub fn view_icon(kind: IconKind, color: Color, size: f32) -> Element<'static, Message> {
    view_icon_with_stroke(kind, color, size, DEFAULT_STROKE)
}

/// 同上，但允许指定描边宽度（用于填充型 / 描边型图标的统一调用）。
pub fn view_icon_with_stroke(
    kind: IconKind,
    color: Color,
    size: f32,
    stroke_width: f32,
) -> Element<'static, Message> {
    let icon = Icon {
        kind,
        color,
        stroke_width,
    };
    canvas(icon)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

// ===== 分发：根据 IconKind 调用对应的绘制函数 =====

fn draw_icon_kind(frame: &mut canvas::Frame, kind: IconKind, color: Color, sw: f32) {
    match kind {
        IconKind::Mining => draw_mining(frame, color, sw),
        IconKind::Operator => draw_operator(frame, color, sw),
        IconKind::Settings => draw_settings(frame, color, sw),
        IconKind::Save => draw_save(frame, color, sw),
        IconKind::Run => draw_run(frame, color),
        IconKind::Debug => draw_debug(frame, color, sw),
        IconKind::Model => draw_model(frame, color, sw),
        IconKind::Pencil => draw_pencil(frame, color, sw),
        IconKind::Trash => draw_trash(frame, color, sw),
        IconKind::Search => draw_search(frame, color, sw),
        IconKind::Plus => draw_plus(frame, color, sw),
        IconKind::Diamond => draw_diamond(frame, color, sw),
        IconKind::Sparkle => draw_sparkle(frame, color),
        IconKind::Edit => draw_pencil(frame, color, sw),
        IconKind::Warning => draw_warning(frame, color, sw),
    }
}

// ===== 描边辅助函数 =====

fn solid_stroke(color: Color, width: f32) -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(color),
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Default::default()
    }
}

// ===== 单个图标绘制 =====

/// 挖掘：三根递增柱状图 + 基线
fn draw_mining(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 三根柱子（从左到右递增），用填充矩形表达"数据增长"
    let bar_w = 3.0_f32;
    let bottom = 19.5_f32;
    let bars = [
        (5.0_f32, 13.5_f32),
        (11.0, 9.0),
        (17.0, 4.5),
    ];
    for (x, top) in bars {
        let bar = Path::rectangle(
            Point::new(x - bar_w / 2.0, top),
            Size::new(bar_w, bottom - top),
        );
        frame.fill(&bar, color);
    }
    // 基线（柱状图底部参考线）
    let baseline = Path::new(|b| {
        b.move_to(Point::new(3.5, 20.5));
        b.line_to(Point::new(20.5, 20.5));
    });
    frame.stroke(&baseline, stroke);
}

/// 算子：8 齿齿轮 + 中心圆孔
fn draw_operator(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    let center = Point::new(12.0, 12.0);
    let r_outer = 8.5_f32;
    let r_inner = 6.5_f32;
    let r_hole = 3.6_f32;
    let n_teeth: usize = 8;

    // 齿轮外轮廓：每齿四段（外→外→内→内），共 n_teeth*4 个顶点
    let gear = Path::new(|b| {
        let steps_per_tooth = 4;
        let total = n_teeth * steps_per_tooth;
        for i in 0..total {
            let phase = i as f32 / total as f32;
            let angle = phase * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            // 齿顶两段用 r_outer，齿根两段用 r_inner
            let r = if i % steps_per_tooth < 2 { r_outer } else { r_inner };
            let x = center.x + r * angle.cos();
            let y = center.y + r * angle.sin();
            if i == 0 {
                b.move_to(Point::new(x, y));
            } else {
                b.line_to(Point::new(x, y));
            }
        }
        b.close();
    });
    frame.stroke(&gear, stroke);

    // 中心圆孔
    let hole = Path::circle(center, r_hole);
    frame.stroke(&hole, stroke);
}

/// 设置：三档滑杆 + 圆钮
fn draw_settings(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    let ys = [6.0_f32, 12.0, 18.0];
    let knob_xs = [15.0_f32, 8.0, 16.0];
    for (i, &y) in ys.iter().enumerate() {
        let line = Path::new(|b| {
            b.move_to(Point::new(4.0, y));
            b.line_to(Point::new(20.0, y));
        });
        frame.stroke(&line, stroke);
        let knob = Path::circle(Point::new(knob_xs[i], y), 2.2);
        frame.fill(&knob, color);
    }
}

/// 保存：软盘轮廓 + 顶部标签条 + 底部磁盘窗
fn draw_save(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 外框
    let outer = Path::rectangle(Point::new(4.0, 4.0), Size::new(16.0, 16.0));
    frame.stroke(&outer, stroke);
    // 顶部标签分隔线
    let sep = Path::new(|b| {
        b.move_to(Point::new(4.0, 9.5));
        b.line_to(Point::new(20.0, 9.5));
    });
    frame.stroke(&sep, stroke);
    // 顶部右侧的金属滑片切口
    let notch = Path::rectangle(Point::new(14.5, 4.0), Size::new(5.5, 4.5));
    frame.stroke(&notch, stroke);
    // 底部磁盘窗
    let disk = Path::rectangle(Point::new(7.0, 12.5), Size::new(10.0, 7.5));
    frame.stroke(&disk, stroke);
}

/// 执行：实心播放三角形
fn draw_run(frame: &mut canvas::Frame, color: Color) {
    let path = Path::new(|b| {
        b.move_to(Point::new(8.0, 5.0));
        b.line_to(Point::new(8.0, 19.0));
        b.line_to(Point::new(19.5, 12.0));
        b.close();
    });
    frame.fill(&path, color);
}

/// 调试：虫子（身躯 + 头 + 触角 + 六条腿 + 背中线）
fn draw_debug(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 身躯（圆角矩形）
    let body = Path::rectangle(Point::new(9.0, 8.0), Size::new(6.0, 12.0));
    frame.stroke(&body, stroke);
    // 头部圆
    let head = Path::circle(Point::new(12.0, 6.0), 2.2);
    frame.stroke(&head, stroke);
    // 触角
    let ant1 = Path::new(|b| {
        b.move_to(Point::new(10.8, 4.5));
        b.line_to(Point::new(8.5, 2.5));
    });
    frame.stroke(&ant1, stroke);
    let ant2 = Path::new(|b| {
        b.move_to(Point::new(13.2, 4.5));
        b.line_to(Point::new(15.5, 2.5));
    });
    frame.stroke(&ant2, stroke);
    // 六条腿
    let legs = [
        (Point::new(9.0, 11.0), Point::new(5.0, 9.0)),
        (Point::new(9.0, 14.0), Point::new(5.0, 14.0)),
        (Point::new(9.0, 17.0), Point::new(5.0, 19.0)),
        (Point::new(15.0, 11.0), Point::new(19.0, 9.0)),
        (Point::new(15.0, 14.0), Point::new(19.0, 14.0)),
        (Point::new(15.0, 17.0), Point::new(19.0, 19.0)),
    ];
    for (p1, p2) in legs.iter() {
        let leg = Path::new(|b| {
            b.move_to(*p1);
            b.line_to(*p2);
        });
        frame.stroke(&leg, stroke);
    }
    // 背中线
    let back = Path::new(|b| {
        b.move_to(Point::new(12.0, 8.0));
        b.line_to(Point::new(12.0, 20.0));
    });
    frame.stroke(&back, stroke);
}

/// 建模：折角文档 + 两条内容线
fn draw_model(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 文档外轮廓（右上角折角）
    let path = Path::new(|b| {
        b.move_to(Point::new(6.0, 3.0));
        b.line_to(Point::new(14.5, 3.0));
        b.line_to(Point::new(18.0, 6.5));
        b.line_to(Point::new(18.0, 21.0));
        b.line_to(Point::new(6.0, 21.0));
        b.close();
    });
    frame.stroke(&path, stroke);
    // 折角内三角（视觉上的"折起"）
    let fold = Path::new(|b| {
        b.move_to(Point::new(14.5, 3.0));
        b.line_to(Point::new(14.5, 6.5));
        b.line_to(Point::new(18.0, 6.5));
    });
    frame.stroke(&fold, stroke);
    // 内容线 1
    let l1 = Path::new(|b| {
        b.move_to(Point::new(9.0, 12.0));
        b.line_to(Point::new(15.0, 12.0));
    });
    frame.stroke(&l1, stroke);
    // 内容线 2
    let l2 = Path::new(|b| {
        b.move_to(Point::new(9.0, 16.0));
        b.line_to(Point::new(15.0, 16.0));
    });
    frame.stroke(&l2, stroke);
}

/// 铅笔：斜置笔身 + 笔尖三角
fn draw_pencil(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 笔身（斜置矩形）
    let body = Path::new(|b| {
        b.move_to(Point::new(17.0, 4.0));
        b.line_to(Point::new(19.5, 6.5));
        b.line_to(Point::new(8.5, 17.5));
        b.line_to(Point::new(6.0, 15.0));
        b.close();
    });
    frame.stroke(&body, stroke);
    // 笔尖（左下三角，填充以强化"尖"的视觉）
    let tip = Path::new(|b| {
        b.move_to(Point::new(8.5, 17.5));
        b.line_to(Point::new(6.0, 15.0));
        b.line_to(Point::new(4.5, 19.5));
        b.close();
    });
    frame.fill(&tip, color);
}

/// 垃圾桶：桶身 + 桶盖 + 把手 + 两条内部竖线（明确不用 ✕ 叉号）
fn draw_trash(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 桶盖（顶部横线）
    let lid = Path::new(|b| {
        b.move_to(Point::new(4.0, 7.0));
        b.line_to(Point::new(20.0, 7.0));
    });
    frame.stroke(&lid, stroke);
    // 把手（顶部小弧形开口）
    let handle = Path::new(|b| {
        b.move_to(Point::new(9.5, 7.0));
        b.line_to(Point::new(9.5, 4.5));
        b.line_to(Point::new(14.5, 4.5));
        b.line_to(Point::new(14.5, 7.0));
    });
    frame.stroke(&handle, stroke);
    // 桶身（轻微梯形：上宽下窄，更像垃圾桶）
    let body = Path::new(|b| {
        b.move_to(Point::new(6.5, 7.0));
        b.line_to(Point::new(7.5, 20.0));
        b.line_to(Point::new(16.5, 20.0));
        b.line_to(Point::new(17.5, 7.0));
    });
    frame.stroke(&body, stroke);
    // 两条内部竖线（桶身的褶皱）
    let l1 = Path::new(|b| {
        b.move_to(Point::new(10.0, 10.0));
        b.line_to(Point::new(10.5, 17.0));
    });
    frame.stroke(&l1, stroke);
    let l2 = Path::new(|b| {
        b.move_to(Point::new(14.0, 10.0));
        b.line_to(Point::new(13.5, 17.0));
    });
    frame.stroke(&l2, stroke);
}

/// 搜索：放大镜
fn draw_search(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 镜片圆
    let lens = Path::circle(Point::new(10.5, 10.5), 6.0);
    frame.stroke(&lens, stroke);
    // 手柄
    let handle = Path::new(|b| {
        b.move_to(Point::new(14.8, 14.8));
        b.line_to(Point::new(20.0, 20.0));
    });
    frame.stroke(&handle, stroke);
}

/// 加号：两根相交直线
fn draw_plus(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    let h = Path::new(|b| {
        b.move_to(Point::new(5.0, 12.0));
        b.line_to(Point::new(19.0, 12.0));
    });
    frame.stroke(&h, stroke);
    let v = Path::new(|b| {
        b.move_to(Point::new(12.0, 5.0));
        b.line_to(Point::new(12.0, 19.0));
    });
    frame.stroke(&v, stroke);
}

/// 空状态：菱形轮廓
fn draw_diamond(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    let path = Path::new(|b| {
        b.move_to(Point::new(12.0, 3.0));
        b.line_to(Point::new(21.0, 12.0));
        b.line_to(Point::new(12.0, 21.0));
        b.line_to(Point::new(3.0, 12.0));
        b.close();
    });
    frame.stroke(&path, stroke);
}

/// 新建模装饰：实心四角星
fn draw_sparkle(frame: &mut canvas::Frame, color: Color) {
    let path = Path::new(|b| {
        b.move_to(Point::new(12.0, 3.0));
        b.line_to(Point::new(13.8, 10.2));
        b.line_to(Point::new(21.0, 12.0));
        b.line_to(Point::new(13.8, 13.8));
        b.line_to(Point::new(12.0, 21.0));
        b.line_to(Point::new(10.2, 13.8));
        b.line_to(Point::new(3.0, 12.0));
        b.line_to(Point::new(10.2, 10.2));
        b.close();
    });
    frame.fill(&path, color);
}

/// 警告：三角外框 + 感叹号
fn draw_warning(frame: &mut canvas::Frame, color: Color, sw: f32) {
    let stroke = solid_stroke(color, sw);
    // 三角外框
    let tri = Path::new(|b| {
        b.move_to(Point::new(12.0, 3.5));
        b.line_to(Point::new(21.0, 19.5));
        b.line_to(Point::new(3.0, 19.5));
        b.close();
    });
    frame.stroke(&tri, stroke);
    // 感叹号竖线
    let bar = Path::new(|b| {
        b.move_to(Point::new(12.0, 9.0));
        b.line_to(Point::new(12.0, 14.5));
    });
    frame.stroke(&bar, stroke);
    // 感叹号圆点
    let dot = Path::circle(Point::new(12.0, 17.0), 0.9);
    frame.fill(&dot, color);
}

// 静态断言：保证模块在编译期捕获未使用的导入（避免误删 import 后无声漂移）
#[allow(dead_code)]
fn _imports_anchor() {
    let _ = LineCap::Round;
    let _ = LineJoin::Round;
}
