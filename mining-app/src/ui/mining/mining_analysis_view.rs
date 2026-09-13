//! 挖掘分析视图：左侧合并面板（建模列表 / 算子面板，tab 切换）+ Tab 栏 + 工具栏 + DAG 画布 + 日志面板。
//!
//! 阶段 3 回填真实交互（基于阶段 2.3 骨架）：
//! - 建模列表：首入视图懒加载（refresh_models），列表项点击打开 tab，
//!   + 新建模 / 重命名 / 删除（含对话框叠加层）
//! - 算子面板：搜索 + 分类树点击添加节点 + 节点参数编辑
//! - 左侧面板顶部 tab：[建模列表 | 算子面板]，由 `active_left_panel` 驱动
//! - Tab 栏：点击切换、× 关闭
//! - 工具栏：保存 / 执行 DAG（置 pending 标志，由 Tick 轮询 spawn）/ 调试切换 / 清空日志
//! - 日志面板：三子标签（提醒 / 算子运行 / 通信报文）+ scrollable 渲染
//!   （日志面板的视图代码位于 `super::log_panel` 模块）
//! - 对话框用 `stack::Stack` 叠加半透明遮罩 + 居中卡片实现
//!
//! 后台执行 spawn：`try_spawn_pending_dag_exec` 在 Tick 中被调用，检查激活 tab 的
//! `pending_run_all` / `pending_run_up_to` 标志，若为真则 clone graph + 起工作线程
//! 调用 `execute_dag_on_server_streaming_debug` / `execute_dag_up_to_detached_streaming_debug`，
//! 通过 mpsc::Sender 把 NodeProgress/StreamChunk/Finished 推回 UI 线程的
//! `DagExecTask.rx`。`poll_dag_exec_task` 在 Tick 中 drain rx，回填 registry 与日志。
//! 流式 chunk（chat DSL）的实时预览留待 chat_preview 窗口接入。

use iced::widget::Stack;
use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_editor, text_input};
use iced::widget::mouse_area;
use iced::{
    Alignment, Background, Color, Element, Length, Padding,
};

// ===== iced_aw 组件导入 =====
// 引入 iced_aw::Card / Badge，替换部分手搓组件，提升 UI 质感。
// 日志面板的 TabBar / TabLabel 已迁出至 log_panel 模块。
use iced_aw::widget::Card;
use iced_aw::widget::badge::Badge;

use super::icons::{self, IconKind};
use super::state::{
    DagEditorState, DagExecKind, DagExecMessage, DagExecTask, DagTab,
    LeftPanelTab, LogLevel, Message, UiState,
};
use super::theme;
use crate::mining::dag_store;
use crate::mining::dag::{OperatorPortParamDef, ParamType, PortDirection};
use crate::mining::operator_executor::{
    apply_dag_execution_result, apply_dag_node_result,
    execute_dag_on_server_streaming_debug,
    execute_dag_up_to_detached_streaming_debug,
};
use operator_executor_client::protocol::OperatorExecutionStatus;
use operator_executor_client::PortData;

// ===== 布局尺寸常量 =====
/// 左侧合并面板（建模列表 / 算子面板共用）宽度。
///
/// 历史上建模列表为 220px、算子面板为 240px；二者合并到同一侧栏后取较大值 240px，
/// 既保证算子卡片有足够展示空间，又让画布水平方向多出 220px。
const LEFT_PANEL_WIDTH: f32 = 240.0;
/// 顶部栏高度：Tab 行 36px + 1px 分隔线 = 37px。
/// 工具栏已迁移为画布上的悬浮条状，不再占用顶部栏空间。
const TOP_BAR_HEIGHT: f32 = 37.0;
/// 对话框基础宽度（实际对话框可能覆盖此值）。
#[allow(dead_code)]
const DIALOG_WIDTH: f32 = 360.0;
/// 右侧节点参数抽屉宽度（画布双击节点时弹出）。
const PARAMS_DRAWER_WIDTH: f32 = 280.0;

pub fn view_mining_analysis(state: &UiState) -> Element<'_, Message> {
    let sidebar = view_sidebar(state);
    let main_area = view_main_area(state);

    let base_body = row![sidebar, main_area]
        .width(Length::Fill)
        .height(Length::Fill);

    let base_layer: Element<'_, Message> = container(base_body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::PANEL_BG).into());
            s
        })
        .into();

    // 对话框叠加层：同一时刻最多一个对话框
    let dialog_layer: Option<Element<'_, Message>> = if state.dag_editor.show_new_model_dialog {
        Some(view_new_model_dialog(state))
    } else if state.dag_editor.show_new_folder_dialog {
        Some(view_new_folder_dialog(state))
    } else if state.dag_editor.rename_target_id.is_some() {
        Some(view_rename_dialog(state))
    } else if state.dag_editor.rename_folder_target_id.is_some() {
        Some(view_rename_folder_dialog(state))
    } else if state.dag_editor.show_delete_model_dialog {
        Some(view_delete_confirm_dialog(state))
    } else if state.dag_editor.show_delete_folder_dialog {
        Some(view_delete_folder_confirm_dialog(state))
    } else if state.dag_editor.show_move_model_dialog {
        Some(view_move_model_dialog(state))
    } else {
        None
    };

    // 注：画布/节点右键菜单的叠加层位于 view_middle 的画布 Stack 内，
    // 使其坐标原点与画布一致（菜单位置 = 右键点击位置，无外壳栏偏移）。
    let mut layers = vec![base_layer];
    if let Some(dlg) = dialog_layer {
        layers.push(dlg);
    }
    let stacked = Stack::with_children(layers)
        .width(Length::Fill)
        .height(Length::Fill);
    stacked.into()
}

// ===== 左侧合并面板：建模列表 + 算子面板（tab 切换） =====

fn view_sidebar(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;

    // 顶部 tab 栏：建模列表 | 算子面板
    let tab_bar = view_left_panel_tabs(editor.active_left_panel);

    // tab 栏下方 1px 分隔线
    let tab_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::DIVIDER).into());
            s
        });

    // 内容区：根据激活的子标签页渲染
    let content: Element<'_, Message> = match editor.active_left_panel {
        LeftPanelTab::Models => view_models_panel(state),
        LeftPanelTab::Operators => view_operator_panel(state),
    };

    let body = column![tab_bar, tab_divider, content]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(body)
        .width(Length::Fixed(LEFT_PANEL_WIDTH))
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::SIDEBAR_BG).into());
            s
        })
        .into()
}

/// 左侧面板顶部 tab 栏：[建模列表 | 算子面板]。
///
/// 手搓两个 button 实现，激活态实色靛紫填充 + 白字，未激活透明底 + 灰字，
/// 完全避免 iced_aw::TabBar 样式派发失效问题。点击切换 `active_left_panel`。
fn view_left_panel_tabs(active: LeftPanelTab) -> Element<'static, Message> {
    use LeftPanelTab::{Models, Operators};

    fn tab_button(
        label: &'static str,
        is_active: bool,
        msg: Message,
    ) -> Element<'static, Message> {
        let txt_color = if is_active { Color::WHITE } else { theme::text_weak() };
        let label_widget = container(text(label).color(txt_color).size(11.0))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center);
        button(label_widget)
            .width(Length::Fill)
            .height(Length::Fixed(32.0))
            .padding(Padding::default())
            .on_press(msg)
            .style(move |_t, status| {
                let mut s = iced::widget::button::Style::default();
                s.border.radius = 6.0.into();
                if is_active {
                    // 激活 tab：实色靛紫填充 + 亮紫边框 + 白字
                    s.background = Some(Color::from(theme::accent()).into());
                    s.text_color = Color::WHITE;
                    s.border.width = 1.0;
                    s.border.color = theme::accent_bright();
                    match status {
                        iced::widget::button::Status::Hovered => {
                            s.background = Some(Color::from(theme::accent_bright()).into());
                        }
                        iced::widget::button::Status::Pressed => {
                            s.background = Some(Color::from(theme::accent_dark()).into());
                            s.border.color = theme::accent();
                        }
                        _ => {}
                    }
                } else {
                    // 未激活 tab：透明底 + 灰字，hover 时微亮
                    s.background = Some(Color::TRANSPARENT.into());
                    s.text_color = theme::text_weak();
                    s.border.width = 0.0;
                    s.border.color = Color::TRANSPARENT;
                    match status {
                        iced::widget::button::Status::Hovered => {
                            s.background = Some(Color::from(theme::hover_bg()).into());
                            s.text_color = theme::text_hover();
                        }
                        iced::widget::button::Status::Pressed => {
                            s.background = Some(Color::from(theme::card_bg()).into());
                        }
                        _ => {}
                    }
                }
                s
            })
            .into()
    }

    let inner = row![
        tab_button("建模列表", matches!(active, Models), Message::SwitchLeftPanel(Models)),
        tab_button("算子面板", matches!(active, Operators), Message::SwitchLeftPanel(Operators)),
    ]
    .width(Length::Fill)
    .height(Length::Fixed(36.0))
    .spacing(4.0)
    .padding(Padding { top: 2.0, bottom: 2.0, left: 8.0, right: 8.0 });

    container(inner)
        .width(Length::Fill)
        .height(Length::Fixed(36.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::SIDEBAR_BG).into());
            s
        })
        .into()
}

/// 左侧面板「建模列表」子页 v2：卡片式列表，支持目录分类浏览。
fn view_models_panel(state: &UiState) -> Element<'_, Message> {
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

// ===== 右侧主区：顶部 Tab 栏 + 工具栏 + DAG 画布 + 算子面板 + 日志面板 =====

fn view_main_area(state: &UiState) -> Element<'_, Message> {
    let top_bar = view_top_bar(state);
    let middle = view_middle(state);

    // 日志面板：可见时显示完整面板，折叠时不再占用主区下方空间，
    // 而是缩小到状态栏右侧（详见 status_bar::view_status_bar）。
    let log_area: Element<'_, Message> = if state.dag_editor.log_panel_visible {
        super::log_panel::view_log_panel(state)
    } else {
        container(column![])
            .width(Length::Fill)
            .height(Length::Fixed(0.0))
            .into()
    };

    let col = column![top_bar, middle, log_area]
        .width(Length::Fill)
        .height(Length::Fill);

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

// ===== 顶部 Tab 栏 =====

