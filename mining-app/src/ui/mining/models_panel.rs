//! 左侧面板「建模列表」子页：卡片式列表，支持目录分类浏览。
//!
//! 从 `mining_analysis_view` 拆出，仅负责建模列表的渲染（头部 + 面包屑 +
//! 目录/建模卡片 + 底部新建按钮）。入口 [`view_models_panel`] 由
//! `mining_analysis_view::view_sidebar` 在 `LeftPanelTab::Models` 分支调用。

use iced::widget::Stack;
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Color, Element, Length, Padding};

use iced_aw::widget::badge::Badge;

use super::icons::{self, IconKind};
use super::state::{Message, UiState};
use super::theme;
use crate::mining::dag_store;

/// 左侧面板「建模列表」子页 v2：卡片式列表，支持目录分类浏览。
pub fn view_models_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;
    let active_model_id: Option<&str> = editor
        .active_tab()
        .map(|t| t.model_id.as_str());

    // 头部：标题 + 计数徽章（当前目录下 目录数 + 建模数）
    let item_count = editor.folders.len() + editor.models.len();
    let count_badge = Badge::<Message>::new(
        text(format!("{}", item_count))
            .color(theme::accent_teal())
            .size(10.0)
    )
    .padding(6)
    .style(theme::count_badge_style());

    let header = container(
        row![
            text("建模列表").color(theme::text_strong()).size(13.0),
            count_badge,
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(38.0))
    .align_y(Alignment::Center)
    .padding(Padding { top: 0.0, bottom: 0.0, left: 14.0, right: 14.0 });

    let header_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::divider()).into());
            s
        });

    // 面包屑（仅非根目录显示）：全部 / 子目录 / ...
    let breadcrumb: Option<Element<'_, Message>> = if editor.current_folder.is_empty() {
        None
    } else {
        Some(view_folder_breadcrumb(&editor.current_folder))
    };

    // 卡片列表：目录在前、建模在后
    let mut list_col = column![].spacing(4).padding(Padding {
        top: 8.0, bottom: 8.0, left: 10.0, right: 10.0,
    });

    if !editor.models_loaded {
        list_col = list_col.push(
            container(
                text("加载中…").color(theme::text_weak()).size(11.0),
            )
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .padding(Padding { top: 16.0, bottom: 16.0, left: 0.0, right: 0.0 }),
        );
    } else if editor.folders.is_empty() && editor.models.is_empty() {
        // 空状态：精美的占位卡片
        let empty_hint = if editor.current_folder.is_empty() {
            "点击下方按钮新建建模或目录".to_string()
        } else {
            "该目录为空，可在其中新建建模或子目录".to_string()
        };
        let empty_card = container(
            column![
                text("◇").color(theme::accent_dim()).size(32.0),
                text("暂无内容").color(theme::text_strong()).size(12.0),
                text(empty_hint).color(theme::text_weak()).size(10.0),
            ]
            .spacing(4)
            .align_x(Alignment::Center)
        )
        .width(Length::Fill)
        .padding(Padding { top: 24.0, bottom: 24.0, left: 12.0, right: 12.0 })
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.radius = theme::CARD_ROUNDING.into();
            s.border.width = 1.0;
            s.border.color = Color {
                r: 1.0, g: 1.0, b: 1.0, a: 20.0 / 255.0
            };
            s
        });
        list_col = list_col.push(empty_card);
    } else {
        for f in &editor.folders {
            list_col = list_col.push(view_folder_card(f));
        }
        for m in &editor.models {
            let is_active = active_model_id == Some(m.id.as_str());
            list_col = list_col.push(view_model_card(m, is_active));
        }
    }

    // 底部双按钮：新建目录（次要）+ 新建建模（主要），各占一半宽度
    let new_folder_btn = panel_bottom_button(
        icons::view_icon(IconKind::Folder, theme::text_strong(), 13.0),
        "新建目录",
        Message::NewFolderClick,
        false,
    );
    let new_model_btn = panel_bottom_button(
        icons::view_icon(IconKind::Plus, Color::WHITE, 13.0),
        "新建建模",
        Message::NewModelClick,
        true,
    );
    let button_row = row![new_folder_btn, new_model_btn]
        .width(Length::Fill)
        .spacing(8);

    let body_scroll = scrollable(list_col)
        .direction(scrollable::Direction::Vertical(theme::cool_scrollbar()))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::cool_scrollbar_style());

    let mut body = column![header, header_divider];
    if let Some(crumb) = breadcrumb {
        body = body.push(crumb);
    }
    body = body
        .push(body_scroll)
        .push(
            container(button_row)
                .width(Length::Fill)
                .padding(Padding { top: 4.0, bottom: 6.0, left: 10.0, right: 10.0 }),
        );

    let body = body
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(body).width(Length::Fill).height(Length::Fill).into()
}

