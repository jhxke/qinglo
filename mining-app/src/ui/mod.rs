pub mod activity_bar;
pub mod icons;
pub mod mining;
pub mod settings_view;
pub mod state;
pub mod status_bar;
pub mod theme;
pub mod title_bar;
/// wry WebView2 菜单试验（仅 Windows）。
#[cfg(windows)]
pub mod webview_menu;
/// 网页菜单插件系统：trait + 注册表 + 内外部插件动态加载（仅 Windows）。
#[cfg(windows)]
pub mod webview_plugins;

pub use state::*;

pub use activity_bar::view_activity_bar;
// DAG 视图族统一收敛到 `mining` 子模块；为保持 main.rs 调用路径兼容，
// 在 ui 顶层继续 re-export 其公共入口函数。
pub use mining::view_mining_analysis;
pub use mining::poll_dag_exec_task;
pub use mining::release_all_debug_sessions;
pub use mining::try_spawn_pending_dag_exec;
pub use settings_view::view_settings;
pub use status_bar::view_status_bar;
pub use title_bar::view_title_bar;

use iced::widget::canvas;
use iced::widget::canvas::{Action, Event, Geometry, LineCap, Path};
use iced::widget::canvas::stroke::{self, Stroke};
use iced::{
    Alignment, Color, Element, Length, Point, Radians, Rectangle, Renderer,
    Theme, mouse,
};
use iced::widget::{column, container, text};

/// 阶段 1 占位视图 helper：居中显示标题 + 副标题，背景为默认暗色。
///
/// 接受任意生命周期的字符串（错误信息是运行时构造的 String），
/// 副标题限宽 480px 自动换行，避免长错误串横向溢出。
pub fn placeholder_view(
    title: impl Into<String>,
    hint: impl Into<String>,
) -> Element<'static, Message> {
    let title_w = text(title.into()).color(theme::TEXT_STRONG).size(18.0);
    let hint_w = text(hint.into())
        .color(theme::TEXT_WEAK)
        .size(12.0)
        .width(Length::Fixed(480.0))
        .align_x(iced::alignment::Horizontal::Center);
    let col = column![title_w, hint_w].spacing(8).align_x(Alignment::Center);
    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

// ===== 网页层冷启动加载视图（旋转 spinner）=====

/// Spinner 视觉尺寸（逻辑像素）。
const SPINNER_SIZE: f32 = 36.0;
/// spinner 每秒旋转的弧度倍数（由 anim_time 驱动）。
const SPINNER_SPEED: f32 = 4.5;

/// 旋转圆环 canvas Program：300° 电光青弧段 + 极淡底环。
struct LoadingSpinner {
    /// 当前旋转角（弧度）。
    angle: f32,
}

/// Spinner 不响应交互，State 仅持有 Cache；角度每帧变化故每次 draw 前
/// 主动 clear 强制重建（36px 几何开销可忽略）。
#[derive(Default)]
struct SpinnerState {
    cache: canvas::Cache,
}

impl canvas::Program<Message> for LoadingSpinner {
    type State = SpinnerState;

    fn update(
        &self,
        _state: &mut SpinnerState,
        _event: &Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        None
    }

    fn draw(
        &self,
        state: &SpinnerState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        state.cache.clear();
        let geo = state.cache.draw(renderer, bounds.size(), |frame| {
            let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
            let radius = bounds.width.min(bounds.height) / 2.0 - 3.0;

            // 底环：白色低透明整圆，提供旋转轨迹参照
            let track = Path::circle(center, radius);
            frame.stroke(
                &track,
                Stroke {
                    style: stroke::Style::Solid(Color {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 14.0 / 255.0,
                    }),
                    width: 3.0,
                    ..Default::default()
                },
            );

            // 旋转弧：300°（5π/3），圆角端点，主题电光青
            let arc = Path::new(|builder| {
                builder.arc(canvas::path::Arc {
                    center,
                    radius,
                    start_angle: Radians(self.angle),
                    end_angle: Radians(
                        self.angle + std::f32::consts::PI * 5.0 / 3.0,
                    ),
                });
            });
            frame.stroke(
                &arc,
                Stroke {
                    style: stroke::Style::Solid(theme::accent_teal()),
                    width: 3.0,
                    line_cap: LineCap::Round,
                    ..Default::default()
                },
            );
        });
        vec![geo]
    }
}

/// 网页层冷启动占位视图：旋转 spinner + 标题 + 提示。
///
/// WebView2 控制器背景透明，网页首帧到达前主窗口透出的正是这层视图
/// （见 `webview_menu` 模块中 `with_transparent(true)` 的说明）。
/// `anim_time` 由订阅的 80ms AnimTick 持续推进，驱动圆环旋转；
/// 预热生效后用户通常只会在极短的一帧内看到它。
pub fn webview_loading_view(anim_time: f32) -> Element<'static, Message> {
    let spinner: Element<'static, Message> = canvas(LoadingSpinner {
        angle: anim_time * SPINNER_SPEED,
    })
    .width(Length::Fixed(SPINNER_SIZE))
    .height(Length::Fixed(SPINNER_SIZE))
    .into();

    let title_w = text("网页功能加载中")
        .color(theme::text_strong())
        .size(15.0);
    let hint_w = text("正在启动 WebView2 运行时并组装插件页面……")
        .color(theme::text_weak())
        .size(12.0);

    let col = column![spinner, title_w, hint_w]
        .spacing(10)
        .align_x(Alignment::Center);

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}
