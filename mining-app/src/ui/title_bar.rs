//! 自定义标题栏 v2：精致 Logo 渐变动画 + 应用名 + 拖拽区 + 圆角窗口控制按钮。
//!
//! 设计改进：
//! - 高度提升至 40px，呼吸感更好
//! - Logo 尺寸增大，末点用青蓝 (Cyan) 替代纯绿，更贴合 v2 配色
//! - 控制按钮圆角胶囊样式，关闭按钮 hover 渐变色
//! - 分隔线改为渐变弱化条

use iced::widget::canvas;
use iced::widget::canvas::stroke::{self, Stroke};
use iced::widget::canvas::{Geometry, LineCap, Path};
use iced::{
    Alignment, Color, Element, Font, Length, Padding, Rectangle, Theme, Vector,
    mouse, Renderer,
    widget::{container, row, text},
};
// iced_aw Badge 替换手搓徽章容器，提升视觉质感。
use iced_aw::widget::badge::Badge;

use super::state::{Message, UiState};
use super::theme;

const TITLE_BAR_HEIGHT: f32 = 40.0;
const LOGO_SIZE: f32 = 24.0;
const CTRL_BTN_WIDTH: f32 = 46.0;
const DIVIDER_HEIGHT: f32 = 1.0;

pub fn view_title_bar(state: &UiState) -> Element<'_, Message> {
    let logo = canvas(LogoProgram {
        time: state.logo_time,
    })
    .width(Length::Fixed(LOGO_SIZE))
    .height(Length::Fixed(LOGO_SIZE));

    let app_name = text("青萝")
        .color(theme::text_strong())
        .font(Font::with_name("Microsoft YaHei"))
        .size(14.0);

    // iced_aw::Badge 替换手搓容器：青蓝弱化底 + 同色边框 + 胶囊圆角。
    let badge = Badge::<Message>::new(
        text("Quant IDE")
            .color(theme::accent_teal())
            .size(9.0),
    )
    .padding(6)
    .style(theme::title_badge_style());

    let left = row![logo, app_name, badge]
        .spacing(10)
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 0.0, bottom: 0.0, left: 14.0, right: 16.0,
        });

    let min_btn = control_button("—", Message::WindowMinimize, false);
    let max_btn = control_button("▢", Message::WindowToggleMaximize, false);
    let close_btn = control_button("✕", Message::WindowClose, true);

    let right = row![min_btn, max_btn, close_btn]
        .align_y(Alignment::Center)
        .height(Length::Fill)
        .padding(Padding { top: 0.0, bottom: 0.0, left: 0.0, right: 6.0 });

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

    let bar_cont = container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT))
        .style(|_theme| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::title_bar_bg()).into());
            s
        });

    let divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(DIVIDER_HEIGHT))
        .style(|_theme| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 25.0/255.0
            }.into());
            s
        });

    iced::widget::column![bar_cont, divider]
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT + DIVIDER_HEIGHT))
        .into()
}

fn control_button(
    label: &'static str,
    msg: Message,
    is_close: bool,
) -> Element<'static, Message> {
    let label_widget = container(
        text(label)
            .color(theme::text_strong())
            .size(12.0),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center);
    let btn = iced::widget::button(label_widget)
    .on_press(msg)
    .width(Length::Fixed(CTRL_BTN_WIDTH))
    .height(Length::Fixed(28.0))
    .style(move |_theme, status| {
        let mut s = iced::widget::button::Style::default();
        s.background = Some(Color::TRANSPARENT.into());
        s.text_color = theme::text_strong();
        s.border.radius = 8.0.into();
        if matches!(status, iced::widget::button::Status::Hovered) {
            if is_close {
                s.background = Some(Color::from_rgb8(239, 68, 68).into()); // 更现代的红
                s.text_color = Color::WHITE;
            } else {
                s.background = Some(Color::from(theme::hover_bg()).into());
            }
        } else if matches!(status, iced::widget::button::Status::Pressed) {
            if is_close {
                s.background = Some(Color::from_rgb8(220, 38, 38).into());
                s.text_color = Color::WHITE;
            } else {
                s.background = Some(Color::from(theme::pressed_bg()).into());
            }
        }
        s
    });
    btn.into()
}

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

        let design = 24.0f32;
        let size = bounds.size();
        let s = size.width.min(size.height) / design;
        let origin = Vector::new(
            bounds.position().x + (size.width - design * s) / 2.0,
            bounds.position().y + (size.height - design * s) / 2.0,
        );
        frame.translate(origin);

        let px = |dx: f32, dy: f32| -> iced::Point {
            iced::Point::new(dx * s, (design / 2.0 + dy) * s)
        };

        let breathe = (self.time * 1.5).sin() * 1.5;
        let pts = [
            px(2.0, -2.0),
            px(6.0, -4.0),
            px(8.0, -3.0),
            px(10.0, -10.0),
            px(14.0, -9.0),
            px(18.0, -18.0 + breathe),
        ];

        // 新配色：靛蓝 → 青蓝（更符合 v2 蓝紫主题）
        let start_color = Color::from_rgba8(99, 102, 241, 1.0);   // #6366F1 靛蓝
        let end_color = Color::from_rgba8(34, 211, 238, 1.0);     // #22D3EE 青蓝

        // 阴影
        let shadow_path = Path::new(|b| {
            b.move_to(iced::Point::new(pts[0].x + s, pts[0].y + s));
            for p in &pts[1..] {
                b.line_to(iced::Point::new(p.x + s, p.y + s));
            }
        });
        frame.stroke(
            &shadow_path,
            Stroke {
                style: stroke::Style::Solid(Color::from_rgba8(99, 102, 241, 80.0 / 255.0)),
                width: 2.0 * s,
                line_cap: LineCap::Round,
                ..Default::default()
            },
        );

        // 渐变折线
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
                    width: 2.8 * s,
                    line_cap: LineCap::Round,
                    ..Default::default()
                },
            );
        }

        // 数据点
        for (i, &p) in pts.iter().enumerate() {
            let progress = i as f32 / (pts.len() - 1) as f32;
            let color = lerp_color(start_color, end_color, progress);
            // 外圈光晕
            let glow_alpha = (130.0 * (1.0 - progress) / 255.0).clamp(0.0, 1.0);
            let glow = Path::circle(p, 3.4 * s);
            frame.fill(
                &glow,
                Color::from_rgba(color.r, color.g, color.b, glow_alpha),
            );
            // 内圈实心点
            let inner = Path::circle(p, 2.2 * s);
            frame.fill(&inner, color);
        }

        vec![frame.into_geometry()]
    }
}

fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::from_rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        1.0,
    )
}