/// 顶部栏：Tab 卡片列表 + 分隔线。
/// 现代风格：无边框、选中态底部 accent 色指示条、关闭按钮仅 hover 显形。
fn view_top_bar(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;

    // 空状态：未打开建模时显示占位文本
    let tabs_row: Element<'_, Message> = if editor.tabs.is_empty() {
        container(text("未打开建模").color(theme::text_weak()).size(11.0))
            .padding(Padding { top: 0.0, bottom: 0.0, left: 14.0, right: 0.0 })
            .width(Length::Fill)
            .height(Length::Fixed(36.0))
            .align_y(Alignment::Center)
            .into()
    } else {
        let tabs: Vec<Element<'_, Message>> = editor.tabs.iter().enumerate().map(|(i, tab)| {
            let is_active = editor.active_tab_index == Some(i);
            let is_hovered = editor.hovered_tab == Some(i);
            let label = if tab.dirty {
                format!("{} •", tab.name)
            } else {
                tab.name.clone()
            };
            view_tab_item(i, label, is_active, is_hovered)
        }).collect();

        iced::widget::row::Row::with_children(tabs)
            .spacing(2.0)
            .width(Length::Fill)
            .height(Length::Fixed(36.0))
            .padding(Padding { top: 0.0, bottom: 0.0, left: 8.0, right: 8.0 })
            .align_y(Alignment::Center)
            .into()
    };

    let bottom_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::divider()).into());
            s
        });

    let inner = column![tabs_row, bottom_divider]
        .width(Length::Fill)
        .height(Length::Fixed(TOP_BAR_HEIGHT))
        .spacing(0);

    container(inner)
        .width(Length::Fill)
        .height(Length::Fixed(TOP_BAR_HEIGHT))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::panel_bg()).into());
            s
        })
        .into()
}

/// 现代 Tab 卡片：标签 + 关闭图标视觉融为一体。
/// 选中 tab 始终显示 ×，未选中 tab hover 时显示 ×。
/// 文字按钮与关闭按钮无间隙拼接，外观如一体。
fn view_tab_item(
    idx: usize,
    label: String,
    is_active: bool,
    is_hovered: bool,
) -> Element<'static, Message> {
    let show_close = is_active || is_hovered;

    // 统一的文字颜色
    let text_color = if is_active { Color::WHITE } else { theme::text_weak() };

    // 通用样式闭包（选中态 vs 未选中态）
    let make_style = move |_t: &iced::Theme, status: iced::widget::button::Status| {
        let mut s = iced::widget::button::Style::default();
        s.border.radius = 0.0.into();
        s.border.width = 0.0;
        s.border.color = Color::TRANSPARENT;
        if is_active {
            s.background = Some(Color {
                r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 30.0/255.0,
            }.into());
            match status {
                iced::widget::button::Status::Hovered => {
                    s.background = Some(Color {
                        r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 50.0/255.0,
                    }.into());
                }
                iced::widget::button::Status::Pressed => {
                    s.background = Some(Color {
                        r: 21.0/255.0, g: 94.0/255.0, b: 117.0/255.0, a: 40.0/255.0,
                    }.into());
                }
                _ => {}
            }
        } else {
            s.background = Some(Color::TRANSPARENT.into());
            match status {
                iced::widget::button::Status::Hovered => {
                    s.background = Some(Color::from(theme::hover_bg()).into());
                    s.text_color = theme::text_strong();
                }
                iced::widget::button::Status::Pressed => {
                    s.background = Some(Color::from(theme::pressed_bg()).into());
                }
                _ => {}
            }
        }
        s
    };

    // 文字按钮（左部分）
    let text_widget = container(
        text(label).color(text_color).size(11.0)
    )
    .width(Length::Shrink)
    .height(Length::Fixed(32.0))
    .padding(Padding { top: 0.0, bottom: 0.0, left: 12.0, right: 2.0 })
    .align_y(Alignment::Center);

    let text_btn = button(text_widget)
        .width(Length::Shrink)
        .height(Length::Fixed(32.0))
        .padding(Padding::default())
        .on_press(Message::SwitchTab(idx))
        .style(make_style.clone());

    // 关闭按钮区域（始终占位，仅在需要时显示 × 图标）
    let close_slot: Element<'static, Message> = if show_close {
        let close_color = if is_active {
            Color { r: 233.0/255.0, g: 235.0/255.0, b: 239.0/255.0, a: 0.65 }
        } else {
            theme::text_weak()
        };
        let close_icon = icons::view_icon_with_stroke(IconKind::Close, close_color, 10.0, 1.2);
        let close_content = container(close_icon)
            .width(Length::Fixed(18.0))
            .height(Length::Fixed(32.0))
            .padding(Padding { top: 0.0, bottom: 0.0, left: 0.0, right: 8.0 })
            .align_x(Alignment::Center)
            .align_y(Alignment::Center);
        button(close_content)
            .width(Length::Fixed(26.0))
            .height(Length::Fixed(32.0))
            .padding(Padding::default())
            .on_press(Message::CloseTab(idx))
            .style(make_style.clone())
            .into()
    } else {
        // 空占位也可点击切换 tab
        button(container(row![]).width(Length::Fixed(26.0)).height(Length::Fixed(32.0)))
            .width(Length::Fixed(26.0))
            .height(Length::Fixed(32.0))
            .padding(Padding::default())
            .on_press(Message::SwitchTab(idx))
            .style(make_style.clone())
            .into()
    };

    // 拼接文字 + 关闭按钮（无间隙，关闭区域始终占位）
    let tab_content = row![text_btn, close_slot]
        .spacing(0.0)
        .height(Length::Fixed(32.0))
        .align_y(Alignment::Center);

    // 外层：hover 追踪 + 圆角裁剪
    let hover_aware = mouse_area(tab_content)
        .on_enter(Message::TabHover(Some(idx)))
        .on_exit(Message::TabHover(None));

    // 用 container 裁剪圆角
    let rounded = container(hover_aware)
        .width(Length::Shrink)
        .height(Length::Fixed(32.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.border.radius = 8.0.into();
            s
        });

    // 底部 accent 指示条
    let bottom_bar: Element<'_, Message> = if is_active {
        container(row![])
            .width(Length::Fill)
            .height(Length::Fixed(2.0))
            .style(|_t| {
                let mut s = iced::widget::container::Style::default();
                s.background = Some(Color::from(theme::accent()).into());
                s
            })
            .into()
    } else {
        container(row![])
            .width(Length::Fill)
            .height(Length::Fixed(2.0))
            .into()
    };

    column![rounded, bottom_bar]
        .width(Length::Shrink)
        .height(Length::Fixed(36.0))
        .into()
}

// ===== 中间：DAG 画布（占满主区剩余空间），未打开建模时显示引导卡片 =====

fn view_middle(state: &UiState) -> Element<'_, Message> {
    let has_tabs = state.dag_editor.active_tab().is_some();

    if !has_tabs {
        // 未打开建模：精美的引导占位 + 快捷操作提示
        let guide_card = container(
            column![
                // 大图标：青色发光装饰
                container(
                    text("◇").color(Color {
                        r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 180.0/255.0
                    }).size(54.0)
                )
                .width(Length::Fixed(88.0))
                .height(Length::Fixed(88.0))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(|_t| {
                    let mut s = iced::widget::container::Style::default();
                    s.background = Some(Color {
                        r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 10.0/255.0
                    }.into());
                    s.border.radius = 24.0.into();
                    s.border.width = 1.0;
                    s.border.color = Color {
                        r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 35.0/255.0
                    };
                    s
                }),
                // 主标题
                text("开启你的量化建模之旅").color(theme::text_strong()).size(18.0),
                // 副标题
                text("选择左侧建模列表，或创建一个新的建模开始工作")
                    .color(theme::text_weak()).size(12.0),
                // 快捷操作：3 步提示
                container(
                    column![
                        row![
                            badge_num("1", theme::accent()),
                            text("在「建模列表」点击「+ 新建建模」")
                                .color(theme::text_hover()).size(11.5),
                        ].spacing(12).align_y(Alignment::Center),
                        row![
                            badge_num("2", theme::accent_teal()),
                            text("从「算子面板」拖拽算子到画布构建工作流")
                                .color(theme::text_hover()).size(11.5),
                        ].spacing(12).align_y(Alignment::Center),
                        row![
                            badge_num("3", theme::success()),
                            text("点击悬浮工具栏的「▶」一键运行全流程")
                                .color(theme::text_hover()).size(11.5),
                        ].spacing(12).align_y(Alignment::Center),
                    ]
                    .spacing(12)
                    .align_x(Alignment::Start)
                )
                .width(Length::Shrink)
                .padding(Padding { top: 20.0, bottom: 0.0, left: 0.0, right: 0.0 }),
            ]
            .spacing(14)
            .align_x(Alignment::Center)
        )
        .padding(Padding { top: 28.0, bottom: 28.0, left: 40.0, right: 40.0 })
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 27.0/255.0, g: 29.0/255.0, b: 34.0/255.0, a: 200.0/255.0
            }.into());
            s.border.radius = 20.0.into();
            s.border.width = 1.0;
            s.border.color = Color {
                r: 1.0, g: 1.0, b: 1.0, a: 28.0/255.0
            };
            s
        });

        let canvas_bg = super::dag_canvas::view_dag_canvas(state);

        // 在画布背景之上叠加引导层
        let stacked = iced::widget::Stack::with_children(vec![
            canvas_bg,
            container(guide_card)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into(),
        ])
        .width(Length::Fill)
        .height(Length::Fill);

        stacked.into()
    } else {
        let canvas = super::dag_canvas::view_dag_canvas(state);
        let toolbar = view_floating_toolbar(state);

        // 画布上叠加悬浮工具栏（顶部居中，向下偏移 12px）+ 右侧节点参数抽屉
        let mut layers: Vec<Element<'_, Message>> = vec![
            canvas,
            container(toolbar)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Start)
                .padding(Padding { top: 12.0, bottom: 0.0, left: 0.0, right: 0.0 })
                .into(),
        ];
        if let Some(drawer) = view_params_drawer(state) {
            layers.push(drawer);
        }
        // 右键菜单最顶层（坐标原点即画布原点，点击遮罩关闭）
        if let Some(ctx) = view_context_menu_if_any(state) {
            layers.push(ctx);
        }
        let stacked = iced::widget::Stack::with_children(layers)
            .width(Length::Fill)
            .height(Length::Fill);

        stacked.into()
    }
}