/// 建模列表底部按钮：32px 高胶囊。`primary=true` 为主色实心（新建建模），
/// `false` 为卡片底描边次按钮（新建目录）。
fn panel_bottom_button<'a>(
    icon: Element<'static, Message>,
    label: &'a str,
    msg: Message,
    primary: bool,
) -> Element<'a, Message> {
    let content = container(
        row![icon, text(label).size(12.0)]
            .spacing(5)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center);

    button(content)
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .on_press(msg)
        .padding(Padding { top: 0.0, bottom: 0.0, left: 8.0, right: 8.0 })
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.border.radius = 10.0.into();
            if primary {
                s.background = Some(Color::from(theme::accent()).into());
                s.text_color = Color::WHITE;
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::accent_bright()).into());
                } else if matches!(status, iced::widget::button::Status::Pressed) {
                    s.background = Some(Color::from(theme::accent_dark()).into());
                }
            } else {
                s.background = Some(Color::from(theme::card_bg()).into());
                s.text_color = theme::text_strong();
                s.border.width = 1.0;
                s.border.color = theme::card_stroke();
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::card_hover_bg()).into());
                    s.border.color = theme::accent_dim();
                    s.text_color = theme::text_hover();
                } else if matches!(status, iced::widget::button::Status::Pressed) {
                    s.background = Some(Color::from(theme::hover_bg()).into());
                }
            }
            s
        })
        .into()
}

/// 目录面包屑：`全部 / A / B`，每段可点击导航；横向超出时可滚动。
fn view_folder_breadcrumb(folder: &str) -> Element<'_, Message> {
    /// 单个面包屑分段：浅色文字按钮，点击导航到该段目录。
    fn crumb_segment(label: String, target: String, current: bool) -> Element<'static, Message> {
        let color = if current {
            theme::text_strong()
        } else {
            theme::text_weak()
        };
        let label_widget = container(text(label).color(color).size(10.5))
            .align_y(Alignment::Center)
            .height(Length::Fill);
        button(label_widget)
            .height(Length::Fixed(22.0))
            .padding(Padding { top: 0.0, bottom: 0.0, left: 5.0, right: 5.0 })
            .on_press(Message::FolderNav(target))
            .style(move |_t, status| {
                let mut s = iced::widget::button::Style::default();
                s.border.radius = 5.0.into();
                if !current && matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::hover_bg()).into());
                    s.text_color = theme::text_hover();
                }
                s
            })
            .into()
    }

    let mut crumbs: Vec<Element<'static, Message>> =
        vec![crumb_segment("全部".to_string(), String::new(), false)];
    let mut acc = String::new();
    let segs: Vec<&str> = folder.split('/').collect();
    let last = segs.len().saturating_sub(1);
    for (i, seg) in segs.iter().enumerate() {
        crumbs.push(
            container(text("/").color(theme::text_weak()).size(10.0))
                .height(Length::Fill)
                .align_y(Alignment::Center)
                .into(),
        );
        if i == 0 {
            acc = seg.to_string();
        } else {
            acc.push('/');
            acc.push_str(seg);
        }
        crumbs.push(crumb_segment(
            seg.to_string(),
            acc.clone(),
            i == last,
        ));
    }

    let row_content = row![].spacing(0).align_y(Alignment::Center);
    let row_content = crumbs
        .into_iter()
        .fold(row_content, |r, c| r.push(c));

    let scroller = scrollable(row_content)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new()
                .width(6.0)
                .scroller_width(3.0)
                .margin(1.0),
        ))
        .width(Length::Fill)
        .height(Length::Fixed(26.0))
        .style(theme::cool_scrollbar_style());

    container(scroller)
        .width(Length::Fill)
        .height(Length::Fixed(28.0))
        .align_y(Alignment::Center)
        .padding(Padding { top: 0.0, bottom: 0.0, left: 6.0, right: 6.0 })
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.color = Color::from(theme::divider());
            s.border.width = 0.0;
            s
        })
        .into()
}

