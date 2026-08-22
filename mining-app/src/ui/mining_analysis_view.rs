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
use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::widget::mouse_area;
use iced::{
    Alignment, Color, Element, Length, Padding,
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
use crate::dag_store;
use crate::operator_executor::{
    apply_dag_execution_result, apply_dag_node_result,
    execute_dag_on_server_streaming_debug,
    execute_dag_up_to_detached_streaming_debug,
};
use operator_executor_client::protocol::OperatorExecutionStatus;

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
    } else if state.dag_editor.rename_target_id.is_some() {
        Some(view_rename_dialog(state))
    } else if state.dag_editor.show_delete_model_dialog {
        Some(view_delete_confirm_dialog(state))
    } else {
        None
    };

    // 画布/节点右键菜单（顶层最上，点击遮罩关闭）
    let ctx_layer = view_context_menu_if_any(state);

    let mut layers = vec![base_layer];
    if let Some(dlg) = dialog_layer {
        layers.push(dlg);
    }
    if let Some(ctx) = ctx_layer {
        layers.push(ctx);
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

/// 左侧面板「建模列表」子页 v2：精美卡片式列表。
fn view_models_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;
    let active_model_id: Option<&str> = editor
        .active_tab()
        .map(|t| t.model_id.as_str());

    // 头部：标题 + 计数徽章（iced_aw::Badge 替换手搓容器）
    let count_badge = Badge::<Message>::new(
        text(format!("{}", editor.models.len()))
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

    // 建模卡片列表
    let mut list_col = column![].spacing(6).padding(Padding {
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
    } else if editor.models.is_empty() {
        // 空状态：精美的占位卡片
        let empty_card = container(
            column![
                text("◇").color(theme::accent_dim()).size(32.0),
                text("暂无建模").color(theme::text_strong()).size(12.0),
                text("点击下方按钮创建第一个建模").color(theme::text_weak()).size(10.0),
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
        for m in &editor.models {
            let is_active = active_model_id == Some(m.id.as_str());
            list_col = list_col.push(view_model_card(m, is_active));
        }
    }

    // 新建模按钮：主色胶囊
    let inner_content = container(
        row![
            text("＋").color(Color::WHITE).size(14.0),
            text("新建建模").color(Color::WHITE).size(12.0),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center);

    let new_btn = button(inner_content)
        .width(Length::Fill)
        .height(Length::Fixed(32.0))
        .on_press(Message::NewModelClick)
        .padding(Padding { top: 0.0, bottom: 0.0, left: 12.0, right: 12.0 })
    .style(|_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.background = Some(Color::from(theme::accent()).into());
        s.text_color = Color::WHITE;
        s.border.radius = 10.0.into();
        if matches!(status, iced::widget::button::Status::Hovered) {
            s.background = Some(Color::from(theme::accent_bright()).into());
        } else if matches!(status, iced::widget::button::Status::Pressed) {
            s.background = Some(Color::from(theme::accent_dark()).into());
        }
        s
    });

    let body_scroll = scrollable(list_col)
        .width(Length::Fill)
        .height(Length::Fill);

    let body = column![
        header,
        header_divider,
        body_scroll,
        container(new_btn)
            .width(Length::Fill)
            .padding(Padding { top: 4.0, bottom: 6.0, left: 10.0, right: 10.0 }),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .spacing(0);

    container(body).width(Length::Fill).height(Length::Fill).into()
}

/// 建模卡片 v2：图标块 + 名称时间 + 操作按钮，卡片式设计。
fn view_model_card(m: &dag_store::DagModelMeta, is_active: bool) -> Element<'_, Message> {
    let name_color = if is_active { Color::WHITE } else { theme::text_strong() };

    // 左侧图标块：根据激活态改变颜色
    let icon_color = if is_active { Color::WHITE } else { theme::accent() };
    let icon_bg = if is_active {
        Color::from(theme::accent())
    } else {
        Color {
            r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 15.0/255.0
        }
    };
    let icon_block = container(
        text("◆").color(icon_color).size(15.0)
    )
    .width(Length::Fixed(34.0))
    .height(Length::Fixed(34.0))
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .style(move |_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(icon_bg.into());
        s.border.radius = 9.0.into();
        s
    });

    // 名称 + 时间
    let info_col = column![
        text(m.name.clone()).color(name_color).size(12.0),
        text(dag_store::format_timestamp(m.updated_at))
            .color(if is_active { Color { r:1.0,g:1.0,b:1.0,a:0.7 } } else { theme::text_weak() })
            .size(9.5),
    ]
    .spacing(2)
    .width(Length::Fill);

    // 操作按钮：编辑（铅笔）+ 删除（垃圾桶，红色警示），矢量图标同尺寸协调
    let rename_btn = card_icon_button_kind(
        IconKind::Pencil,
        Message::RenameModelClick(m.id.clone()),
        is_active,
        None,
    );
    let delete_btn = card_icon_button_kind(
        IconKind::Trash,
        Message::DeleteModelClick(m.id.clone(), m.name.clone()),
        is_active,
        Some(theme::danger()),
    );
    let actions = row![rename_btn, delete_btn].spacing(4);

    let mid = button(
        row![icon_block, info_col, actions]
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
            // 激活态：靛蓝渐变 + 边框发光
            s.background = Some(Color {
                r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 95.0/255.0
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

/// 建模列表卡片操作按钮（矢量图标版）：32×32 透明底，hover 微亮背景，
/// 用 `icons::view_icon_with_stroke` 矢量图标统一风格。编辑用铅笔、删除用垃圾桶。
///
/// `tone` 传入 `Some(color)` 时，图标常态即用该语义色（删除按钮传 danger 红）；
/// `None` 时按激活/非激活自动取近白/弱灰，hover 提亮到近白。
fn card_icon_button_kind(
    icon_kind: IconKind,
    msg: Message,
    is_active: bool,
    tone: Option<Color>,
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
    let icon = icons::view_icon_with_stroke(icon_kind, normal_color, 16.0, 1.5);
    let icon_widget = container(icon)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
    button(icon_widget)
        .width(Length::Fixed(32.0))
        .height(Length::Fixed(32.0))
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(Color::TRANSPARENT.into());
            s.text_color = hover_color;
            s.border.radius = 7.0.into();
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
                r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 30.0/255.0,
            }.into());
            match status {
                iced::widget::button::Status::Hovered => {
                    s.background = Some(Color {
                        r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 50.0/255.0,
                    }.into());
                }
                iced::widget::button::Status::Pressed => {
                    s.background = Some(Color {
                        r: 79.0/255.0, g: 70.0/255.0, b: 229.0/255.0, a: 40.0/255.0,
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
            Color { r: 235.0/255.0, g: 238.0/255.0, b: 250.0/255.0, a: 0.65 }
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
                // 大图标：蓝紫渐变发光装饰
                container(
                    text("◇").color(Color {
                        r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 180.0/255.0
                    }).size(54.0)
                )
                .width(Length::Fixed(88.0))
                .height(Length::Fixed(88.0))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(|_t| {
                    let mut s = iced::widget::container::Style::default();
                    s.background = Some(Color {
                        r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 10.0/255.0
                    }.into());
                    s.border.radius = 24.0.into();
                    s.border.width = 1.0;
                    s.border.color = Color {
                        r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 35.0/255.0
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
                r: 24.0/255.0, g: 30.0/255.0, b: 48.0/255.0, a: 180.0/255.0
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

        // 画布上叠加悬浮工具栏（顶部居中，向下偏移 12px）
        let stacked = iced::widget::Stack::with_children(vec![
            canvas,
            container(toolbar)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Start)
                .padding(Padding { top: 12.0, bottom: 0.0, left: 0.0, right: 0.0 })
                .into(),
        ])
        .width(Length::Fill)
        .height(Length::Fill);

        stacked.into()
    }
}

/// 画布悬浮工具栏：顶部居中胶囊条，纯图标按钮（保存 / 执行 DAG / 调试）。
///
/// 设计要点：
/// - 半透明深色底 + 1px 微光边框 + 大圆角，漂浮于画布之上不遮挡节点
/// - 图标来自 `icons::view_icon`（IconKind::Save / Run / Debug），零外部资源依赖
/// - 次按钮透明底 + hover 提亮；主按钮 accent 实色填充；激活态（调试开）描边高亮
fn view_floating_toolbar(state: &UiState) -> Element<'_, Message> {
    let debug_on = state
        .dag_editor
        .active_tab()
        .map(|t| t.debug_mode)
        .unwrap_or(false);

    let tools = row![
        icon_only_tool_button(IconKind::Save, Message::SaveTab, false),
        icon_only_tool_button(IconKind::Run, Message::RunAllClick, true),
        icon_only_tool_button(IconKind::Debug, Message::ToggleDebug, debug_on),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    container(tools)
        .padding(Padding { top: 4.0, bottom: 4.0, left: 6.0, right: 6.0 })
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color {
                r: 13.0/255.0, g: 17.0/255.0, b: 28.0/255.0, a: 200.0/255.0
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
        // 主按钮：靛蓝实色底 → 图标用 WHITE 保证对比度
        Color::WHITE
    } else if primary_or_active {
        // 激活态（调试开）：半透明靛蓝底 → 亮靛蓝图标
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
                // 主按钮（▶ 执行 DAG）：靛蓝实色 + 亮边框高光；调试激活态：半透明底 + accent 描边
                let is_primary = matches!(icon, IconKind::Run);
                if is_primary {
                    s.background = Some(Color::from(theme::accent()).into());
                    s.text_color = Color::WHITE;
                    s.border.width = 1.0;
                    s.border.color = Color {
                        r: 165.0/255.0, g: 180.0/255.0, b: 252.0/255.0, a: 1.0
                    };
                    if matches!(status, iced::widget::button::Status::Hovered) {
                        s.background = Some(Color::from(theme::accent_bright()).into());
                    } else if matches!(status, iced::widget::button::Status::Pressed) {
                        s.background = Some(Color::from(theme::accent_dark()).into());
                    }
                } else {
                    s.background = Some(Color {
                        r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 30.0/255.0
                    }.into());
                    s.text_color = theme::accent_bright();
                    s.border.width = 1.0;
                    s.border.color = Color {
                        r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 180.0/255.0
                    };
                    if matches!(status, iced::widget::button::Status::Hovered) {
                        s.background = Some(Color {
                            r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 50.0/255.0
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

/// 左侧面板「算子面板」子页 v2：上方算子目录 + 下方节点参数。
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
    let categories = crate::dag::get_operator_categories();
    let filter = search_value.trim().to_lowercase();
    let mut op_col = column![].spacing(5).padding(Padding {
        top: 2.0, bottom: 8.0, left: 8.0, right: 8.0,
    });
    render_operator_categories(&categories, &filter, 0, &mut op_col);
    let op_scroll = scrollable(op_col)
        .width(Length::Fill)
        .height(Length::Fill);

    let op_col_top = column![search_container, op_scroll]
        .width(Length::Fill)
        .height(Length::FillPortion(3))
        .spacing(0);

    // 分隔条
    let divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::divider()).into());
            s
        });

    // 节点参数面板标题
    let params_header = container(
        row![
            text("节点参数").color(theme::text_strong()).size(12.0),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(Padding { top: 6.0, bottom: 2.0, left: 12.0, right: 12.0 });

    let params_body = if let Some(tab) = editor.active_tab() {
        view_params_body(tab)
    } else {
        container(
            column![
                text("◇").color(theme::accent_dim()).size(26.0),
                text("未打开建模").color(theme::text_weak()).size(10.5),
            ]
            .spacing(4)
            .align_x(Alignment::Center)
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
    };

    let params = container(column![params_header, params_body].spacing(0))
        .width(Length::Fill)
        .height(Length::FillPortion(2))
        .padding(Padding { top: 0.0, bottom: 8.0, left: 2.0, right: 2.0 });

    let col = column![header, header_divider, op_col_top, divider, params]
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

/// 参数面板 body：未选中节点 → 占位；选中节点 → 参数名+类型+text_input 列表。
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
    let param_defs = node.operator_type.param_defs();
    if param_defs.is_empty() {
        return container(
            column![
                text(node.operator_type.name())
                    .color(theme::text_strong())
                    .size(11.0),
                text("(该算子无参数)")
                    .color(theme::text_weak())
                    .size(10.0),
            ]
            .spacing(4)
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Start)
        .align_y(Alignment::Start)
        .padding(Padding {
            top: 4.0,
            bottom: 4.0,
            left: 0.0,
            right: 0.0,
        })
        .into();
    }

    let mut col = column![
        text(node.operator_type.name())
            .color(theme::text_strong())
            .size(11.0)
    ]
    .spacing(4)
    .padding(Padding {
        top: 2.0,
        bottom: 2.0,
        left: 0.0,
        right: 0.0,
    });

    for def in param_defs {
        let current = node
            .operator_type
            .get_param_value(&def.name)
            .unwrap_or_default();
        let nid = node_id.clone();
        let pname = def.name.clone();
        let type_label = text(def.param_type.to_str())
            .color(theme::text_weak())
            .size(9.0);
        let input = text_input(&def.name, &current)
            .on_input(move |v| Message::ParamInput(nid.clone(), pname.clone(), v))
            .width(Length::Fill)
            .size(11.0)
            .padding(Padding {
                top: 3.0,
                bottom: 3.0,
                left: 5.0,
                right: 5.0,
            });
        col = col.push(column![
            row![text(def.name.as_str()).color(theme::text_strong()).size(11.0), type_label]
                .spacing(6)
                .align_y(Alignment::Center),
            input,
        ].spacing(1));
    }

    scrollable(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

// ===== 画布 / 节点右键菜单叠加（view_mining_analysis 的 dialog_layer 之后加入） =====

/// 渲染画布/节点右键菜单（Stack 最上层卡片）。
///
/// 若 `context_menu_node_id` 为 Some → 节点菜单（运行到此节点/删除节点/关闭）；
/// 否则 → 画布空白菜单（重置视图/关闭菜单）。
fn view_context_menu_if_any(state: &UiState) -> Option<Element<'_, Message>> {
    let tab = state.dag_editor.active_tab()?;
    let screen_pos = tab.context_menu_screen_pos?;

    // 菜单项内容
    let items: Vec<Element<'_, Message>> = if let Some(node_id) = &tab.context_menu_node_id {
        // 节点菜单
        let nid = node_id.clone();
        let run_btn = button(text("运行到此节点").color(theme::text_strong()).size(11.0))
            .width(Length::Fill)
            .height(Length::Fixed(24.0))
            .on_press(Message::RunUpToNode(nid.clone()))
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 8.0,
                right: 8.0,
            })
            .style(|_t, _status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.text_color = theme::text_strong();
                s
            });
        let del_btn = button(text("删除节点").color(theme::danger()).size(11.0))
            .width(Length::Fill)
            .height(Length::Fixed(24.0))
            .on_press(Message::DeleteNodeClick(nid))
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 8.0,
                right: 8.0,
            })
            .style(|_t, _status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.text_color = theme::text_strong();
                s
            });
        let close_btn = button(text("关闭菜单").color(theme::text_weak()).size(11.0))
            .width(Length::Fill)
            .height(Length::Fixed(24.0))
            .on_press(Message::ContextMenuClose)
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 8.0,
                right: 8.0,
            })
            .style(|_t, _status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.text_color = theme::text_strong();
                s
            });
        vec![run_btn.into(), del_btn.into(), close_btn.into()]
    } else {
        // 画布空白菜单
        let close_btn = button(text("关闭菜单").color(theme::text_weak()).size(11.0))
            .width(Length::Fill)
            .height(Length::Fixed(24.0))
            .on_press(Message::ContextMenuClose)
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 8.0,
                right: 8.0,
            })
            .style(|_t, _status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.text_color = theme::text_strong();
                s
            });
        vec![close_btn.into()]
    };

    // 菜单卡片：宽 140px，位置用 stack 子元素绝对定位不可行；这里用半透明全屏遮罩（点击遮罩关闭菜单）
    // + 卡片容器 padding 模拟绝对位置（通过大 padding 把菜单顶到屏幕坐标）。
    let pad_top = screen_pos.y.max(0.0);
    let pad_left = screen_pos.x.max(0.0);

    let items_col = column(items).spacing(0);

    let card = container(items_col)
        .width(Length::Fixed(140.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s
        });

    // 全屏遮罩：点击关闭菜单
    let mask = mouse_area(container(text("")).width(Length::Fill).height(Length::Fill))
        .on_press(Message::ContextMenuClose);

    let content = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding {
            top: pad_top,
            bottom: 0.0,
            left: pad_left,
            right: 0.0,
        })
        .align_x(Alignment::Start)
        .align_y(Alignment::Start);

    let stacked = Stack::with_children(vec![mask.into(), content.into()])
        .width(Length::Fill)
        .height(Length::Fill);
    Some(stacked.into())
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
                r: 99.0/255.0, g: 102.0/255.0, b: 241.0/255.0, a: 15.0/255.0
            }.into());
            s.border.radius = 12.0.into();
            s
        });
    let title_col = column![
        text("新建建模").color(theme::text_strong()).size(15.0),
        text("为新的建模起一个名字").color(theme::text_weak()).size(10.5),
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
                r: 5.0/255.0, g: 8.0/255.0, b: 18.0/255.0, a: 0.72
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
            DagExecMessage::StreamChunk { node_id, chunk: _ } => {
                // 流式 chunk（chat DSL 等）的实时预览留待 chat_preview 窗口接入；
                // 暂记一条运行日志便于排查
                if let Some(i) = tab_idx {
                    editor_state.tabs[i].add_runtime_log(
                        format!("节点 {} 流式 chunk 接收", node_id),
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
