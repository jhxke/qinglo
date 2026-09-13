//! 模型服务视图：展示所有已发布的 DAG 模型服务，支持调用 / 删除 / 刷新。
//!
//! - 顶部标题栏 + 刷新按钮
//! - HTTP API 使用说明卡片（告知外部程序如何通过 HTTP 调用已发布的服务）
//! - 服务列表：每张卡片展示服务名、描述、节点数、创建时间，并提供「调用」「删除」按钮

use iced::alignment::Horizontal;
use iced::widget::{button, column, container, row, scrollable, text};
use iced::widget::scrollable::Direction;
use iced::{Alignment, Color, Element, Length, Padding};

use super::icons::{self, IconKind};
use super::state::{Message, UiState};
use super::theme;

/// 卡片内边距
const CARD_PADDING: Padding = Padding {
    top: 14.0,
    bottom: 14.0,
    left: 16.0,
    right: 16.0,
};

pub fn view_services(state: &UiState) -> Element<'_, Message> {
    // ===== 顶部标题栏 =====
    let title = text("模型服务")
        .color(theme::text_strong())
        .size(18.0);

    let refresh_btn = button(
        container(
            row![
                icons::view_icon(IconKind::Refresh, theme::text_strong(), 14.0),
                text("刷新").size(11.5).color(theme::text_strong()),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 12.0,
            right: 12.0,
        })
        .height(Length::Fixed(32.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center),
    )
    .on_press(Message::RefreshServices)
    .style(|_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.border.radius = theme::WIDGET_ROUNDING.into();
        s.border.width = 1.0;
        s.border.color = Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 30.0 / 255.0,
        };
        s.background = if matches!(status, iced::widget::button::Status::Hovered) {
            Some(Color::from(theme::hover_bg()).into())
        } else {
            Some(Color::TRANSPARENT.into())
        };
        s
    });

    let header = row![title, refresh_btn]
        .spacing(12)
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 16.0,
            bottom: 8.0,
            left: 20.0,
            right: 20.0,
        });

    // ===== HTTP API 使用说明 =====
    let api_hint = render_api_hint_card();

    // ===== 操作结果提示 =====
    let result_msg = if let Some(msg) = &state.services.last_result {
        Some(render_result_banner(msg))
    } else {
        None
    };

    // ===== 服务列表 =====
    let list_title = text("已发布服务")
        .color(theme::text_weak())
        .size(12.0)
        .align_x(Horizontal::Left);

    let mut list_col = column![list_title].spacing(8).padding(Padding {
        top: 4.0,
        bottom: 4.0,
        left: 20.0,
        right: 20.0,
    });

    if state.services.refreshing {
        list_col = list_col.push(
            container(text("正在刷新服务列表…").color(theme::text_weak()).size(12.0))
                .padding(20),
        );
    } else if state.services.services.is_empty() {
        list_col = list_col.push(render_empty_state());
    } else {
        for svc in &state.services.services {
            list_col = list_col.push(render_service_card(svc));
        }
    }

    // 组合为可滚动视图
    let scroll = scrollable(list_col)
        .direction(Direction::Vertical(theme::cool_scrollbar()))
        .style(theme::cool_scrollbar_style())
        .width(Length::Fill)
        .height(Length::Fill);

    let mut content = column![header, api_hint].spacing(10).width(Length::Fill);
    if let Some(msg) = result_msg {
        content = content.push(msg);
    }
    content = content.push(scroll).height(Length::Fill);

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::panel_bg()).into());
            s
        })
        .into()
}

/// HTTP API 使用说明卡片。
fn render_api_hint_card() -> Element<'static, Message> {
    // 每行 = 方法 + 路径（等宽字体、电光青）+ 中文说明（普通字体、次要灰）。
    // 不能整行用 MONOSPACE：系统等宽字体不含中文字形，中文会回退到异常大的系统字体。
    let api_rows: [( &str, &str); 3] = [
        ("GET  http://127.0.0.1:17891/services", "列出所有已发布服务"),
        ("GET  http://127.0.0.1:17891/services/{name}", "查看服务 DAG 详情"),
        ("POST http://127.0.0.1:17891/services/{name}", "调用服务（body 可选 params_overrides）"),
    ];

    let mut code_lines = column![].spacing(5);
    for (path_part, desc_part) in &api_rows {
        let line = row![
            text(*path_part)
                .color(theme::accent_teal())
                .size(11.0)
                .font(iced::Font::MONOSPACE),
            text(*desc_part)
                .color(theme::text_weak())
                .size(11.0),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        code_lines = code_lines.push(line);
    }

    let code_block = container(code_lines)
        .padding(Padding {
            top: 10.0,
            bottom: 10.0,
            left: 14.0,
            right: 14.0,
        })
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 8.0 / 255.0,
                g: 9.0 / 255.0,
                b: 11.0 / 255.0,
                a: 1.0,
            }
            .into());
            s.border.radius = 8.0.into();
            s.border.width = 1.0;
            s.border.color = Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 14.0 / 255.0,
            };
            s
        })
        .width(Length::Fill);

    let desc = text("发布后的服务可通过 HTTP API 被外部程序调用：")
        .color(theme::text_weak())
        .size(11.0);

    let card = column![desc, code_block].spacing(8);

    container(card)
        .padding(Padding {
            top: 12.0,
            bottom: 12.0,
            left: 20.0,
            right: 20.0,
        })
        .width(Length::Fill)
        .into()
}

