//! 底部状态栏（Trae / VS Code 风格）。

use iced::{Alignment, Color, Element, Length, Padding};
use iced::widget::{column, container, row, text};

use super::state::{LogLevel, Message, UiState, ViewType};
use super::theme;

pub fn view_status_bar(state: &UiState) -> Element<'_, Message> {
    let view_name = match state.current_view {
        ViewType::MiningAnalysis => "挖掘分析",
        ViewType::OperatorDevelopment => "算子开发",
        ViewType::Settings => "系统设置",
    };
    let (level, msg) = current_status(state);
    let dot = text("●").color(level_color(&level)).size(12.0);
    let msg_text = text(msg).color(Color::WHITE).size(11.5);

    let left = row![dot, msg_text].spacing(6).align_y(Alignment::Center);

    let mut right_items: Vec<Element<'_, Message>> = Vec::new();
    if state.dag_editor.dag_exec_task.is_some() {
        right_items.push(text("执行中").color(Color::WHITE).size(11.5).into());
    }
    if state.current_view == ViewType::MiningAnalysis {
        if let Some(tab) = state.dag_editor.active_tab() {
            if tab.dirty {
                right_items.push(text("● 未保存").color(Color::WHITE).size(11.5).into());
            }
        }
    }
    right_items.push(text(view_name).color(Color::WHITE).size(11.5).into());

    let right_row = right_items.into_iter().fold(
        row![].spacing(6).align_y(Alignment::Center),
        |acc, item| acc.push(item),
    );

    let pad = Padding { top: 0.0, bottom: 0.0, left: 8.0, right: 8.0 };

    let bar = row![
        left,
        row![].width(Length::Fill),
        right_row,
    ]
    .width(Length::Fill)
    .height(Length::Fixed(24.0))
    .spacing(8)
    .align_y(Alignment::Center)
    .padding(pad);

    let bar_cont = container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(24.0))
        .style(|_theme| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::STATUS_BAR_BG).into());
            s
        });

    bar_cont.into()
}

fn current_status(state: &UiState) -> (LogLevel, String) {
    if state.dag_editor.dag_exec_task.is_some() {
        return (LogLevel::Info, "正在执行 DAG 流程…".into());
    }
    match state.current_view {
        ViewType::MiningAnalysis => {
            if let Some(tab) = state.dag_editor.active_tab() {
                if let Some(err) = &tab.error_message {
                    return (LogLevel::Error, err.clone());
                }
                if let Some(last) = tab.action_logs.last() {
                    return (last.level.clone(), last.message.clone());
                }
            }
        }
        ViewType::OperatorDevelopment => {
            if let Some(err) = &state.operator_development.error_message {
                return (LogLevel::Error, err.clone());
            }
            if let Some(last) = state.operator_development.run_logs.last() {
                return (last.level.clone(), last.message.clone());
            }
        }
        ViewType::Settings => {
            if let Some((ok, msg)) = &state.settings.last_result {
                let level = if *ok { LogLevel::Success } else { LogLevel::Error };
                return (level, msg.clone());
            }
        }
    }
    (LogLevel::Info, "就绪".into())
}

fn level_color(level: &LogLevel) -> Color {
    match level {
        LogLevel::Info => Color::from_rgb8(140, 200, 255),
        LogLevel::Success => Color::from_rgb8(80, 220, 110),
        LogLevel::Warning => Color::from_rgb8(240, 190, 70),
        LogLevel::Error => Color::from_rgb8(245, 100, 100),
    }
}

#[allow(dead_code)]
fn _unused() {
    let _c: iced::widget::Column<'_, (), iced::Theme, iced::Renderer> = column![];
}
