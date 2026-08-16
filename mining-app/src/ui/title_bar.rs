//! 自定义标题栏：Logo 矢量动画 + 应用名 + 拖拽区 + 窗口控制按钮。
//!
//! 由于 iced::Application 设置 `decorations: false` 后窗口没有原生标题栏，
//! 必须自己绘制：
//! - 左侧：Logo（Canvas Program 重绘 icon.rs 的渐变折线 + 末点呼吸动画）
//! - 中间：应用名 "青萝" + 大段可拖拽空白区（mouse_area on_press → WindowDrag）
//! - 右侧：最小化 / 最大化·还原 / 关闭 三个按钮
//!
//! iced 0.14 关键 API：
//! - `iced::widget::mouse_area`：可以监听鼠标 press/release/移动，不影响子 widget 布局
//! - `iced::widget::canvas::canvas(program)` + `Program::draw`：自定义矢量绘制
//! - `window::drag()/close(id)/toggle_maximize(id)/minimize(id)`：在 update 中调用

use iced::widget::canvas;
use iced::widget::canvas::stroke::{self, Stroke};
use iced::widget::canvas::{Geometry, LineCap, Path};
use iced::{
    Alignment, Color, Element, Font, Length, Padding, Rectangle, Theme, Vector,
    mouse, Renderer,
    widget::{container, row, text},
};

use super::state::{Message, UiState};
use super::theme;

/// 标题栏高度（像素）。
const TITLE_BAR_HEIGHT: f32 = 32.0;
/// Logo 画布尺寸（正方形边长）。
const LOGO_SIZE: f32 = 20.0;
/// 标题栏右侧三个按钮的宽度。
const CTRL_BTN_WIDTH: f32 = 46.0;
/// 标题栏底部分隔线高度。
const DIVIDER_HEIGHT: f32 = 1.0;

/// 渲染整个标题栏。
///
/// 调用方：把此函数返回的 Element 放到主 column 顶部，body 之前。
pub fn view_title_bar(state: &UiState) -> Element<'_, Message> {
    // 左侧：Logo Canvas
    let logo = canvas(LogoProgram {
        time: state.logo_time,
    })
    .width(Length::Fixed(LOGO_SIZE))
    .height(Length::Fixed(LOGO_SIZE));

    // 应用名
    let title = text("青萝")
        .color(theme::TEXT_STRONG)
        .font(Font::with_name("Microsoft YaHei"))
        .size(13.0);

    // 左侧组合：Logo + 应用名
    let left = row![logo, title]
        .spacing(8)
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 8.0,
            right: 12.0,
        });

    // 右侧：三个控制按钮
    let min_btn = control_button("—", Message::WindowMinimize, false);
    let max_btn = control_button("▢", Message::WindowToggleMaximize, false);
    let close_btn = control_button("✕", Message::WindowClose, true);

    let right = row![min_btn, max_btn, close_btn]
        .align_y(Alignment::Center)
        .height(Length::Fill);

    // 中间填充拖拽区：用 mouse_area 包住一个空 row，让它在按下时发 WindowDrag
    // 关键：mouse_area 必须在按钮的外层，按钮的事件会先被 button 处理，不会冒泡到 mouse_area
    let drag_area = iced::widget::mouse_area(
        row![]
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Message::WindowDrag);

    let bar = row![left, drag_area, right]
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT))
        .align_y(Alignment::Center);

    // 标题栏容器：深色背景
    let bar_cont = container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT))
        .style(|_theme| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::TITLE_BAR_BG).into());
            s
        });

    // 底部 1px 分隔线（iced 0.14 Border 没有 per-side 字段，用独立 container 实现）
    let divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(DIVIDER_HEIGHT))
        .style(|_theme| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::DIVIDER).into());
            s
        });

    iced::widget::column![bar_cont, divider]
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT + DIVIDER_HEIGHT))
        .into()
}

/// 渲染一个窗口控制按钮：极简、无边框、悬停时背景高亮。
///
/// `is_close` 为 true 时表示关闭按钮，悬停背景为红色（视觉警示）。
/// 此参数单独传入而非从 msg 推断，是因为 `Message` 未实现 `Copy`，
/// `on_press(msg)` 会消耗 msg，无法在 style 闭包中再引用。
fn control_button(
    label: &'static str,
    msg: Message,
    is_close: bool,
) -> Element<'static, Message> {
    let btn = iced::widget::button(
        text(label)
            .color(theme::TEXT_STRONG)
            .size(12.0),
    )
    .on_press(msg)
    .width(Length::Fixed(CTRL_BTN_WIDTH))
    .height(Length::Fixed(TITLE_BAR_HEIGHT))
    .style(move |_theme, status| {
        let mut s = iced::widget::button::Style::default();
        s.background = Some(Color::TRANSPARENT.into());
        s.text_color = theme::TEXT_STRONG;
        if matches!(status, iced::widget::button::Status::Hovered) {
            // 关闭按钮悬停红色，其它按钮灰色
            let bg = if is_close {
                Color::from_rgb8(232, 65, 68) // #E84144
            } else {
                Color::from(theme::HOVER_BG)
            };
            s.background = Some(bg.into());
            if is_close {
                s.text_color = Color::WHITE;
            }
        }
        s
    });
    btn.into()
}