/// 操作结果横幅（成功=青色，失败=红色）。
fn render_result_banner(msg: &str) -> Element<'static, Message> {
    let is_error = msg.starts_with("失败") || msg.starts_with("错误");
    let color = if is_error {
        Color::from_rgb8(239, 68, 68) // red-500
    } else {
        theme::accent_teal()
    };

    container(
        text(msg.to_string())
            .color(color)
            .size(11.5)
            .width(Length::Fill),
    )
    .padding(Padding {
        top: 8.0,
        bottom: 8.0,
        left: 20.0,
        right: 20.0,
    })
    .width(Length::Fill)
    .into()
}

/// 空状态提示。
fn render_empty_state() -> Element<'static, Message> {
    container(
        column![
            icons::view_icon(IconKind::Service, theme::text_weak(), 48.0),
            text("还没有已发布的服务").color(theme::text_weak()).size(13.0),
            text("在挖掘视图的画布工具栏点击「发布」按钮，将当前 DAG 模型发布为服务")
                .color(theme::text_weak())
                .size(11.0)
                .width(Length::Fixed(360.0))
                .align_x(Horizontal::Center),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(Padding {
        top: 40.0,
        bottom: 40.0,
        left: 0.0,
        right: 0.0,
    })
    .align_x(Alignment::Center)
    .into()
}

/// 单个服务卡片。
fn render_service_card(svc: &operator_executor_client::protocol::PublishedServiceInfo) -> Element<'static, Message> {
    let name_label = text(svc.name.clone())
        .color(theme::text_strong())
        .size(14.0);

    let desc_text = if svc.description.is_empty() {
        "（无描述）"
    } else {
        &svc.description
    };
    let desc_label = text(desc_text.to_string())
        .color(theme::text_weak())
        .size(11.0);

    // 元信息：节点数 · 边数 · 创建时间（含中文，不能用 MONOSPACE）
    let created_str = format_timestamp(svc.created_at);
    let meta = text(format!(
        "{} 个节点 · {} 条边 · 发布于 {}",
        svc.node_count, svc.edge_count, created_str
    ))
    .color(theme::text_weak())
    .size(10.0);

    // 源建模 id（如有）
    let source = if let Some(id) = &svc.source_model_id {
        Some(text(format!("源建模: {}", id)).color(theme::text_weak()).size(10.0))
    } else {
        None
    };

    // 左侧信息列
    let mut info_col = column![name_label, desc_label, meta].spacing(4);
    if let Some(s) = source {
        info_col = info_col.push(s);
    }

    // 右侧操作按钮
    let invoke_btn = button(
        container(
            row![
                icons::view_icon(IconKind::Run, Color::WHITE, 12.0),
                text("调用").size(11.0).color(Color::WHITE),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 12.0,
            right: 12.0,
        })
        .height(Length::Fixed(30.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center),
    )
    .on_press(Message::InvokeServiceClick(svc.name.clone()))
    .style(|_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.border.radius = 8.0.into();
        s.background = Some(Color::from(theme::accent()).into());
        s.text_color = Color::WHITE;
        if matches!(status, iced::widget::button::Status::Hovered) {
            s.background = Some(Color::from(theme::accent_bright()).into());
        }
        s
    });

    let delete_btn = button(
        container(
            row![
                icons::view_icon(IconKind::Trash, theme::text_weak(), 12.0),
                text("删除").size(11.0).color(theme::text_weak()),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 12.0,
            right: 12.0,
        })
        .height(Length::Fixed(30.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center),
    )
    .on_press(Message::UnpublishServiceClick(svc.name.clone()))
    .style(|_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.border.radius = 8.0.into();
        s.border.width = 1.0;
        s.border.color = Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 20.0 / 255.0,
        };
        s.background = Some(Color::TRANSPARENT.into());
        if matches!(status, iced::widget::button::Status::Hovered) {
            s.background = Some(Color {
                r: 239.0 / 255.0,
                g: 68.0 / 255.0,
                b: 68.0 / 255.0,
                a: 20.0 / 255.0,
            }
            .into());
            s.border.color = Color::from_rgb8(239, 68, 68);
        }
        s
    });

    let actions = row![invoke_btn, delete_btn].spacing(8).align_y(Alignment::Center);

    let card_body = row![info_col, actions]
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    container(card_body)
        .padding(CARD_PADDING)
        .width(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.radius = 10.0.into();
            s.border.width = 1.0;
            s.border.color = Color::from(theme::card_stroke()).into();
            s
        })
        .into()
}

/// 将毫秒时间戳格式化为可读日期时间字符串。
fn format_timestamp(millis: u64) -> String {
    let secs = millis / 1000;
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let _sec = secs % 60;
    // 简化日期：从 1970-01-01 推算
    let year = 1970 + (days / 365);
    let day_of_year = days % 365;
    let month = (day_of_year / 30).clamp(0, 11) + 1;
    let day = (day_of_year % 30).clamp(1, 30);
    format!(
        "{}-{:02}-{:02} {:02}:{:02}",
        year, month, day, hours, mins
    )
}
