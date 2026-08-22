//! 底部状态栏 v2：精致胶囊分隔 + 语义色高亮。
//!
//! 改造：使用 iced_aw::Badge 替换手搓容器，统一风格。
//! - 左侧状态胶囊：状态点 + 消息（语义色 Badge）
//! - 右侧状态项："执行中 / 未保存 / 视图名" 等（按语义色生成 Badge）

use iced::{Alignment, Color, Element, Length, Padding};
use iced::widget::{column, container, row, text};

// iced_aw::Badge 替换手搓胶囊容器，统一蓝紫主题质感。
use iced_aw::widget::badge::Badge;

use super::state::{LogLevel, Message, UiState, ViewType};
use super::theme;

pub fn view_status_bar(state: &UiState) -> Element<'_, Message> {
    let view_name = match state.current_view {
        ViewType::MiningAnalysis => "挖掘分析",
        ViewType::Settings => "系统设置",
    };
    let (level, msg) = current_status(state);

    // 左侧状态：胶囊型 Badge + 点 + 消息
    let dot = text("●").color(level_color(&level)).size(10.0);
    let msg_text = text(msg).color(theme::text_hover()).size(11.0);

    let status_pill = Badge::<Message>::new(
        row![dot, msg_text].spacing(6).align_y(Alignment::Center),
    )
    .padding(8)
    .style(theme::status_pill_style(level_color(&level)));

    // 右侧胶囊型状态项
    let mut right_items: Vec<Element<'_, Message>> = Vec::new();
    if state.dag_editor.dag_exec_task.is_some() {
        let pill = status_pill_small("⏵ 执行中", theme::accent_teal());
        right_items.push(pill);
    }
    if state.current_view == ViewType::MiningAnalysis {
        if let Some(tab) = state.dag_editor.active_tab() {
            if tab.dirty {
                let pill = status_pill_small("● 未保存", theme::warning());
                right_items.push(pill);
            }
        }
    }
    let view_pill = status_pill_small(view_name, theme::accent());
    right_items.push(view_pill);

    let right_row = right_items.into_iter().fold(
        row![].spacing(6).align_y(Alignment::Center),
        |acc, item| acc.push(item),
    );

    let bar = row![
        status_pill,
        row![].width(Length::Fill),
        right_row,
    ]
    .width(Length::Fill)
    .height(Length::Fixed(28.0))
    .spacing(8)
    .align_y(Alignment::Center)
    .padding(Padding { top: 0.0, bottom: 0.0, left: 12.0, right: 12.0 });

    let top_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 1.0, g: 1.0, b: 1.0, a: 20.0 / 255.0
            }.into());
            s
        });

    let bar_cont = container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(28.0))
        .style(|_theme| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::status_bar_bg()).into());
            s
        });

    column![top_divider, bar_cont]
        .width(Length::Fill)
        .height(Length::Fixed(29.0))
        .into()
}

/// 右侧小胶囊：使用 iced_aw::Badge + status_pill_style，按语义色生成。
fn status_pill_small(label: &'static str, color: Color) -> Element<'static, Message> {
    Badge::<Message>::new(
        text(label).color(color).size(10.0),
    )
    .padding(4)
    .style(theme::status_pill_style(color))
    .into()
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
        LogLevel::Info => theme::accent_teal(),
        LogLevel::Success => theme::success(),
        LogLevel::Warning => theme::warning(),
        LogLevel::Error => theme::danger(),
    }
}

#[allow(dead_code)]
fn _unused() {
    let _c: iced::widget::Column<'_, (), iced::Theme, iced::Renderer> = column![];
}