/// 右侧节点参数抽屉（由画布双击节点触发）。
///
/// 抽屉浮于画布之上、贴主区右边缘，固定宽 280px，全高填充；标题栏含算子名与
/// 关闭按钮。参数表复用 `view_params_body` 跟随 `selected_node_id` 自动刷新。
fn view_params_drawer<'a>(state: &'a UiState) -> Option<Element<'a, Message>> {
    let tab = state.dag_editor.active_tab()?;
    if !tab.params_drawer_open {
        return None;
    }

    // 标题：优先取选中节点的算子名，缺失时降级为"节点参数"
    let title = tab
        .selected_node_id
        .as_deref()
        .and_then(|nid| tab.graph.get_node(nid))
        .map(|n| n.operator_type.name().to_string())
        .unwrap_or_else(|| "节点参数".to_string());

    // 关闭按钮：透明底 + hover 提亮，红色 × 图标（与 tab 关闭按钮同源 IconKind::Close）
    let close_icon = icons::view_icon_with_stroke(IconKind::Close, theme::text_weak(), 11.0, 1.4);
    let close_content = container(close_icon)
        .width(Length::Fixed(26.0))
        .height(Length::Fixed(26.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    let close_btn = button(close_content)
        .width(Length::Fixed(26.0))
        .height(Length::Fixed(26.0))
        .padding(Padding::default())
        .on_press(Message::CloseParamsDrawer)
        .style(|_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(Color::TRANSPARENT.into());
            s.border.radius = 7.0.into();
            match status {
                iced::widget::button::Status::Hovered => {
                    s.background =
                        Some(Color { r: 1.0, g: 1.0, b: 1.0, a: 18.0 / 255.0 }.into());
                    s.text_color = theme::danger();
                }
                iced::widget::button::Status::Pressed => {
                    s.background =
                        Some(Color { r: 1.0, g: 1.0, b: 1.0, a: 28.0 / 255.0 }.into());
                    s.text_color = theme::danger();
                }
                _ => {}
            }
            s
        });

    // 标题区占满剩余宽度（左对齐），把关闭按钮顶到行尾最右侧
    let header = row![
        container(text(title).color(theme::text_strong()).size(12.0))
            .width(Length::Fill)
            .align_x(Alignment::Start)
            .align_y(Alignment::Center),
        close_btn,
    ]
    .align_y(Alignment::Center)
    .padding(Padding {
        top: 8.0,
        bottom: 8.0,
        left: 12.0,
        right: 8.0,
    });

    let header_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::divider()).into());
            s
        });

    // 复用左侧 sidebar 中的参数 body（未选中节点占位、参数表、滚动等）
    let body = view_params_body(tab);

    let col = column![header, header_divider, body]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    // 抽屉容器：贴右边缘、固定宽度、card 背景 + 圆角 + 边框
    let drawer = container(col)
        .width(Length::Fixed(PARAMS_DRAWER_WIDTH))
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.radius = theme::CARD_ROUNDING.into();
            s.border.width = 1.0;
            s.border.color = Color::from(theme::card_stroke());
            s
        });

    // 外层定位容器：把抽屉顶到右边缘，并留 12px 上下/右内侧留白
    let positioned = container(drawer)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::End)
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 12.0,
            bottom: 12.0,
            left: 0.0,
            right: 12.0,
        });

    Some(positioned.into())
}

/// 画布悬浮工具栏：顶部居中胶囊条，纯图标按钮（保存 / 执行 DAG / 调试）。
///
/// 设计要点：
/// - 半透明深色底 + 1px 微光边框 + 大圆角，漂浮于画布之上不遮挡节点
/// - 图标来自 `icons::view_icon`（IconKind::Save / Run / Debug），零外部资源依赖
/// - 次按钮透明底 + hover 提亮；主按钮 accent 实色填充；激活态（调试开）描边高亮
/// - 当激活 tab 至少有 2 个多选节点时，追加居上 / 居左对齐按钮
fn view_floating_toolbar(state: &UiState) -> Element<'_, Message> {
    let debug_on = state
        .dag_editor
        .active_tab()
        .map(|t| t.debug_mode)
        .unwrap_or(false);
    let multi_count = state
        .dag_editor
        .active_tab()
        .map(|t| t.selected_node_ids.len())
        .unwrap_or(0);
    let show_align = multi_count >= 2;

    let mut tools = row![
        icon_only_tool_button(IconKind::Save, Message::SaveTab, false),
        icon_only_tool_button(IconKind::Run, Message::RunAllClick, true),
        icon_only_tool_button(IconKind::Debug, Message::ToggleDebug, debug_on),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    // 多选场景：追加对齐组（分隔线 + 居上 / 居左两个按钮）
    if show_align {
        tools = tools.push(toolbar_divider());
        tools = tools.push(icon_only_tool_button(
            IconKind::AlignTop,
            Message::AlignTop,
            false,
        ));
        tools = tools.push(icon_only_tool_button(
            IconKind::AlignLeft,
            Message::AlignLeft,
            false,
        ));
    }

    container(tools)
        .padding(Padding { top: 4.0, bottom: 4.0, left: 6.0, right: 6.0 })
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 13.0/255.0, g: 14.0/255.0, b: 17.0/255.0, a: 210.0/255.0
            }.into());
            s.border.radius = 12.0.into();
            s.border.width = 1.0;
            s.border.color = Color {
                r: 1.0, g: 1.0, b: 1.0, a: 28.0/255.0
            };
            s
        })
        .into()
}

/// 悬浮工具栏分组分隔线：1px 宽 + 16px 高的半透明竖线，与按钮同高对齐。
fn toolbar_divider() -> Element<'static, Message> {
    container(row![])
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(16.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 1.0, g: 1.0, b: 1.0, a: 28.0 / 255.0
            }.into());
            s
        })
        .into()
}

/// 悬浮工具栏纯图标按钮：方形小图标，主按钮 accent 实色填充；激活态（调试开）描边高亮。
fn icon_only_tool_button(
    icon: IconKind,
    msg: Message,
    primary_or_active: bool,
) -> Element<'static, Message> {
    const SIZE: f32 = 30.0;
    const ICON: f32 = 16.0;
    let is_primary = matches!(icon, IconKind::Run);

    let icon_color = if is_primary {
        // 主按钮：深青实色底 → 图标用 WHITE 保证对比度
        Color::WHITE
    } else if primary_or_active {
        // 激活态（调试开）：半透明青底 → 亮青图标
        theme::accent_bright()
    } else {
        theme::text_hover()
    };

    let content = container(icons::view_icon(icon, icon_color, ICON))
        .width(Length::Fixed(SIZE))
        .height(Length::Fixed(SIZE))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);

    button(content)
        .width(Length::Fixed(SIZE))
        .height(Length::Fixed(SIZE))
        .padding(Padding { top: 0.0, bottom: 0.0, left: 0.0, right: 0.0 })
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.border.radius = 9.0.into();
            if primary_or_active {
                // 主按钮（▶ 执行 DAG）：深青实色 + 亮青边框高光；调试激活态：半透明底 + accent 描边
                let is_primary = matches!(icon, IconKind::Run);
                if is_primary {
                    s.background = Some(Color::from(theme::accent()).into());
                    s.text_color = Color::WHITE;
                    s.border.width = 1.0;
                    s.border.color = Color {
                        r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 1.0
                    };
                    if matches!(status, iced::widget::button::Status::Hovered) {
                        s.background = Some(Color::from(theme::accent_bright()).into());
                    } else if matches!(status, iced::widget::button::Status::Pressed) {
                        s.background = Some(Color::from(theme::accent_dark()).into());
                    }
                } else {
                    s.background = Some(Color {
                        r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 30.0/255.0
                    }.into());
                    s.text_color = theme::accent_bright();
                    s.border.width = 1.0;
                    s.border.color = Color {
                        r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 180.0/255.0
                    };
                    if matches!(status, iced::widget::button::Status::Hovered) {
                        s.background = Some(Color {
                            r: 14.0/255.0, g: 116.0/255.0, b: 144.0/255.0, a: 50.0/255.0
                        }.into());
                    }
                }
            } else {
                s.background = Some(Color::TRANSPARENT.into());
                s.text_color = theme::text_hover();
                s.border.width = 1.0;
                s.border.color = Color::TRANSPARENT;
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color {
                        r: 1.0, g: 1.0, b: 1.0, a: 18.0/255.0
                    }.into());
                    s.text_color = theme::text_strong();
                }
            }
            s
        })
        .on_press(msg)
        .into()
}

/// 步骤编号徽章：圆形 + 语义色背景 + 白色数字
fn badge_num(n: &'static str, bg_color: Color) -> Element<'static, Message> {
    container(text(n).color(Color::WHITE).size(11.0))
        .width(Length::Fixed(22.0))
        .height(Length::Fixed(22.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(bg_color.into());
            s.border.radius = 11.0.into();
            s
        })
        .into()
}

/// 左侧面板「算子面板」子页 v2：搜索框 + 算子目录（占满全高）。
/// 节点参数已迁移至右侧抽屉（画布双击节点弹出，详见 `view_params_drawer`）。
fn view_operator_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;
    let search_value = editor
        .active_tab()
        .map(|t| t.operator_search_filter.clone())
        .unwrap_or_default();

    // 标题栏
    let header = container(
        row![
            text("算子面板").color(theme::text_strong()).size(13.0),
            text("点击添加节点").color(theme::text_weak()).size(9.5),
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

    // 搜索框 v2
    let search = text_input("搜索算子…", &search_value)
        .on_input(Message::OperatorSearchInput)
        .width(Length::Fill)
        .size(11.0)
        .padding(Padding { top: 7.0, bottom: 7.0, left: 10.0, right: 10.0 });
    let search_wrap = container(
        row![
            text("⌕").color(theme::text_weak()).size(12.0),
            search,
        ]
        .spacing(6)
        .align_y(Alignment::Center)
    )
    .width(Length::Fill)
    .padding(Padding { top: 0.0, bottom: 0.0, left: 8.0, right: 8.0 })
    .style(|_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(Color::from(theme::card_bg()).into());
        s.border.color = theme::card_stroke();
        s.border.width = 1.0;
        s.border.radius = theme::WIDGET_ROUNDING.into();
        s
    });
    let search_container = container(search_wrap)
        .width(Length::Fill)
        .padding(Padding { top: 8.0, bottom: 6.0, left: 10.0, right: 10.0 });

    // 算子目录递归渲染
    let categories = crate::mining::dag::get_operator_categories();
    let filter = search_value.trim().to_lowercase();
    let mut op_col = column![].spacing(5).padding(Padding {
        top: 2.0, bottom: 8.0, left: 8.0, right: 8.0,
    });
    render_operator_categories(&categories, &filter, 0, &mut op_col);
    let op_scroll = scrollable(op_col)
        .direction(scrollable::Direction::Vertical(theme::cool_scrollbar()))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::cool_scrollbar_style());

    let op_col_top = column![search_container, op_scroll]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    // 节点参数已迁移至右侧抽屉（画布双击节点弹出），左侧算子面板占满全高。
    let col = column![header, header_divider, op_col_top]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(col).width(Length::Fill).height(Length::Fill).into()
}

