//! 应用图标：纯代码绘制，零外部资源依赖
//!
//! 设计说明：
//! - 128×128 RGBA 位图，运行时由代码直接栅格化，启动即用。
//!   不再依赖 `resource/logo_icon.bin`，也不需要 resvg / usvg / cairosvg 等栅格化工具。
//! - 复刻标题栏 `main.rs::render_logo` 的设计：上升趋势折线图 + 数据点光晕，
//!   呈"数据挖掘/分析"的上升趋势意象，契合"挖掘分析"主题。
//! - 配色沿用蓝→绿渐变（#007AFF → #34C759），与标题栏 logo 视觉一致。
//! - 设计稿基于 24×24 坐标系，按比例缩放到 128×128 画布；图标为静态帧（无动画）。
//! - 对外接口 `create_app_icon()` 保持不变，调用方无需改动。

/// 图标边长（像素）
const ICON_SIZE: u32 = 128;

/// 创建应用图标（128×128 RGBA）。
///
/// 从 egui 迁移到 Iced 后，返回类型由 `egui::IconData` 改为
/// `Option<iced::window::Icon>`：内部仍按 24×24 设计坐标系栅格化 RGBA，
/// 再交给 `iced::window::icon::from_rgba` 构造窗口图标。
pub fn create_app_icon() -> Option<iced::window::Icon> {
    // 与 main.rs::render_logo 保持一致的 24×24 设计坐标系
    const LOGO_DESIGN_SIZE: f32 = 24.0;
    let canvas = ICON_SIZE as f32;
    let s = canvas / LOGO_DESIGN_SIZE; // 缩放因子
    let center = [canvas / 2.0, canvas / 2.0];
    let half_size = canvas * 0.8 / 2.0; // 内容区占 80%

    // 配色（与 main.rs::render_logo 一致）
    let start_color = [0.0f32, 122.0 / 255.0, 1.0, 1.0]; // #007AFF
    let end_color = [52.0 / 255.0, 199.0 / 255.0, 89.0 / 255.0, 1.0]; // #34C759

    // 设计坐标 → 画布像素（原点左上，y 向下；dy 为负表示向上）
    let px = |dx: f32, dy: f32| -> [f32; 2] {
        [center[0] - half_size + dx * s, center[1] + half_size + dy * s]
    };

    // 折线图形状（上升趋势），与 render_logo 静态点一致（图标无动画偏移）
    // 末尾新增顶部突破点 (18, -16)：在原峰值正上方，象征数据挖掘的“发现峰值”，
    // 落点 progress=1.0 恰为纯绿 #34C759，作为趋势收尾。
    let points = [
        px(2.0, -2.0),
        px(6.0, -4.0),
        px(8.0, -3.0),
        px(10.0, -10.0),
        px(14.0, -9.0),
        px(18.0, -18.0),
    ];

    let mut p = IconPainter::new(ICON_SIZE as usize);

    // 1) 折线阴影：向右下偏移 1 个设计单位，黑色低透明
    let shadow_offset = 1.0 * s;
    let shadow_color = [0.0f32, 0.0, 0.0, 60.0 / 255.0];
    let shadow_pts: Vec<[f32; 2]> = points
        .iter()
        .map(|pt| [pt[0] + shadow_offset, pt[1] + shadow_offset])
        .collect();
    for i in 0..shadow_pts.len() - 1 {
        p.line(shadow_pts[i], shadow_pts[i + 1], 2.0 * s, shadow_color);
    }

    // 2) 渐变折线：分段绘制，颜色沿蓝→绿渐变
    let n = points.len();
    for i in 0..n - 1 {
        let progress = i as f32 / (n - 2) as f32;
        let color = lerp_rgba(start_color, end_color, progress);
        p.line(points[i], points[i + 1], 2.5 * s, color);
    }

    // 3) 数据点：外圈光晕 + 内圈实心点，颜色随进度渐变
    for (i, &point) in points.iter().enumerate() {
        let progress = i as f32 / (n - 1) as f32;
        let color = lerp_rgba(start_color, end_color, progress);

        // 外圈光晕（随进度衰减）
        let glow_alpha = (100.0 * (1.0 - progress) / 255.0).clamp(0.0, 1.0);
        let glow_color = [color[0], color[1], color[2], glow_alpha];
        p.circle_filled(point, 3.0 * s, glow_color);

        // 内圈实心点
        p.circle_filled(point, 2.0 * s, color);
    }

    iced::window::icon::from_rgba(p.into_bytes(), ICON_SIZE, ICON_SIZE).ok()
}

