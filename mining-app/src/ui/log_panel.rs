//! 底部日志面板：从 mining_analysis_view 分离出的运行日志视图。
//!
//! 包含：
//! - `view_log_panel`：日志面板主体（标题 + 子分类 Tab + 清日志按钮 + 滚动列表）
//! - `view_run_logs`：渲染提醒 / 算子运行日志
//! - `view_json_logs`：渲染通信报文日志
//! - `tool_button`：日志面板内「清日志」按钮使用的胶囊样式按钮
//!
//! 子分类 Tab 通过 `LogCategory` 切换：Action（提醒）/ Runtime（算子运行）/ Json（通信报文）。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Color, Element, Length, Padding};

// ===== iced_aw 组件导入 =====
// 引入 iced_aw::TabBar / TabLabel，替换部分手搓组件，提升 UI 质感。
use iced_aw::widget::{TabBar, TabLabel};

use super::state::{
    JsonDirection, JsonLogEntry, LogCategory, LogLevel, Message, RunLogEntry, UiState,
};
use super::theme;

// ===== 布局尺寸常量 =====
/// 底部日志面板高度。
const LOG_PANEL_HEIGHT: f32 = 220.0;
/// 日志面板最多渲染条数（避免千条日志拖垮渲染）。
const LOG_RENDER_LIMIT: usize = 200;

/// 底部日志面板：标题栏左为"运行日志"标题 + 日志分类胶囊 Tab，右侧为调试开关与清日志按钮。
///
/// v3 调整：将原先位于顶部工具栏的「调试切换」与「清日志」两个低频操作迁移到此处，
/// 顶部工具栏因此只保留「保存 / 执行 DAG」两个核心操作，避免主操作区过度拥挤。
pub fn view_log_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;
    let active = editor.active_tab();

    // 顶部细分隔条
    let top_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::divider()).into());
            s
        });

    // 子标签栏：用 iced_aw::TabBar 替换手搓胶囊 button，
    // 圆角通过 log_tab_bar_style 的 tab_border_radius = 8px 实现。
    let current_cat = active.map(|t| t.active_log_category).unwrap_or_default();
    let tabs_bar = TabBar::new(|c: LogCategory| Message::SwitchLogCategory(c))
        .push(LogCategory::Action, TabLabel::Text(String::from("提醒")))
        .push(LogCategory::Runtime, TabLabel::Text(String::from("算子运行")))
        .push(LogCategory::Json, TabLabel::Text(String::from("通信报文")))
        .set_active_tab(&current_cat)
        .style(theme::log_tab_bar_style())
        .height(Length::Fixed(32.0))
        .text_size(11.0)
        .tab_width(Length::Shrink)
        .padding(Padding { top: 0.0, bottom: 0.0, left: 4.0, right: 4.0 })
        .spacing(4.0);

    // 右侧操作组：清日志
    let actions = row![
        tool_button("⌫ 清日志", Message::ClearLogs, false),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let title = container(text("运行日志").color(theme::text_strong()).size(12.0))
        .padding(Padding { top: 0.0, bottom: 0.0, left: 2.0, right: 0.0 });

    let header_inner = row![
        title,
        tabs_bar,
        row![].width(Length::Fill),
        actions,
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .padding(Padding { top: 8.0, bottom: 6.0, left: 12.0, right: 12.0 });

    let body: Element<'_, Message> = match active {
        None => text("未打开建模").color(theme::text_weak()).size(11.0).into(),
        Some(tab) => match current_cat {
            LogCategory::Action => view_run_logs(&tab.action_logs),
            LogCategory::Runtime => view_run_logs(&tab.runtime_logs),
            LogCategory::Json => view_json_logs(&tab.json_logs),
        },
    };

    let body_wrap = container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding { top: 2.0, bottom: 6.0, left: 0.0, right: 0.0 });

    let body_scroll = scrollable(body_wrap)
        .width(Length::Fill)
        .height(Length::Fill);

    let col = column![top_divider, header_inner, body_scroll]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(col)
        .width(Length::Fill)
        .height(Length::Fixed(LOG_PANEL_HEIGHT))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::panel_bg()).into());
            s
        })
        .into()
}