/// 递归渲染算子分类树（分类名 + 子分类 + 算子卡片）。
///
/// `depth` 控制左侧缩进（每层 8px）；空 filter 不过滤；非空 filter 时，
/// 只有匹配的算子/子分类才展示；匹配的算子卡片按 `AddOperator(name)` 发消息。
///
/// 注意：所有文本字段都使用 `.to_string()` 取得所有权，避免元素树借用
/// 临时 `categories` 本地变量导致的生命周期错误（E0515）。
fn render_operator_categories(
    categories: &[operator_executor_client::protocol::OperatorCategory],
    filter: &str,
    depth: u32,
    col: &mut iced::widget::Column<'_, Message>,
) {
    use iced::widget::column as col_elem;
    for cat in categories {
        // 收集该分类下匹配的算子卡片列表（拷贝必要字段，避免后续借用 categories）
        let mut matched_ops: Vec<(
            String,
            String,
            [u8; 3],
        )> = Vec::new();
        for op in &cat.operators {
            if filter.is_empty() || op.name.to_lowercase().contains(filter) {
                let desc = if op.summary.is_empty() {
                    op.description.clone()
                } else {
                    op.summary.clone()
                };
                matched_ops.push((op.name.clone(), desc, op.color));
            }
        }
        // 递归收集子分类（即使本层无匹配，子分类匹配也算本分类需要展示）
        let sub_has_match = !filter.is_empty() && {
            let mut stack: Vec<&[operator_executor_client::protocol::OperatorCategory]> = Vec::new();
            stack.push(&cat.subcategories);
            let mut hit = false;
            while let Some(cats) = stack.pop() {
                for sub in cats {
                    for op in &sub.operators {
                        if op.name.to_lowercase().contains(filter) {
                            hit = true;
                            break;
                        }
                    }
                    if !sub.subcategories.is_empty() {
                        stack.push(&sub.subcategories);
                    }
                }
                if hit {
                    break;
                }
            }
            hit
        };

        let show_category = filter.is_empty() || !matched_ops.is_empty() || sub_has_match;
        if !show_category {
            continue;
        }

        let indent = depth as f32 * 10.0;
        // 分类标签：弱化颜色（非 accent），让算子卡片更突出；加竖向留白
        let label = container(
            text(cat.name.clone())
                .color(theme::text_weak())
                .size(10.0),
        )
        .width(Length::Fill)
        .padding(Padding {
            top: 6.0,
            bottom: 2.0,
            left: indent + 2.0,
            right: 0.0,
        });
        *col = std::mem::replace(col, col_elem![]).push(label);

        for (op_name, op_desc, op_color) in matched_ops {
            let color = Color::from_rgb8(op_color[0], op_color[1], op_color[2]);
            let name_owned = op_name.clone();
            let desc_owned = op_desc.clone();
            let op_name_for_msg = op_name.clone();

            // 色点（小圆点，替代色条更精致）
            let dot = container(text("").size(1.0))
                .width(Length::Fixed(8.0))
                .height(Length::Fixed(8.0))
                .style(move |_t| {
                    let mut s = iced::widget::container::Style::default();
                    s.background = Some(color.into());
                    s.border.radius = theme::PILL_ROUNDING.into();
                    s
                });

            let card_btn = button(
                row![
                    dot,
                    column![
                        text(name_owned).color(theme::text_strong()).size(11.0),
                        text(desc_owned).color(theme::text_weak()).size(9.0),
                    ]
                    .spacing(1)
                    .width(Length::Fill)
                    .align_x(Alignment::Start),
                ]
                .align_y(Alignment::Center)
                .spacing(8)
                .width(Length::Fill),
            )
            .width(Length::Fill)
            .on_press(Message::AddOperator(op_name_for_msg))
            .padding(Padding { top: 8.0, bottom: 8.0, left: 10.0, right: 10.0 })
            .style(move |_t, status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.border.color = theme::card_stroke();
                s.border.width = 1.0;
                s.border.radius = 9.0.into();
                s.text_color = theme::text_strong();
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::card_hover_bg()).into());
                    s.border.color = theme::accent_dim();
                }
                s
            });

            let card = container(card_btn).padding(Padding {
                top: 0.0, bottom: 0.0, left: indent, right: 0.0,
            });
            *col = std::mem::replace(col, col_elem![]).push(card);
        }

        // 子分类（相同 filter）
        render_operator_categories(&cat.subcategories, filter, depth + 1, col);
    }
}

/// 参数面板 body：未选中节点 → 占位；选中节点 → 端口列表（输入/输出）+ 参数表单。
///
/// 算子名只在抽屉标题栏显示，body 不再重复。布局自上而下：
///   1. 输入端口区（仅当存在时）— 每行：端口名 + 类型徽标
///   2. 输出端口区（仅当存在时）— 每行：端口名 + 类型徽标
///   3. 参数区（仅当存在时）— 每条：参数名+类型行 + 控件
///      - `Bool` 用 `checkbox` 渲染（与设置页一致的小开关风格）
///      - `Text` 用 `text_editor` 渲染（多行编辑器，便于 SQL/提示词等长文本）
///      - 其余 `Float/Int/String` 用 `text_input` 渲染
fn view_params_body<'a>(tab: &'a DagTab) -> Element<'a, Message> {
    let Some(node_id) = &tab.selected_node_id else {
        return container(
            text("(未选中节点)")
                .color(theme::text_weak())
                .size(11.0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into();
    };
    let Some(node) = tab.graph.get_node(node_id) else {
        return container(
            text("(节点已删除)")
                .color(theme::text_weak())
                .size(11.0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into();
    };
    let input_defs = node.operator_type.input_defs();
    let output_defs = node.operator_type.output_defs();
    let param_defs = node.operator_type.param_defs();
    if input_defs.is_empty() && output_defs.is_empty() && param_defs.is_empty() {
        return container(
            text("(该算子无输入/输出/参数)")
                .color(theme::text_weak())
                .size(10.0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Start)
        .align_y(Alignment::Start)
        .padding(Padding {
            top: 10.0,
            bottom: 12.0,
            left: 12.0,
            right: 10.0,
        })
        .into();
    }

    let mut col = column![].spacing(6)
    // 左右留白避免内容贴抽屉边框（右侧略窄，给滚动条留位）；
    // 顶部与标题分隔线、底部与抽屉边缘留出呼吸空间
    .padding(Padding {
        top: 10.0,
        bottom: 12.0,
        left: 12.0,
        right: 10.0,
    });

    // ===== 端口区（输入/输出）=====
    if !input_defs.is_empty() {
        col = col.push(view_port_section("输入", &input_defs, PortDirection::Input));
    }
    if !output_defs.is_empty() {
        col = col.push(view_port_section("输出", &output_defs, PortDirection::Output));
    }

    // ===== 参数区 =====
    if !param_defs.is_empty() {
        col = col.push(view_section_header("参数"));
        let mut params_col = column![].spacing(6);
        for def in param_defs {
            params_col = params_col.push(view_param_row(tab, node_id, node, def));
        }
        col = col.push(params_col);
    }

    scrollable(col)
        .direction(scrollable::Direction::Vertical(theme::cool_scrollbar()))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::cool_scrollbar_style())
        .into()
}

/// 渲染端口区域块：小标题 + 端口列表。
///
/// 输入端口类型徽标用 `accent_teal`（电光青）突出「上游数据进入」；
/// 输出端口用 `accent_blue`（天蓝）突出「下游数据产出」。
fn view_port_section<'a>(
    title: &str,
    defs: &[&'a OperatorPortParamDef],
    direction: PortDirection,
) -> Element<'a, Message> {
    let badge_color = match direction {
        PortDirection::Input => theme::accent_teal(),
        PortDirection::Output => theme::accent_blue(),
        PortDirection::Param => theme::text_weak(),
    };
    let mut list = column![].spacing(2);
    for def in defs {
        list = list.push(row![
            text(def.name.as_str())
                .color(theme::text_strong())
                .size(10.5),
            view_type_badge(def.param_type.to_str(), badge_color),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .width(Length::Fill));
    }
    column![
        view_section_header(title),
        container(list)
            .width(Length::Fill)
            .padding(Padding {
                top: 2.0,
                bottom: 2.0,
                left: 8.0,
                right: 0.0,
            }),
    ]
    .spacing(3)
    .into()
}

/// 渲染参数行：参数名 + 类型徽标 + 控件（Bool→checkbox / Text→text_editor / 其余→text_input）。
fn view_param_row<'a>(
    tab: &'a DagTab,
    node_id: &str,
    node: &'a crate::mining::dag::Node,
    def: &'a OperatorPortParamDef,
) -> Element<'a, Message> {
    let current = node
        .operator_type
        .get_param_value(&def.name)
        .unwrap_or_default();
    let header = row![
        text(def.name.as_str())
            .color(theme::text_strong())
            .size(11.0),
        view_type_badge(def.param_type.to_str(), theme::text_weak()),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .width(Length::Fill);

    let body: Element<'a, Message> = match def.param_type {
        ParamType::Bool => {
            // 复选框开关：把 ParamInput 消息以 "true"/"false" 字符串派发，
            // 复用现有 set_param_value 落盘路径；同时与设置页开关风格保持一致。
            let checked = current.parse::<bool>().unwrap_or(false);
            let nid = node_id.to_string();
            let pname = def.name.clone();
            let label_text = if checked { "true" } else { "false" };
            let toggle = checkbox(checked)
                .label(label_text.to_string())
                .on_toggle(move |v| Message::ParamInput(nid.clone(), pname.clone(), v.to_string()))
                .size(13.0)
                .spacing(6);
            container(toggle)
                .width(Length::Fill)
                .padding(Padding {
                    top: 2.0,
                    bottom: 2.0,
                    left: 0.0,
                    right: 0.0,
                })
                .into()
        }
        ParamType::Text => {
            // 长文本：多行 text_editor（SQL/提示词等）。预热由 AnimTick 兜底，
            // 若缓存缺失（极少数情况下预热未及时触发）则降级为单行 text_input。
            let key = format!("{}::{}", node_id, def.name);
            if let Some(content) = tab.text_editors.get(&key) {
                let nid = node_id.to_string();
                let pname = def.name.clone();
                text_editor(content)
                    .on_action(move |a| Message::ParamTextEdit(nid.clone(), pname.clone(), a))
                    .height(Length::Fixed(140.0))
                    .padding(Padding {
                        top: 5.0,
                        bottom: 5.0,
                        left: 6.0,
                        right: 6.0,
                    })
                    .style(|_t, status| {
                        // 默认状态样式（Active）：深炭灰底 + 灰边框 + 圆角 6px
                        let base = iced::widget::text_editor::Style {
                            background: Background::Color(Color::from(theme::canvas_bg())),
                            border: iced::Border {
                                color: Color::from(theme::card_stroke()),
                                width: 1.0,
                                radius: 6.0.into(),
                            },
                            placeholder: Color::from(theme::text_weak()),
                            value: Color::from(theme::text_strong()),
                            selection: Color::from(theme::accent()),
                        };
                        match status {
                            iced::widget::text_editor::Status::Hovered => iced::widget::text_editor::Style {
                                border: iced::Border {
                                    width: 1.2,
                                    ..base.border
                                },
                                ..base
                            },
                            iced::widget::text_editor::Status::Focused { .. } => iced::widget::text_editor::Style {
                                border: iced::Border {
                                    color: Color::from(theme::accent_teal()),
                                    width: 1.4,
                                    ..base.border
                                },
                                ..base
                            },
                            _ => base,
                        }
                    })
                    .into()
            } else {
                let nid = node_id.to_string();
                let pname = def.name.clone();
                text_input(&def.name, &current)
                    .on_input(move |v| Message::ParamInput(nid.clone(), pname.clone(), v))
                    .width(Length::Fill)
                    .size(11.0)
                    .padding(Padding {
                        top: 3.0,
                        bottom: 3.0,
                        left: 5.0,
                        right: 5.0,
                    })
                    .into()
            }
        }
        _ => {
            // Float / Int / String：单行 text_input
            let nid = node_id.to_string();
            let pname = def.name.clone();
            text_input(&def.name, &current)
                .on_input(move |v| Message::ParamInput(nid.clone(), pname.clone(), v))
                .width(Length::Fill)
                .size(11.0)
                .padding(Padding {
                    top: 3.0,
                    bottom: 3.0,
                    left: 5.0,
                    right: 5.0,
                })
                .into()
        }
    };

    column![header, body].spacing(2).into()
}

/// 渲染分组小标题：弱化色 + 全宽细线分隔（与设置页分组风格一致）。
fn view_section_header(label: &str) -> Element<'static, Message> {
    row![
        text(label.to_string())
            .color(theme::text_weak())
            .size(9.5),
        container(row![])
            .width(Length::Fill)
            .height(Length::Fixed(1.0))
            .style(|_t| {
                let mut s = iced::widget::container::Style::default();
                s.background = Some(Color::from(theme::divider()).into());
                s
            }),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .into()
}

/// 渲染类型徽标：极小号文字 + 弱化色填充底 + 1px 边框 + 圆角。
/// 用于端口/参数行右侧的类型标识（DataFrame、Int、长文本 等）。
fn view_type_badge(label: &str, color: Color) -> Element<'static, Message> {
    container(
        text(label.to_string())
            .color(color)
            .size(8.5),
    )
    .padding(Padding {
        top: 1.0,
        bottom: 1.0,
        left: 4.0,
        right: 4.0,
    })
    .style(move |_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(Color { r: color.r, g: color.g, b: color.b, a: 24.0 / 255.0 }.into());
        s.border = iced::Border {
            color,
            width: 0.8,
            radius: 4.0.into(),
        };
        s
    })
    .into()
}

