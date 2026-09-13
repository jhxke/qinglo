//! 左侧活动栏 v3：图标 + 文字垂直堆叠，激活态渐变色块 + 胶囊左指示器。
//!
//! v3 改进：
//! - 图标使用蓝紫→青蓝双色渐变色（激活态），替代单一 accent
//! - 激活指示器改为胶囊型 2.5px 竖条 + 发光半透明背景块
//! - 按钮整体圆角提升至 10px，hover 时更柔和填充
//! - 文字统一 Microsoft YaHei，间距更舒适

use iced::{Alignment, Color, Element, Length, Padding};
use iced::widget::{button, column, container, row, text};

use super::icons::{self, IconKind};
use super::state::{Message, UiState, ViewType};
use super::theme;

const BAR_WIDTH: f32 = 62.0;
const BUTTON_SIZE: f32 = 58.0;

pub fn view_activity_bar(state: &UiState) -> Element<'_, Message> {
    let current = state.current_view.clone();
    let settings_btn = view_activity_button(IconKind::Settings, "设置".to_string(), ViewType::Settings, &current);

    let spacer_top = container(text("").size(1.0))
        .width(Length::Fill)
        .height(Length::Fixed(14.0));

    // 按钮自上而下排列；当 `settings.hide_mining` 为 true 时，「挖掘」按钮不渲染，
    // 让活动栏自动收起该入口。隐藏后仍保留各插件与设置两个视图入口。
    let mut col = column![spacer_top];
    if !state.settings.hide_mining {
        let mining_btn = view_activity_button(IconKind::Mining, "挖掘".to_string(), ViewType::MiningAnalysis, &current);
        col = col.push(mining_btn);
        // 模型服务入口：展示已发布的 DAG 模型服务列表
        let services_btn = view_activity_button(IconKind::Service, "服务".to_string(), ViewType::Services, &current);
        col = col.push(services_btn);
    }

    // 每个插件 = 一个独立菜单项（活动栏按钮）。点击后进入该插件的 WebView 视图，
    // 内容仅为该插件自身的完整功能模块，不再与其他插件挤在同一页面。
    #[cfg(windows)]
    {
        for meta in state.webview_menu.plugin_list() {
            let vt = ViewType::Plugin(meta.id.clone());
            let btn = view_activity_button(IconKind::Sparkle, meta.title.clone(), vt, &current);
            col = col.push(btn);
        }
    }

    col = col.push(settings_btn)
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(3);

    container(col)
        .width(Length::Fixed(BAR_WIDTH))
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::activity_bar_bg()).into());
            s
        })
        .into()
}

fn view_activity_button(
    icon: IconKind,
    label: String,
    vt: ViewType,
    current: &ViewType,
) -> Element<'static, Message> {
    let is_active = current == &vt;

    // v3：激活态图标用双色渐变首末颜色之间的"中间色"模拟发光；
    // 非激活态：弱化 text_weak()
    let icon_color = if is_active {
        // 石墨主题：激活态电光青，科技感发光
        Color::from_rgb8(34, 211, 238) // Cyan-400
    } else {
        theme::text_weak()
    };

    let icon_widget = container(icons::view_icon(icon, icon_color, 19.0))
        .width(Length::Fill)
        .height(Length::Fixed(22.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);

    let label_color = if is_active { theme::text_strong() } else { theme::text_weak() };
    let label_widget = text(label)
        .color(label_color)
        .size(10.0);

    let content = container(
        column![icon_widget, label_widget]
            .spacing(3)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center);

    let btn = button(content)
        .on_press(Message::SwitchView(vt))
        .width(Length::Fill)
        .height(Length::Fixed(BUTTON_SIZE))
        .padding(Padding { top: 0.0, bottom: 0.0, left: 0.0, right: 0.0 })
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(Color::TRANSPARENT.into());
            s.border.radius = 10.0.into();
            if is_active {
                // 石墨主题：激活态青色半透明底 + 电光青微光边框
                s.background = Some(Color {
                    r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 22.0/255.0
                }.into());
                s.border.width = 1.0;
                s.border.color = Color {
                    r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 60.0/255.0
                };
            } else if matches!(status, iced::widget::button::Status::Hovered) {
                // hover：更柔和的填充，圆角与激活一致
                s.background = Some(Color::from(theme::hover_bg()).into());
            } else if matches!(status, iced::widget::button::Status::Pressed) {
                s.background = Some(Color::from(theme::pressed_bg()).into());
            }
            s
        });

    // v3：左侧指示器改为胶囊竖条，激活时配合发光
    let (bar_w, bar_h, bar_color) = if is_active {
        (2.5f32, 30.0f32, theme::accent_bright())
    } else {
        (0.0f32, 0.0f32, Color::TRANSPARENT)
    };
    let accent_bar = container(text("").size(1.0))
        .width(Length::Fixed(bar_w))
        .height(Length::Fixed(bar_h))
        .style(move |_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(bar_color.into());
            s.border.radius = theme::PILL_ROUNDING.into();
            s
        });

    row![
        container(accent_bar)
            .width(Length::Fixed(5.0))
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
        btn,
    ]
    .width(Length::Fixed(BAR_WIDTH))
    .height(Length::Fixed(BUTTON_SIZE))
    .spacing(0)
    .align_y(Alignment::Center)
    .into()
}

#[allow(dead_code)]
fn _unused() {
    let _r: iced::widget::Row<'_, (), iced::Theme, iced::Renderer> = row![];
    let _ = Color::BLACK;
}