/// 渲染运行日志（action / runtime 共用）：每条 [时间戳 消息]，按 level 着色。
fn view_run_logs(logs: &[RunLogEntry]) -> Element<'_, Message> {
    if logs.is_empty() {
        return text("(无日志)").color(theme::TEXT_WEAK).size(11.0).into();
    }
    let mut col = column![].spacing(1).padding(Padding {
        top: 4.0,
        bottom: 4.0,
        left: 8.0,
        right: 8.0,
    });
    for entry in logs.iter().rev().take(LOG_RENDER_LIMIT).rev() {
        let msg_color = match entry.level {
            LogLevel::Info => theme::TEXT_HOVER,
            LogLevel::Success => theme::success(),
            LogLevel::Warning => theme::warning(),
            LogLevel::Error => theme::danger(),
        };
        let line = row![
            text(entry.timestamp.clone()).color(theme::TEXT_WEAK).size(10.0),
            text(entry.message.clone()).color(msg_color).size(11.0).width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);
        col = col.push(line);
    }
    col.into()
}

/// 渲染 JSON 通信报文日志：每条 [方向 时间戳 标题]，payload 缩进显示。
fn view_json_logs(logs: &[JsonLogEntry]) -> Element<'_, Message> {
    if logs.is_empty() {
        return text("(无通信报文)").color(theme::TEXT_WEAK).size(11.0).into();
    }
    let mut col = column![].spacing(2).padding(Padding {
        top: 4.0,
        bottom: 4.0,
        left: 8.0,
        right: 8.0,
    });
    for entry in logs.iter().rev().take(LOG_RENDER_LIMIT).rev() {
        let dir_color = match entry.direction {
            JsonDirection::Send => theme::accent(),
            JsonDirection::Receive => theme::success(),
        };
        let dir_label = match entry.direction {
            JsonDirection::Send => "→",
            JsonDirection::Receive => "←",
        };
        let head = row![
            text(dir_label).color(dir_color).size(11.0),
            text(entry.timestamp.clone()).color(theme::TEXT_WEAK).size(10.0),
            text(entry.title.clone()).color(theme::TEXT_HOVER).size(11.0).width(Length::Fill),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .width(Length::Fill);
        let payload = text(entry.payload.clone())
            .color(theme::TEXT_WEAK)
            .size(10.0)
            .width(Length::Fill);
        col = col.push(column![head, payload].spacing(2));
    }
    col.into()
}

/// 工具栏按钮 v3：更精致胶囊样式，主按钮带渐变高光 + 微阴影感。
fn tool_button(label: &str, msg: Message, primary: bool) -> Element<'_, Message> {
    let txt_color = if primary { Color::WHITE } else { theme::text_hover() };
    let label_widget = container(
        text(label).color(txt_color).size(11.0)
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center);
    button(label_widget)
        .height(Length::Fixed(30.0))
        .padding(Padding { top: 0.0, bottom: 0.0, left: 16.0, right: 16.0 })
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.border.radius = 9.0.into();
            if primary {
                // 主按钮：靛蓝主色 + 亮边框高光 + 极细外描边（微阴影错觉）
                s.background = Some(Color::from(theme::accent()).into());
                s.text_color = Color::WHITE;
                s.border.width = 1.0;
                s.border.color = Color {
                    r: 165.0/255.0, g: 180.0/255.0, b: 252.0/255.0, a: 1.0
                };
                match status {
                    iced::widget::button::Status::Hovered => {
                        s.background = Some(Color::from(theme::accent_bright()).into());
                        s.border.color = Color {
                            r: 199.0/255.0, g: 210.0/255.0, b: 254.0/255.0, a: 1.0
                        };
                    }
                    iced::widget::button::Status::Pressed => {
                        s.background = Some(Color::from(theme::accent_dark()).into());
                        s.border.color = Color::from(theme::accent());
                    }
                    _ => {}
                }
            } else {
                // 次按钮：卡片底色 + 细边框，hover 提亮背景 + 文字
                s.background = Some(Color::from(theme::card_bg()).into());
                s.text_color = theme::text_hover();
                s.border.width = 1.0;
                s.border.color = theme::card_stroke();
                match status {
                    iced::widget::button::Status::Hovered => {
                        s.background = Some(Color::from(theme::hover_bg()).into());
                        s.text_color = theme::text_strong();
                        s.border.color = Color {
                            r: 1.0, g: 1.0, b: 1.0, a: 45.0/255.0
                        };
                    }
                    iced::widget::button::Status::Pressed => {
                        s.background = Some(Color::from(theme::pressed_bg()).into());
                    }
                    _ => {}
                }
            }
            s
        })
        .on_press(msg)
        .into()
}