// ===== 画布 / 节点右键菜单叠加（画布 Stack 最上层） =====
//
// 坐标说明：本叠加层挂在 view_middle 的画布 Stack 内，原点即画布左上角，
// 与 DagProgram 上报的右键坐标同坐标系——菜单直接在右键位置弹出，
// 不会再被活动栏 / 侧栏 / 顶栏挤出 ~300px 的偏移。

/// 右键菜单固定宽度。
const CTX_MENU_WIDTH: f32 = 168.0;
/// 单个菜单项高度。
const CTX_ITEM_HEIGHT: f32 = 30.0;
/// 菜单卡片内边距（上下左右）。
const CTX_CARD_PAD: f32 = 5.0;
/// 分隔线占用高度（上下各 4px 呼吸）。
const CTX_SEPARATOR_H: f32 = 9.0;
/// 菜单弹出位置距光标的微偏移，避免指针压住边框。
const CTX_CURSOR_OFFSET: f32 = 2.0;
/// 菜单距画布边缘的最小留白。
const CTX_EDGE_MARGIN: f32 = 6.0;

/// 渲染画布/节点右键菜单（画布 Stack 最上层卡片）。
///
/// 若 `context_menu_node_id` 为 Some → 节点菜单（运行到此节点 / 删除节点）；
/// 否则 → 画布空白菜单（重置视图）。点击卡片外遮罩关闭菜单。
fn view_context_menu_if_any(state: &UiState) -> Option<Element<'_, Message>> {
    let tab = state.dag_editor.active_tab()?;
    let screen_pos = tab.context_menu_screen_pos?;
    let node_id = tab.context_menu_node_id.clone();

    // 菜单总高度（用于靠近底边时向上翻转）
    let menu_height = if node_id.is_some() {
        CTX_CARD_PAD * 2.0 + CTX_ITEM_HEIGHT * 2.0 + CTX_SEPARATOR_H
    } else {
        CTX_CARD_PAD * 2.0 + CTX_ITEM_HEIGHT
    };

    // responsive 取得画布层实际尺寸，做右/下边缘翻转
    let overlay = iced::widget::responsive(move |size| {
        // ---- 菜单项 ----
        let mut items: Vec<Element<'_, Message>> = Vec::new();
        if let Some(nid) = &node_id {
            items.push(context_menu_item(
                IconKind::Run,
                "运行到此节点",
                Message::RunUpToNode(nid.clone()),
                MenuItemTone::Accent,
            ));
            items.push(context_menu_separator());
            items.push(context_menu_item(
                IconKind::Trash,
                "删除节点",
                Message::DeleteNodeClick(nid.clone()),
                MenuItemTone::Danger,
            ));
        } else {
            items.push(context_menu_item(
                IconKind::FitView,
                "重置视图",
                Message::ResetCanvasView,
                MenuItemTone::Normal,
            ));
        }

        // ---- 定位：默认在光标的右下；超出右/下边缘则贴边翻转 ----
        let mut x = screen_pos.x + CTX_CURSOR_OFFSET;
        let mut y = screen_pos.y + CTX_CURSOR_OFFSET;
        if size.width >= CTX_MENU_WIDTH + CTX_EDGE_MARGIN * 2.0
            && x + CTX_MENU_WIDTH > size.width - CTX_EDGE_MARGIN
        {
            x = (size.width - CTX_MENU_WIDTH - CTX_EDGE_MARGIN).max(CTX_EDGE_MARGIN);
        }
        if size.height >= menu_height + CTX_EDGE_MARGIN * 2.0
            && y + menu_height > size.height - CTX_EDGE_MARGIN
        {
            y = (size.height - menu_height - CTX_EDGE_MARGIN).max(CTX_EDGE_MARGIN);
        }

        let card = container(column(items).spacing(0))
            .width(Length::Fixed(CTX_MENU_WIDTH))
            .padding(Padding {
                top: CTX_CARD_PAD,
                bottom: CTX_CARD_PAD,
                left: CTX_CARD_PAD,
                right: CTX_CARD_PAD,
            })
            .style(|_t| {
                let mut s = iced::widget::container::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.border.radius = 10.0.into();
                s.border.width = 1.0;
                s.border.color = theme::card_stroke();
                s.shadow = iced::Shadow {
                    color: Color { r: 0.0, g: 0.0, b: 0.0, a: 150.0 / 255.0 },
                    offset: iced::Vector::new(0.0, 6.0),
                    blur_radius: 20.0,
                };
                s
            });

        let positioned = container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding {
                top: y.max(0.0),
                bottom: 0.0,
                left: x.max(0.0),
                right: 0.0,
            })
            .align_x(Alignment::Start)
            .align_y(Alignment::Start);

        // 透明遮罩：点击任意处关闭菜单
        let mask = mouse_area(
            container(text("")).width(Length::Fill).height(Length::Fill),
        )
        .on_press(Message::ContextMenuClose);

        Stack::with_children(vec![mask.into(), positioned.into()])
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    });

    Some(overlay.into())
}

/// 菜单项视觉语义：普通 / 强调（运行）/ 危险（删除）。
#[derive(Clone, Copy)]
enum MenuItemTone {
    Normal,
    Accent,
    Danger,
}

