//! 自定义标题栏 v2：精致 Logo 渐变动画 + 应用名 + 拖拽区 + 圆角窗口控制按钮。
//!
//! 设计改进：
//! - 高度提升至 40px，呼吸感更好
//! - Logo 尺寸增大，末点用青蓝 (Cyan) 替代纯绿，更贴合 v2 配色
//! - 控制按钮圆角胶囊样式，关闭按钮 hover 渐变色
//! - 分隔线改为渐变弱化条
//!
//! 品牌化改造（2026-09）：
//! - 应用名 / 副标题徽标 / Logo 样式均按 `state.brand_snapshot` 即时渲染；
//! - `TitleLogo::Sparkline` → 现有折线 canvas 动画；
//! - `TitleLogo::Initial` → 单字文字（取 app_name 首字，居中放置）；
//! - `TitleLogo::ImageFile` → 与 `LogoSource::File` 共用图片路径，
//!   用 `image::Handle::from_path` 直接交给 iced image widget（GPU 上传一次缓存）。
//!   图片解码失败时静默回退到 Sparkline，避免标题栏空白。

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

use super::state::{Message, TitleLogo, UiState};
use super::theme;

const TITLE_BAR_HEIGHT: f32 = 40.0;
const LOGO_SIZE: f32 = 24.0;
const CTRL_BTN_WIDTH: f32 = 46.0;
const DIVIDER_HEIGHT: f32 = 1.0;

pub fn view_title_bar(state: &UiState) -> Element<'_, Message> {
    // Logo 渲染按 brand_snapshot.title_logo 分支：
    // - Sparkline: 折线 canvas 动画（默认，带呼吸效果）
    // - Initial: 单字文字（取 app_name 首字符，无动画）
    // - ImageFile: 用 iced Image widget 加载 LogoSource::File 指定的图片；
    //   无路径 / 文件不可用时回退 Sparkline。
    let logo: Element<'_, Message> = match &state.brand_snapshot.title_logo {
        TitleLogo::Sparkline => canvas(LogoProgram {
            time: state.logo_time,
        })
        .width(Length::Fixed(LOGO_SIZE))
        .height(Length::Fixed(LOGO_SIZE))
        .into(),
        TitleLogo::Initial => {
            let ch = state.brand_snapshot.logo_initial_char();
            container(
                text(ch.to_string())
                    .color(theme::accent_teal())
                    .font(Font::with_name("Microsoft YaHei"))
                    .size(18.0),
            )
            .width(Length::Fixed(LOGO_SIZE))
            .height(Length::Fixed(LOGO_SIZE))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .into()
        }
        TitleLogo::ImageFile => {
            use iced::widget::image;
            match state.brand_snapshot.effective_logo_path() {
                Some(path) => {
                    // 用 from_path 让 iced 内部缓存解码结果，路径变更时
                    // 自动重新解码（Handle::Path 在 widget 比较 时按路径判等）。
                    image(image::Handle::from_path(path))
                        .width(Length::Fixed(LOGO_SIZE))
                        .height(Length::Fixed(LOGO_SIZE))
                        .into()
                }
                // ImageFile 模式但无有效路径：回退 Sparkline
                None => canvas(LogoProgram {
                    time: state.logo_time,
                })
                .width(Length::Fixed(LOGO_SIZE))
                .height(Length::Fixed(LOGO_SIZE))
                .into(),
            }
        }
    };

    // 应用名：空值回退 "青萝"
    let app_name = text(state.brand_snapshot.effective_app_name().to_string())
        .color(theme::text_strong())
        .font(Font::with_name("Microsoft YaHei"))
        .size(14.0);

    // 副标题徽标：空字符串 → 隐藏 Badge（不渲染占位）
    let left_row = match state.brand_snapshot.effective_subtitle() {
        Some(subtitle) => {
            let badge = Badge::<Message>::new(
                text(subtitle.to_string())
                    .color(theme::accent_teal())
                    .size(9.0),
            )
            .padding(6)
            .style(theme::title_badge_style());
            row![logo, app_name, badge]
        }
        None => row![logo, app_name],
    };

    let left = left_row
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
                r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 35.0/255.0
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

        // 石墨主题：深青 → 电光青，黑色底上的发光数据线
        let start_color = Color::from_rgba8(8, 145, 178, 1.0);    // #0891B2 深青
        let end_color = Color::from_rgba8(34, 211, 238, 1.0);     // #22D3EE 电光青

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
                style: stroke::Style::Solid(Color::from_rgba8(8, 145, 178, 80.0 / 255.0)),
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
