//! 左侧活动栏（Trae / VS Code 风格）。

use iced::{Alignment, Color, Element, Length};
use iced::widget::{button, column, container, row, text};

use super::state::{Message, UiState, ViewType};
use super::theme;

const BAR_WIDTH: f32 = 48.0;
const BUTTON_SIZE: f32 = 48.0;

pub fn view_activity_bar(state: &UiState) -> Element<'_, Message> {
    let mining_btn = view_activity_button("挖掘", ViewType::MiningAnalysis, state.current_view);
    let op_btn = view_activity_button("算子", ViewType::OperatorDevelopment, state.current_view);
    let settings_btn = view_activity_button("设置", ViewType::Settings, state.current_view);

    let col = column![mining_btn, op_btn, settings_btn]
        .width(Length::Fill)
        .height(Length::Fill);

    container(col)
        .width(Length::Fixed(BAR_WIDTH))
        .height(Length::Fill)
        .into()
}

fn view_activity_button(
    label: &'static str,
    vt: ViewType,
    current: ViewType,
) -> Element<'_, Message> {
    let is_active = current == vt;
    // 激活：强白文字 + 左侧 accent 竖条；非激活：弱化文字，hover 时变亮
    let txt_color = if is_active { theme::TEXT_STRONG } else { theme::TEXT_WEAK };

    let label_widget = text(label).color(txt_color).size(11.0);

    let btn = button(label_widget)
        .on_press(Message::SwitchView(vt))
        .width(Length::Fill)
        .height(Length::Fixed(BUTTON_SIZE))
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(Color::TRANSPARENT.into());
            s.text_color = txt_color;
            if is_active {
                // 激活：稍亮底色，让选中感更明显
                s.background = Some(Color::from(theme::HOVER_BG).into());
            } else if matches!(status, iced::widget::button::Status::Hovered) {
                // 非激活 hover：弱底色
                s.background = Some(Color { r: 1.0, g: 1.0, b: 1.0, a: 12.0 / 255.0 }.into());
                s.text_color = theme::TEXT_HOVER;
            }
            s
        });

    // 左侧 2px accent 竖条（仅激活态绘制），与按钮拼成完整一行
    let bar_w = if is_active { 2.0 } else { 0.0 };
    let accent_bar = container(text("").size(1.0))
        .width(Length::Fixed(bar_w))
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::ACCENT).into());
            s
        });

    row![accent_bar, btn]
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