/// RGBA 颜色线性插值（未预乘，0..1）
fn lerp_rgba(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// 极简软件光栅化器：在 RGBA 缓冲区上绘制带抗锯齿的线段/圆。
struct IconPainter {
    size: usize,
    buf: Vec<u8>, // RGBA，行优先，原点左上
}

impl IconPainter {
    fn new(size: usize) -> Self {
        Self {
            size,
            buf: vec![0u8; size * size * 4],
        }
    }

    /// 把 src 以 src-over 方式合成到 dst 上（均按未预乘 0..1 处理）
    #[inline]
    fn blend(&mut self, x: usize, y: usize, src: [f32; 4]) {
        if src[3] <= 0.0 {
            return;
        }
        let i = (y * self.size + x) * 4;
        let dst = [
            self.buf[i] as f32 / 255.0,
            self.buf[i + 1] as f32 / 255.0,
            self.buf[i + 2] as f32 / 255.0,
            self.buf[i + 3] as f32 / 255.0,
        ];
        let sa = src[3];
        let out_a = sa + dst[3] * (1.0 - sa);
        if out_a <= 0.0 {
            return;
        }
        let out_rgb = |c: f32, d: f32| (c * sa + d * dst[3] * (1.0 - sa)) / out_a;
        let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        self.buf[i] = to_u8(out_rgb(src[0], dst[0]));
        self.buf[i + 1] = to_u8(out_rgb(src[1], dst[1]));
        self.buf[i + 2] = to_u8(out_rgb(src[2], dst[2]));
        self.buf[i + 3] = to_u8(out_a);
    }

    /// 绘制带圆头的线段（宽度 width，1px 抗锯齿）
    fn line(&mut self, p0: [f32; 2], p1: [f32; 2], width: f32, color: [f32; 4]) {
        let half = width / 2.0;
        let ab = [p1[0] - p0[0], p1[1] - p0[1]];
        let len2 = ab[0] * ab[0] + ab[1] * ab[1];

        let (min_x, max_x, min_y, max_y) = self.bbox(&[p0, p1], half + 1.0);
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let q = [x as f32 + 0.5, y as f32 + 0.5];
                // 点到线段的距离
                let dist = if len2 > 0.0 {
                    let t = ((q[0] - p0[0]) * ab[0] + (q[1] - p0[1]) * ab[1]) / len2;
                    let t = t.clamp(0.0, 1.0);
                    let cx = p0[0] + ab[0] * t;
                    let cy = p0[1] + ab[1] * t;
                    ((q[0] - cx).powi(2) + (q[1] - cy).powi(2)).sqrt()
                } else {
                    ((q[0] - p0[0]).powi(2) + (q[1] - p0[1]).powi(2)).sqrt()
                };
                let cov = (half + 0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, [color[0], color[1], color[2], color[3] * cov]);
                }
            }
        }
    }

    /// 绘制实心圆（半径 radius）
    fn circle_filled(&mut self, c: [f32; 2], radius: f32, color: [f32; 4]) {
        let (min_x, max_x, min_y, max_y) = self.bbox(&[c], radius + 1.0);
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let q = [x as f32 + 0.5, y as f32 + 0.5];
                let d = ((q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2)).sqrt();
                let cov = (radius + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, [color[0], color[1], color[2], color[3] * cov]);
                }
            }
        }
    }

    /// 计算给定点的包围盒（裁剪到画布内），返回 (min_x, max_x, min_y, max_y)
    fn bbox(&self, pts: &[[f32; 2]], pad: f32) -> (usize, usize, usize, usize) {
        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        for p in pts {
            min_x = min_x.min(p[0]);
            max_x = max_x.max(p[0]);
            min_y = min_y.min(p[1]);
            max_y = max_y.max(p[1]);
        }
        let last = self.size as f32 - 1.0;
        (
            (min_x - pad).max(0.0).floor() as usize,
            (max_x + pad).min(last).ceil() as usize,
            (min_y - pad).max(0.0).floor() as usize,
            (max_y + pad).min(last).ceil() as usize,
        )
    }

    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}