// ===== Logo Canvas Program =====
//
// 复刻 icon.rs::create_app_icon 的设计：上升趋势折线图 + 数据点光晕，
// 蓝→绿渐变 (#007AFF → #34C759)。增加：末点 (18,-18) 在 y 方向做缓慢
// 呼吸动画，让 logo 有"生命感"。
//
// iced 0.14 canvas::Program 的 draw 签名：
// `fn draw(&self, renderer: &Renderer, _theme: &Theme, bounds: &Rectangle,
//          cursor: mouse::Cursor) -> Vec<Geometry>`
// 这里用一个内部 Cache 持有上次绘制的 Geometry，bounds 不变就复用。

#[derive(Clone)]
struct LogoProgram {
    time: f32,
}

impl<Message> canvas::Program<Message> for LogoProgram {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        // 设计坐标系：24×24，缩放到画布
        let design = 24.0f32;
        let size = bounds.size();
        let s = size.width.min(size.height) / design;
        let origin = Vector::new(
            bounds.position().x + (size.width - design * s) / 2.0,
            bounds.position().y + (size.height - design * s) / 2.0,
        );
        // 把 frame 的原点移到设计坐标 (0,0)，让 px 函数直接用设计坐标
        frame.translate(origin);

        // 设计坐标 → 画布坐标（原点左上，y 向下；dy 为负表示向上）
        let px = |dx: f32, dy: f32| -> iced::Point {
            iced::Point::new(dx * s, (design / 2.0 + dy) * s)
        };

        // 折线点（与 icon.rs 一致，末点加呼吸动画）
        // 呼吸：末点 y 在 -18 基础上加 ±1.5 的正弦摆动
        let breathe = (self.time * 1.5).sin() * 1.5;
        let pts = [
            px(2.0, -2.0),
            px(6.0, -4.0),
            px(8.0, -3.0),
            px(10.0, -10.0),
            px(14.0, -9.0),
            px(18.0, -18.0 + breathe),
        ];

        let start_color = Color::from_rgba8(0, 122, 255, 1.0);  // #007AFF
        let end_color = Color::from_rgba8(52, 199, 89, 1.0);    // #34C759

        // 1) 阴影线：向右下偏移 1 设计单位，黑色低透明
        let shadow_path = Path::new(|b| {
            b.move_to(iced::Point::new(pts[0].x + s, pts[0].y + s));
            for p in &pts[1..] {
                b.line_to(iced::Point::new(p.x + s, p.y + s));
            }
        });
        frame.stroke(
            &shadow_path,
            Stroke {
                style: stroke::Style::Solid(Color::from_rgba8(0, 0, 0, 60.0 / 255.0)),
                width: 2.0 * s,
                line_cap: LineCap::Round,
                ..Default::default()
            },
        );

        // 2) 渐变折线：分段绘制（每段独立颜色）
        for i in 0..pts.len() - 1 {
            let progress = i as f32 / (pts.len() - 2) as f32;
            let color = lerp_color(start_color, end_color, progress);
            let path = Path::new(|b| {
                b.move_to(pts[i]);
                b.line_to(pts[i + 1]);
            });
            frame.stroke(
                &path,
                Stroke {
                    style: stroke::Style::Solid(color),
                    width: 2.5 * s,
                    line_cap: LineCap::Round,
                    ..Default::default()
                },
            );
        }

        // 3) 数据点：外圈光晕 + 内圈实心点
        for (i, &p) in pts.iter().enumerate() {
            let progress = i as f32 / (pts.len() - 1) as f32;
            let color = lerp_color(start_color, end_color, progress);
            // 外圈光晕
            let glow_alpha = (100.0 * (1.0 - progress) / 255.0).clamp(0.0, 1.0);
            let glow = Path::circle(p, 3.0 * s);
            frame.fill(
                &glow,
                Color::from_rgba(
                    color.r,
                    color.g,
                    color.b,
                    glow_alpha,
                ),
            );
            // 内圈实心点
            let inner = Path::circle(p, 2.0 * s);
            frame.fill(&inner, color);
        }

        vec![frame.into_geometry()]
    }
}

/// RGB 颜色线性插值（保留 alpha = 1.0）
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::from_rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        1.0,
    )
}