/// 目录卡片 v4（紧凑单行）：文件夹图标块 32 + 名称 + 建模数 + 重命名/删除操作。
fn view_folder_card(f: &dag_store::ModelFolderMeta) -> Element<'_, Message> {
    let icon_block = container(icons::view_icon(IconKind::Folder, theme::accent(), 16.0))
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 34.0 / 255.0, g: 211.0 / 255.0, b: 238.0 / 255.0, a: 15.0 / 255.0,
            }.into());
            s.border.radius = 8.0.into();
            s
        });

    let name_widget = text(f.name.clone())
        .color(theme::text_strong())
        .size(12.0)
        .shaping(text::Shaping::Advanced);
    let name_container = container(name_widget)
        .width(Length::Fill)
        .align_y(Alignment::Center);

    // 建模数作为弱色后缀，空目录显示"空"
    let count_text = if f.model_count == 0 {
        text("空").color(theme::text_weak()).size(9.0)
    } else {
        text(format!("{} 个", f.model_count))
            .color(theme::text_weak())
            .size(9.0)
    };

    let info_row = row![name_container, count_text]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let rename_btn = card_icon_button_kind(
        IconKind::Pencil,
        Message::RenameFolderClick(f.id.clone()),
        false,
        None,
        28.0,
        15.0,
    );
    let delete_btn = card_icon_button_kind(
        IconKind::Trash,
        Message::DeleteFolderClick(f.id.clone(), f.name.clone()),
        false,
        Some(theme::danger()),
        28.0,
        15.0,
    );
    let actions = row![rename_btn, delete_btn].spacing(2);

    button(
        row![icon_block, info_row, actions]
            .spacing(8)
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .on_press(Message::OpenFolder(f.id.clone()))
    .padding(Padding { top: 7.0, bottom: 7.0, left: 8.0, right: 6.0 })
    .style(|_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.border.radius = theme::CARD_ROUNDING.into();
        s.border.width = 1.0;
        s.background = Some(Color::from(theme::card_bg()).into());
        s.border.color = theme::card_stroke();
        s.text_color = theme::text_strong();
        if matches!(status, iced::widget::button::Status::Hovered) {
            s.background = Some(Color::from(theme::card_hover_bg()).into());
            s.border.color = theme::accent_dim();
        }
        s
    })
    .into()
}