/// 构造一个右键菜单项：左侧矢量图标 + 文字，hover/press 有高亮反馈。
fn context_menu_item<'a>(
    icon: IconKind,
    label: &'a str,
    message: Message,
    tone: MenuItemTone,
) -> Element<'a, Message> {
    let (icon_color, label_color): (Color, Color) = match tone {
        MenuItemTone::Normal => (theme::text_weak(), theme::text_strong()),
        MenuItemTone::Accent => (theme::accent_teal(), theme::text_strong()),
        MenuItemTone::Danger => (theme::danger(), theme::danger()),
    };

    let icon_slot = container(icons::view_icon(icon, icon_color, 12.5))
        .width(Length::Fixed(15.0))
        .align_x(Alignment::Center);

    let content = row![
        icon_slot,
        text(label).color(label_color).size(11.5),
    ]
    .spacing(9.0)
    .height(Length::Fill)
    .align_y(Alignment::Center);

    button(content)
        .width(Length::Fill)
        .height(Length::Fixed(CTX_ITEM_HEIGHT))
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 9.0,
            right: 9.0,
        })
        .on_press(message)
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.border.radius = 6.0.into();
            s.border.width = 0.0;
            s.text_color = label_color;
            match status {
                iced::widget::button::Status::Hovered => {
                    s.background = Some(match tone {
                        MenuItemTone::Danger => Color {
                            r: 248.0 / 255.0,
                            g: 113.0 / 255.0,
                            b: 113.0 / 255.0,
                            a: 26.0 / 255.0,
                        },
                        _ => Color::from(theme::hover_bg()),
                    }
                    .into());
                }
                iced::widget::button::Status::Pressed => {
                    s.background = Some(match tone {
                        MenuItemTone::Danger => Color {
                            r: 248.0 / 255.0,
                            g: 113.0 / 255.0,
                            b: 113.0 / 255.0,
                            a: 40.0 / 255.0,
                        },
                        _ => Color::from(theme::pressed_bg()),
                    }
                    .into());
                }
                _ => {
                    s.background = Some(Color::TRANSPARENT.into());
                }
            }
            s
        })
        .into()
}

/// 菜单项之间的细分隔线（左右各留 8px 呼吸）。
fn context_menu_separator<'a>() -> Element<'a, Message> {
    let line = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::divider()).into());
            s
        });

    container(line)
        .width(Length::Fill)
        .height(Length::Fixed(CTX_SEPARATOR_H))
        .padding(Padding {
            top: 4.0,
            bottom: 4.0,
            left: 8.0,
            right: 8.0,
        })
        .into()
}

// ===== 对话框叠加层 v2 =====

fn view_new_model_dialog(state: &UiState) -> Element<'_, Message> {
    let icon = container(text("✦").color(theme::accent()).size(22.0))
        .width(Length::Fixed(44.0))
        .height(Length::Fixed(44.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 15.0/255.0
            }.into());
            s.border.radius = 12.0.into();
            s
        });
    let new_model_hint = match state.dag_editor.current_folder.rsplit('/').next() {
        Some(last) if !last.is_empty() => format!("将创建在目录「{}」中", last),
        _ => "为新的建模起一个名字".to_string(),
    };
    let title_col = column![
        text("新建建模").color(theme::text_strong()).size(15.0),
        text(new_model_hint).color(theme::text_weak()).size(10.5),
    ].spacing(2);

    let input = text_input("建模名称…", &state.dag_editor.new_model_name_input)
        .on_input(Message::NewModelNameInput)
        .on_submit(Message::NewModelConfirm)
        .size(12.0)
        .padding(Padding { top: 8.0, bottom: 8.0, left: 10.0, right: 10.0 });
    let input_wrap = container(input)
        .width(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.radius = theme::WIDGET_ROUNDING.into();
            s.border.width = 1.0;
            s.border.color = theme::card_stroke();
            s
        });

    let confirm_btn = dialog_button("确认创建", Message::NewModelConfirm, true);
    let cancel_btn = dialog_button("取消", Message::NewModelCancel, false);
    let btns = row![row![].width(Length::Fill), cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    // 用 iced_aw::Card 三段式：head=图标+标题, body=输入框, foot=按钮栏
    // padding 分段控制：head 上 20px、body 中间 16px、foot 下 18px，水平统一 20px
    let card = Card::new(
        row![icon, title_col].spacing(12).align_y(Alignment::Center).width(Length::Fill),
        input_wrap,
    )
    .foot(btns)
    .style(theme::float_card_style())
    .padding_head(Padding { top: 20.0, bottom: 0.0, left: 20.0, right: 20.0 })
    .padding_body(Padding { top: 16.0, bottom: 16.0, left: 20.0, right: 20.0 })
    .padding_foot(Padding { top: 0.0, bottom: 18.0, left: 20.0, right: 20.0 })
    .width(Length::Fixed(360.0));

    dialog_overlay(card.into(), Message::NewModelCancel)
}

fn view_rename_dialog(state: &UiState) -> Element<'_, Message> {
    let icon = container(text("✎").color(theme::accent_teal()).size(20.0))
        .width(Length::Fixed(44.0))
        .height(Length::Fixed(44.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 34.0/255.0, g: 211.0/255.0, b: 238.0/255.0, a: 15.0/255.0
            }.into());
            s.border.radius = 12.0.into();
            s
        });
    let title_col = column![
        text("重命名建模").color(theme::text_strong()).size(15.0),
        text("输入新的建模名称").color(theme::text_weak()).size(10.5),
    ].spacing(2);

    let input = text_input("新名称…", &state.dag_editor.rename_input)
        .on_input(Message::RenameInput)
        .on_submit(Message::RenameConfirm)
        .size(12.0)
        .padding(Padding { top: 8.0, bottom: 8.0, left: 10.0, right: 10.0 });
    let input_wrap = container(input)
        .width(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.radius = theme::WIDGET_ROUNDING.into();
            s.border.width = 1.0;
            s.border.color = theme::card_stroke();
            s
        });

    let confirm_btn = dialog_button("确认", Message::RenameConfirm, true);
    let cancel_btn = dialog_button("取消", Message::RenameCancel, false);
    let btns = row![row![].width(Length::Fill), cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    // iced_aw::Card 三段式替换 container(content)
    let card = Card::new(
        row![icon, title_col].spacing(12).align_y(Alignment::Center).width(Length::Fill),
        input_wrap,
    )
    .foot(btns)
    .style(theme::float_card_style())
    .padding_head(Padding { top: 20.0, bottom: 0.0, left: 20.0, right: 20.0 })
    .padding_body(Padding { top: 16.0, bottom: 16.0, left: 20.0, right: 20.0 })
    .padding_foot(Padding { top: 0.0, bottom: 18.0, left: 20.0, right: 20.0 })
    .width(Length::Fixed(360.0));

    dialog_overlay(card.into(), Message::RenameCancel)
}

fn view_delete_confirm_dialog(state: &UiState) -> Element<'_, Message> {
    let name = state
        .dag_editor
        .delete_model_target_name
        .clone()
        .unwrap_or_default();

    let icon = container(text("!").color(theme::danger()).size(22.0))
        .width(Length::Fixed(44.0))
        .height(Length::Fixed(44.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 248.0/255.0, g: 113.0/255.0, b: 113.0/255.0, a: 15.0/255.0
            }.into());
            s.border.radius = 12.0.into();
            s
        });
    let title_col = column![
        text("删除建模").color(theme::danger()).size(15.0),
        text(format!("确定删除「{}」吗？此操作可手动恢复（.deleted）。", name))
            .color(theme::text_hover())
            .size(10.5),
    ].spacing(2);

    let confirm_btn = dialog_button("确认删除", Message::DeleteModelConfirm, true);
    let cancel_btn = dialog_button("取消", Message::DeleteModelCancel, false);
    let btns = row![row![].width(Length::Fill), cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    // iced_aw::Card 三段式：head=图标+提示文字, body=空（仅留呼吸空间）, foot=按钮栏
    let card = Card::new(
        row![icon, title_col].spacing(12).align_y(Alignment::Center).width(Length::Fill),
        text(""),
    )
    .foot(btns)
    .style(theme::float_card_style())
    .padding_head(Padding { top: 20.0, bottom: 0.0, left: 20.0, right: 20.0 })
    .padding_body(Padding { top: 8.0, bottom: 8.0, left: 20.0, right: 20.0 })
    .padding_foot(Padding { top: 0.0, bottom: 18.0, left: 20.0, right: 20.0 })
    .width(Length::Fixed(380.0));

    dialog_overlay(card.into(), Message::DeleteModelCancel)
}

/// 对话框头部 44×44 圆角图标块（内嵌矢量图标）。
fn dialog_icon_block(icon: Element<'static, Message>) -> Element<'static, Message> {
    container(icon)
        .width(Length::Fixed(44.0))
        .height(Length::Fixed(44.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 34.0 / 255.0, g: 211.0 / 255.0, b: 238.0 / 255.0, a: 15.0 / 255.0,
            }.into());
            s.border.radius = 12.0.into();
            s
        })
        .into()
}

/// 新建目录对话框：在当前浏览目录下创建一个分类子目录。
fn view_new_folder_dialog(state: &UiState) -> Element<'_, Message> {
    let icon = dialog_icon_block(icons::view_icon(IconKind::Folder, theme::accent(), 22.0));
    let hint = match state.dag_editor.current_folder.rsplit('/').next() {
        Some(last) if !last.is_empty() => format!("在目录「{}」中创建子目录", last),
        _ => "创建一个目录来分类管理建模".to_string(),
    };
    let title_col = column![
        text("新建目录").color(theme::text_strong()).size(15.0),
        text(hint).color(theme::text_weak()).size(10.5),
    ]
    .spacing(2);

    let input = text_input(
        "目录名称…",
        &state.dag_editor.new_folder_name_input,
    )
    .on_input(Message::NewFolderNameInput)
    .on_submit(Message::NewFolderConfirm)
    .size(12.0)
    .padding(Padding { top: 8.0, bottom: 8.0, left: 10.0, right: 10.0 });
    let input_wrap = container(input)
        .width(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.radius = theme::WIDGET_ROUNDING.into();
            s.border.width = 1.0;
            s.border.color = theme::card_stroke();
            s
        });

    let confirm_btn = dialog_button("确认创建", Message::NewFolderConfirm, true);
    let cancel_btn = dialog_button("取消", Message::NewFolderCancel, false);
    let btns = row![row![].width(Length::Fill), cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let card = Card::new(
        row![icon, title_col].spacing(12).align_y(Alignment::Center).width(Length::Fill),
        input_wrap,
    )
    .foot(btns)
    .style(theme::float_card_style())
    .padding_head(Padding { top: 20.0, bottom: 0.0, left: 20.0, right: 20.0 })
    .padding_body(Padding { top: 16.0, bottom: 16.0, left: 20.0, right: 20.0 })
    .padding_foot(Padding { top: 0.0, bottom: 18.0, left: 20.0, right: 20.0 })
    .width(Length::Fixed(360.0));

    dialog_overlay(card.into(), Message::NewFolderCancel)
}