/// 建模卡片 v5（两行布局）：
/// 第一行：图标块 36 + 名称(大字, Fill 截断) + 放大操作按钮（右对齐, 叠加名称行）
/// 第二行：日期时间小字（YYYY-MM-DD HH:MM）
/// 按钮放上层使名称行获得最大横向空间。
fn view_model_card(m: &dag_store::DagModelMeta, is_active: bool) -> Element<'_, Message> {
    let name_color = if is_active { Color::WHITE } else { theme::text_strong() };
    let date_color = if is_active {
        Color { r: 1.0, g: 1.0, b: 1.0, a: 0.5 }
    } else {
        theme::text_weak()
    };

    // 左侧图标块 36×36
    let icon_color = if is_active { Color::WHITE } else { theme::accent() };
    let icon_bg = if is_active {
        Color::from(theme::accent())
    } else {
        Color {
            r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 15.0/255.0
        }
    };
    let icon_block = container(
        icons::view_icon(IconKind::Model, icon_color, 18.0)
    )
    .width(Length::Fixed(36.0))
    .height(Length::Fixed(36.0))
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .style(move |_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(icon_bg.into());
        s.border.radius = 9.0.into();
        s
    });

    // 操作按钮（放大到 32×32，图标 17）
    let btn_size = 32.0;
    let icon_size = 17.0;
    let rename_btn = card_icon_button_kind(
        IconKind::Pencil,
        Message::RenameModelClick(m.id.clone()),
        is_active,
        None,
        btn_size,
        icon_size,
    );
    let move_btn = card_icon_button_kind(
        IconKind::Move,
        Message::MoveModelClick(m.id.clone(), m.name.clone()),
        is_active,
        None,
        btn_size,
        icon_size,
    );
    let delete_btn = card_icon_button_kind(
        IconKind::Trash,
        Message::DeleteModelClick(m.id.clone(), m.name.clone()),
        is_active,
        Some(theme::danger()),
        btn_size,
        icon_size,
    );
    let actions = row![rename_btn, move_btn, delete_btn].spacing(1);

    // 名称（大字, 单行截断, Fill 宽度撑满）
    let name_widget = text(m.name.clone())
        .color(name_color)
        .size(15.0)
        .shaping(text::Shaping::Advanced);
    let name_container = container(name_widget)
        .width(Length::Fill)
        .height(Length::Fixed(btn_size))
        .align_x(Alignment::Start)
        .align_y(Alignment::Center);

    // 第一行：名称 Fill 全宽 + 操作按钮悬浮在上层（z-index 覆盖, 不挤占文本宽度）
    let actions_overlay = container(actions)
        .width(Length::Fill)
        .height(Length::Fixed(btn_size))
        .align_x(Alignment::End)
        .align_y(Alignment::Center);
    let top_row = Stack::new()
        .push(name_container)
        .push(actions_overlay)
        .width(Length::Fill)
        .height(Length::Fixed(btn_size));

    // 第二行：日期时间小字（YYYY-MM-DD HH:MM）
    let date_widget = text(dag_store::format_date_hhmm(m.updated_at))
        .color(date_color)
        .size(9.0);
    let date_container = container(date_widget)
        .width(Length::Fill)
        .align_x(Alignment::Start);

    // 右侧列（名称行 + 时间行）
    let right_col = column![top_row, date_container]
        .spacing(1)
        .width(Length::Fill);

    let mid = button(
        row![icon_block, right_col]
            .spacing(10)
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .on_press(Message::OpenModel(m.id.clone()))
    .padding(Padding { top: 9.0, bottom: 9.0, left: 10.0, right: 8.0 })
    .style(move |_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.border.radius = theme::CARD_ROUNDING.into();
        s.border.width = 1.0;
        if is_active {
            s.background = Some(Color {
                r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 95.0/255.0
            }.into());
            s.border.color = Color::from(theme::accent_bright());
            s.text_color = Color::WHITE;
        } else {
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.color = theme::card_stroke();
            s.text_color = theme::text_strong();
            if matches!(status, iced::widget::button::Status::Hovered) {
                s.background = Some(Color::from(theme::card_hover_bg()).into());
                s.border.color = theme::accent_dim();
            }
        }
        s
    });

    mid.into()
}

/// 建模列表卡片操作按钮（矢量图标版）：透明底，hover 微亮背景，
/// 用 `icons::view_icon_with_stroke` 矢量图标统一风格。编辑用铅笔、删除用垃圾桶。
///
/// `tone` 传入 `Some(color)` 时，图标常态即用该语义色（删除按钮传 danger 红）；
/// `None` 时按激活/非激活自动取近白/弱灰，hover 提亮到近白。
///
/// `btn_size` 控制按钮外框边长，`icon_size` 控制内部矢量图标尺寸。
fn card_icon_button_kind(
    icon_kind: IconKind,
    msg: Message,
    is_active: bool,
    tone: Option<Color>,
    btn_size: f32,
    icon_size: f32,
) -> Element<'static, Message> {
    let normal_color = match tone {
        Some(c) => c,
        None => if is_active {
            Color { r: 1.0, g: 1.0, b: 1.0, a: 220.0 / 255.0 }
        } else {
            theme::text_weak()
        },
    };
    let hover_color = match tone {
        Some(c) => c,
        None => if is_active {
            Color::WHITE
        } else {
            theme::text_strong()
        },
    };
    let stroke_width = (icon_size * 1.5 / 15.0).max(1.2);
    let icon = icons::view_icon_with_stroke(icon_kind, normal_color, icon_size, stroke_width);
    let icon_widget = container(icon)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    let corner_r = (btn_size / 2.6).min(7.0);
    button(icon_widget)
        .width(Length::Fixed(btn_size))
        .height(Length::Fixed(btn_size))
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(Color::TRANSPARENT.into());
            s.text_color = hover_color;
            s.border.radius = corner_r.into();
            if matches!(status, iced::widget::button::Status::Hovered) {
                let hover_alpha = if is_active { 30.0 } else { 18.0 };
                s.background = Some(Color { r:1.0,g:1.0,b:1.0, a: hover_alpha / 255.0 }.into());
                s.text_color = hover_color;
            }
            s
        })
        .on_press(msg)
        .into()
}