/// 重命名目录对话框（只改目录末段名，不影响层级）。
fn view_rename_folder_dialog(state: &UiState) -> Element<'_, Message> {
    let icon = dialog_icon_block(icons::view_icon(IconKind::Pencil, theme::accent_teal(), 20.0));
    let title_col = column![
        text("重命名目录").color(theme::text_strong()).size(15.0),
        text("输入新的目录名称").color(theme::text_weak()).size(10.5),
    ]
    .spacing(2);

    let input = text_input("新名称…", &state.dag_editor.rename_folder_input)
        .on_input(Message::RenameFolderInput)
        .on_submit(Message::RenameFolderConfirm)
        .size(12.0)
        .padding(Padding { top: 8.0, bottom: 8.0, left: 10.0, right: 10.0 });
    let input_wrap = container(input)
        .width(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.radius = theme::WIDGET_ROUNDING.into();
            s.border.width = 1.0;
            s.border.color = theme::card_stroke();
            s
        });

    let confirm_btn = dialog_button("确认", Message::RenameFolderConfirm, true);
    let cancel_btn = dialog_button("取消", Message::RenameFolderCancel, false);
    let btns = row![row![].width(Length::Fill), cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let card = Card::new(
        row![icon, title_col].spacing(12).align_y(Alignment::Center).width(Length::Fill),
        input_wrap,
    )
    .foot(btns)
    .style(theme::float_card_style())
    .padding_head(Padding { top: 20.0, bottom: 0.0, left: 20.0, right: 20.0 })
    .padding_body(Padding { top: 16.0, bottom: 16.0, left: 20.0, right: 20.0 })
    .padding_foot(Padding { top: 0.0, bottom: 18.0, left: 20.0, right: 20.0 })
    .width(Length::Fixed(360.0));

    dialog_overlay(card.into(), Message::RenameFolderCancel)
}

/// 删除目录确认对话框：目录整体软删除（改名 `.deleted`），内含建模一并隐藏。
fn view_delete_folder_confirm_dialog(state: &UiState) -> Element<'_, Message> {
    let name = state
        .dag_editor
        .delete_folder_target_name
        .clone()
        .unwrap_or_default();
    let target_id = state
        .dag_editor
        .delete_folder_target_id
        .clone()
        .unwrap_or_default();
    let model_count = state
        .dag_editor
        .folders
        .iter()
        .find(|f| f.id == target_id)
        .map(|f| f.model_count)
        .unwrap_or(0);

    let icon = container(text("!").color(theme::danger()).size(22.0))
        .width(Length::Fixed(44.0))
        .height(Length::Fixed(44.0))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 248.0 / 255.0, g: 113.0 / 255.0, b: 113.0 / 255.0, a: 15.0 / 255.0,
            }.into());
            s.border.radius = 12.0.into();
            s
        });
    let detail = if model_count == 0 {
        format!("确定删除空目录「{}」吗？可手动恢复（.deleted）。", name)
    } else {
        format!(
            "确定删除目录「{}」吗？其中 {} 个建模将一并从列表移除（软删除 .deleted，可手动恢复）。",
            name, model_count
        )
    };
    let title_col = column![
        text("删除目录").color(theme::danger()).size(15.0),
        text(detail).color(theme::text_hover()).size(10.5),
    ]
    .spacing(2);

    let confirm_btn = dialog_button("确认删除", Message::DeleteFolderConfirm, true);
    let cancel_btn = dialog_button("取消", Message::DeleteFolderCancel, false);
    let btns = row![row![].width(Length::Fill), cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let card = Card::new(
        row![icon, title_col].spacing(12).align_y(Alignment::Center).width(Length::Fill),
        text(""),
    )
    .foot(btns)
    .style(theme::float_card_style())
    .padding_head(Padding { top: 20.0, bottom: 0.0, left: 20.0, right: 20.0 })
    .padding_body(Padding { top: 8.0, bottom: 8.0, left: 20.0, right: 20.0 })
    .padding_foot(Padding { top: 0.0, bottom: 18.0, left: 20.0, right: 20.0 })
    .width(Length::Fixed(380.0));

    dialog_overlay(card.into(), Message::DeleteFolderCancel)
}

/// 「移动到目录」对话框：列出根目录 + 所有子目录，点击任一目录即把目标建模移动过去。
fn view_move_model_dialog(state: &UiState) -> Element<'_, Message> {
    let model_name = state
        .dag_editor
        .move_model_target_name
        .clone()
        .unwrap_or_default();

    let icon = dialog_icon_block(icons::view_icon(IconKind::Move, theme::accent(), 22.0));
    let title_col = column![
        text("移动到目录").color(theme::text_strong()).size(15.0),
        text(format!("将建模「{}」移动到：", model_name))
            .color(theme::text_weak())
            .size(10.5),
    ]
    .spacing(2);

    // 目录列表：根目录在前，随后递归所有子目录（按字典序）
    let mut items: Vec<Element<'static, Message>> = Vec::new();

    // 根目录项
    items.push(folder_pick_item("根目录".to_string(), String::new(), 0));

    let all_folders = dag_store::list_all_folders();
    for f in &all_folders {
        let depth = f.id.matches('/').count();
        let display = f.id.replace('/', " / ");
        items.push(folder_pick_item(display, f.id.clone(), depth));
    }

    let list_col = if items.is_empty() {
        column![
            container(text("（没有可选目录）").color(theme::text_weak()).size(11.0))
                .width(Length::Fill)
                .align_x(Alignment::Center)
                .padding(Padding { top: 16.0, bottom: 16.0, left: 0.0, right: 0.0 })
        ]
        .spacing(4)
    } else {
        let mut c = column![].spacing(4);
        for it in items {
            c = c.push(it);
        }
        c
    };

    let list_scroll = scrollable(list_col)
        .direction(scrollable::Direction::Vertical(theme::cool_scrollbar()))
        .height(Length::Fixed(220.0))
        .style(theme::cool_scrollbar_style());

    let cancel_btn = dialog_button("取消", Message::MoveModelCancel, false);
    let btns = row![row![].width(Length::Fill), cancel_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let card = Card::new(
        row![icon, title_col].spacing(12).align_y(Alignment::Center).width(Length::Fill),
        list_scroll,
    )
    .foot(btns)
    .style(theme::float_card_style())
    .padding_head(Padding { top: 20.0, bottom: 0.0, left: 20.0, right: 20.0 })
    .padding_body(Padding { top: 16.0, bottom: 12.0, left: 20.0, right: 20.0 })
    .padding_foot(Padding { top: 0.0, bottom: 18.0, left: 20.0, right: 20.0 })
    .width(Length::Fixed(380.0));

    dialog_overlay(card.into(), Message::MoveModelCancel)
}

/// 移动对话框中的单个目录选项：文件夹图标 + 路径名，点击即移动到该目录。
fn folder_pick_item(label: String, folder_id: String, depth: usize) -> Element<'static, Message> {
    let indent = 10.0 + (depth as f32) * 14.0;
    let icon = icons::view_icon(IconKind::Folder, theme::accent(), 14.0);
    let content = row![icon, text(label).color(theme::text_strong()).size(11.5)]
        .spacing(8)
        .align_y(Alignment::Center);
    let btn = button(content)
        .width(Length::Fill)
        .padding(Padding { top: 7.0, bottom: 7.0, left: indent, right: 10.0 })
        .on_press(Message::MoveModelToFolder(folder_id))
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.border.radius = theme::WIDGET_ROUNDING.into();
            s.background = Some(Color::TRANSPARENT.into());
            s.text_color = theme::text_strong();
            if matches!(status, iced::widget::button::Status::Hovered) {
                s.background = Some(Color::from(theme::card_hover_bg()).into());
                s.border.color = theme::accent_dim();
            }
            s
        });
    btn.into()
}

/// 通用对话框遮罩层 v2：靛蓝黑 + 居中卡片
fn dialog_overlay(card: Element<'_, Message>, cancel: Message) -> Element<'_, Message> {
    let centered = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 6.0/255.0, g: 7.0/255.0, b: 9.0/255.0, a: 0.72
            }.into());
            s
        });
    mouse_area(centered).on_press(cancel).into()
}

/// 对话框按钮 v2：主按钮靛蓝渐变，次按钮灰边胶囊
fn dialog_button(label: &str, msg: Message, primary: bool) -> Element<'_, Message> {
    let label_widget = container(
        text(label).size(11.5)
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center);
    button(label_widget)
        .height(Length::Fixed(32.0))
        .padding(Padding { top: 0.0, bottom: 0.0, left: 16.0, right: 16.0 })
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.border.radius = theme::WIDGET_ROUNDING.into();
            if primary {
                s.background = Some(Color::from(theme::accent()).into());
                s.text_color = Color::WHITE;
                s.border.width = 1.0;
                s.border.color = Color::from(theme::accent_bright());
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::accent_bright()).into());
                } else if matches!(status, iced::widget::button::Status::Pressed) {
                    s.background = Some(Color::from(theme::accent_dark()).into());
                }
            } else {
                s.background = Some(Color::TRANSPARENT.into());
                s.text_color = theme::text_hover();
                s.border.width = 1.0;
                s.border.color = theme::card_stroke();
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::hover_bg()).into());
                    s.text_color = theme::text_strong();
                }
            }
            s
        })
        .on_press(msg)
        .into()
}

// ===== 后台任务轮询（保持原有逻辑） =====

pub fn poll_dag_exec_task(editor_state: &mut DagEditorState) {
    let finished = poll_exec_task_messages(editor_state);
    if finished {
        editor_state.dag_exec_task = None;
    }
}

/// 检查激活 tab 的 `pending_run_all` / `pending_run_up_to` 标志，若为真则
/// 起工作线程调用流式执行 API，把 `DagExecTask` 挂到 `editor_state.dag_exec_task`。
///
/// 必须在 `poll_dag_exec_task` 之前调用（避免同一帧内既消费又挂载）。
pub fn try_spawn_pending_dag_exec(editor_state: &mut DagEditorState) {
    let active_idx = match editor_state.active_tab_index {
        Some(i) => i,
        None => return,
    };

    // 快照 pending 标志（只读借用）
    let (pending_all, pending_upto) = {
        let tab = &editor_state.tabs[active_idx];
        (tab.pending_run_all, tab.pending_run_up_to.clone())
    };
    if !pending_all && pending_upto.is_none() {
        return;
    }

    // 预检：图非空
    if editor_state.tabs[active_idx].graph.nodes.is_empty() {
        let tab = &mut editor_state.tabs[active_idx];
        tab.pending_run_all = false;
        tab.pending_run_up_to = None;
        tab.add_action_log("DAG 为空，无节点可执行".to_string(), LogLevel::Error);
        return;
    }

    // 预检：无正在运行的任务
    if editor_state.dag_exec_task.is_some() {
        let tab = &mut editor_state.tabs[active_idx];
        tab.pending_run_all = false;
        tab.pending_run_up_to = None;
        tab.add_action_log(
            "已有执行任务在进行中，请等待完成".to_string(),
            LogLevel::Warning,
        );
        return;
    }

    // 收集 spawn 所需全部 owned 数据（一次性可变借用）
    let (kind, graph, dag_name, target_node_id, debug_session_id, model_id) = {
        let tab = &mut editor_state.tabs[active_idx];
        tab.pending_run_all = false;
        tab.pending_run_up_to = None;

        let kind = if let Some(tid) = pending_upto.clone() {
            DagExecKind::RunUpTo { target_node_id: tid }
        } else {
            DagExecKind::RunAll
        };

        // 重新执行前清除上一轮的成功/失败标识，让画布状态回到"未执行"，
        // 否则上一轮的绿色对勾会让人误以为本轮也已成功。
        match &kind {
            DagExecKind::RunAll => {
                tab.io_registry.reset_all();
            }
            DagExecKind::RunUpTo { target_node_id: tid } => {
                // 与服务端实际执行的子图范围保持一致（目标节点 + 全部上游）
                if let Ok(ids) = tab.graph.get_ancestors(tid) {
                    tab.io_registry.reset_nodes(ids.iter().map(String::as_str));
                }
            }
        }

        // Debug 模式：生成会话 ID 下发到服务端，保留各节点完整输出供分页查询
        let debug_session_id = if tab.debug_mode {
            Some(uuid::Uuid::new_v4().to_string())
        } else {
            None
        };
        if let Some(sid) = &debug_session_id {
            tab.debug_session_id = Some(sid.clone());
        }

        let dag_name = match &kind {
            DagExecKind::RunAll => format!("runall_{}", tab.model_id),
            DagExecKind::RunUpTo { target_node_id } => {
                format!("upto_{}", target_node_id)
            }
        };

        (
            kind,
            tab.graph.clone(),
            dag_name,
            pending_upto,
            debug_session_id,
            tab.model_id.clone(),
        )
    };

    // 起工作线程：clone graph → 调用流式执行 → mpsc 推送进度/chunk/结果
    let (tx, rx) = std::sync::mpsc::channel::<DagExecMessage>();
    let graph_for_thread = graph.clone();
    let dag_name_for_thread = dag_name.clone();
    let debug_sid_for_thread = debug_session_id.clone();
    let target_for_thread = target_node_id.clone();

    std::thread::spawn(move || {
        let tx_progress = tx.clone();
        let tx_chunk = tx.clone();

        let result = if let Some(tid) = &target_for_thread {
            // 「运行到此结点」：执行目标节点上游子图
            execute_dag_up_to_detached_streaming_debug(
                &graph_for_thread,
                tid,
                debug_sid_for_thread.as_deref(),
                |nr| {
                    let _ = tx_progress.send(DagExecMessage::NodeProgress(nr.clone()));
                },
                |node_id, chunk| {
                    let _ = tx_chunk.send(DagExecMessage::StreamChunk {
                        node_id: node_id.to_string(),
                        chunk: chunk.clone(),
                    });
                },
            )
        } else {
            // 「执行 DAG」：执行整张图
            execute_dag_on_server_streaming_debug(
                &graph_for_thread,
                &dag_name_for_thread,
                debug_sid_for_thread.as_deref(),
                |nr| {
                    let _ = tx_progress.send(DagExecMessage::NodeProgress(nr.clone()));
                },
                |node_id, chunk| {
                    let _ = tx_chunk.send(DagExecMessage::StreamChunk {
                        node_id: node_id.to_string(),
                        chunk: chunk.clone(),
                    });
                },
            )
        };

        let _ = tx.send(DagExecMessage::Finished(result));
    });

    editor_state.dag_exec_task = Some(DagExecTask {
        kind,
        rx,
        model_id,
    });

    // 起线程成功后写一条提醒日志（用 target_node_id 判断类型，避免再借 dag_exec_task）
    let msg = if target_node_id.is_some() {
        "已开始执行上游子图".to_string()
    } else {
        "已开始执行 DAG".to_string()
    };
    if let Some(tab) = editor_state.active_tab_mut() {
        tab.add_action_log(msg, LogLevel::Info);
    }
}

/// 流式 chunk 在日志中的最大展示字符数（超出截断并标注总长度）。
const STREAM_CHUNK_LOG_MAX_CHARS: usize = 200;

/// 把流式 chunk 格式化成单行日志摘要。
///
/// - String：转义换行/制表符等控制字符后按字符截断，避免长文本
///   （chat DSL 快照、长行文本等）一条日志刷掉整个面板；
/// - Float/Int/Bool：直接显示值；
/// - DataFrame/DataFrameArray：显示行 × 列 / 帧数摘要，不展开数据。
fn format_stream_chunk_preview(chunk: &PortData) -> String {
    match chunk {
        PortData::String(s) => {
            // 先转义反斜杠，再转义控制字符，保证日志始终单行
            let escaped = s
                .replace('\\', r"\\")
                .replace('\r', r"\r")
                .replace('\n', r"\n")
                .replace('\t', r"\t");
            let total = escaped.chars().count();
            if total == 0 {
                "String(空)".to_string()
            } else if total <= STREAM_CHUNK_LOG_MAX_CHARS {
                format!("\"{}\"", escaped)
            } else {
                let head: String = escaped.chars().take(STREAM_CHUNK_LOG_MAX_CHARS).collect();
                format!("\"{}\"…(共 {} 字符)", head, total)
            }
        }
        PortData::Float(v) => format!("Float({})", v),
        PortData::Int(v) => format!("Int({})", v),
        PortData::Bool(v) => format!("Bool({})", v),
        PortData::DataFrame(df) => {
            format!("DataFrame({} 行 × {} 列)", df.row_count, df.columns.len())
        }
        PortData::DataFrameArray(dfs) => {
            let rows: usize = dfs.iter().map(|df| df.row_count).sum();
            format!("DataFrameArray({} 帧, 共 {} 行)", dfs.len(), rows)
        }
    }
}

fn poll_exec_task_messages(editor_state: &mut DagEditorState) -> bool {
    let task = match editor_state.dag_exec_task.take() {
        Some(t) => t,
        None => return false,
    };
    let mut finished = false;

    // 任务可能跨 tab 完成（用户执行期间切换 tab），按 model_id 定位归属 tab
    let tab_idx = editor_state.find_tab_by_model(&task.model_id);

    while let Ok(msg) = task.rx.try_recv() {
        match msg {
            DagExecMessage::Log(text, level) => {
                if let Some(i) = tab_idx {
                    editor_state.tabs[i].add_runtime_log(text, level);
                }
            }
            DagExecMessage::NodeProgress(nr) => {
                if let Some(i) = tab_idx {
                    // 进度日志：节点名 + 状态
                    let display_name = editor_state.tabs[i]
                        .graph
                        .get_node(&nr.node_id)
                        .map(|n| n.operator_type.name())
                        .unwrap_or(&nr.operator_name)
                        .to_string();
                    let level = match nr.execution_result.status {
                        OperatorExecutionStatus::Completed => LogLevel::Success,
                        OperatorExecutionStatus::Failed => LogLevel::Error,
                        _ => LogLevel::Info,
                    };
                    editor_state.tabs[i].add_runtime_log(
                        format!(
                            "节点 {} ({}) → {}",
                            nr.node_id,
                            display_name,
                            nr.execution_result.status.to_str()
                        ),
                        level,
                    );
                    // 回填 registry：需 graph + registry 同时引用，clone graph
                    // 规避同一 struct 上的可变/不可变借用冲突
                    let graph_clone = editor_state.tabs[i].graph.clone();
                    if let Err(e) = apply_dag_node_result(
                        &graph_clone,
                        &nr,
                        &mut editor_state.tabs[i].io_registry,
                    ) {
                        editor_state.tabs[i].add_runtime_log(e, LogLevel::Error);
                    }
                }
            }
            DagExecMessage::StreamChunk { node_id, chunk } => {
                // 流式 chunk（chat DSL 等）的实时预览留待 chat_preview 窗口接入；
                // 日志中先打印 chunk 内容摘要（字符串转义截断为单行），便于排查
                if let Some(i) = tab_idx {
                    editor_state.tabs[i].add_runtime_log(
                        format!(
                            "节点 {} 流式 chunk: {}",
                            node_id,
                            format_stream_chunk_preview(&chunk)
                        ),
                        LogLevel::Info,
                    );
                }
            }
            DagExecMessage::Finished(res) => {
                finished = true;
                if let Some(i) = tab_idx {
                    match res {
                        Ok(result) => {
                            let graph_clone = editor_state.tabs[i].graph.clone();
                            let total = result.node_results.len();
                            let ok_count = result
                                .node_results
                                .iter()
                                .filter(|nr| {
                                    matches!(
                                        nr.execution_result.status,
                                        OperatorExecutionStatus::Completed
                                    )
                                })
                                .count();
                            match apply_dag_execution_result(
                                &graph_clone,
                                &result,
                                &mut editor_state.tabs[i].io_registry,
                            ) {
                                Ok(()) => {
                                    editor_state.tabs[i].add_runtime_log(
                                        format!(
                                            "DAG 执行完成（{}/{} 节点成功）",
                                            ok_count, total
                                        ),
                                        LogLevel::Success,
                                    );
                                }
                                Err(e) => {
                                    editor_state.tabs[i].add_runtime_log(
                                        format!("DAG 执行完成但回填出错: {}", e),
                                        LogLevel::Error,
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            editor_state.tabs[i].add_runtime_log(
                                format!("DAG 执行失败: {}", e),
                                LogLevel::Error,
                            );
                        }
                    }
                }
                break;
            }
        }
    }

    if !finished {
        editor_state.dag_exec_task = Some(task);
    }
    finished
}

pub fn release_all_debug_sessions(editor_state: &mut DagEditorState) {
    for tab in &mut editor_state.tabs {
        tab.debug_session_id = None;
        tab.debug_preview = None;
    }
}
