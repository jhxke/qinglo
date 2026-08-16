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

use super::state::{
    DagEditorState, DagExecKind, DagExecMessage, DagExecTask, DagTab,
    JsonDirection, LeftPanelTab, LogCategory, LogLevel, Message, UiState,
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
/// 顶部 Tab 栏 + 工具栏的高度。
const TOP_BAR_HEIGHT: f32 = 36.0;
/// 底部日志面板高度。
const LOG_PANEL_HEIGHT: f32 = 160.0;
/// 日志面板最多渲染条数（避免千条日志拖垮渲染）。
const LOG_RENDER_LIMIT: usize = 200;
/// 对话框卡片宽度。
const DIALOG_WIDTH: f32 = 320.0;

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
/// 激活态：文字强白 + 底部 2px accent 下划线 + 稍亮底色；
/// 非激活：弱化文字 + 透明底，hover 时弱底色。点击切换 `active_left_panel`。
fn view_left_panel_tabs(active: LeftPanelTab) -> Element<'static, Message> {
    let mk_tab = |label: &'static str, tab: LeftPanelTab, is_active: bool| -> Element<'static, Message> {
        let txt_color = if is_active { theme::TEXT_STRONG } else { theme::TEXT_WEAK };
        let base_bg = if is_active {
            Color::from(theme::HOVER_BG)
        } else {
            Color::TRANSPARENT
        };
        let name_btn = button(text(label).color(txt_color).size(11.0))
            .height(Length::Fill)
            .on_press(Message::SwitchLeftPanel(tab))
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 10.0,
                right: 10.0,
            })
            .style(move |_t, status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(base_bg.into());
                s.text_color = txt_color;
                if !is_active && matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color { r: 1.0, g: 1.0, b: 1.0, a: 10.0 / 255.0 }.into());
                    s.text_color = theme::TEXT_HOVER;
                }
                s
            });
        let tab_row = row![name_btn]
            .align_y(Alignment::Center)
            .height(Length::Fill);
        if is_active {
            let underline = container(row![])
                .width(Length::Fill)
                .height(Length::Fixed(2.0))
                .style(|_t| {
                    let mut s = iced::widget::container::Style::default();
                    s.background = Some(Color::from(theme::ACCENT).into());
                    s
                });
            column![tab_row, underline]
                .height(Length::Fill)
                .spacing(0)
                .into()
        } else {
            tab_row.into()
        }
    };

    let models_tab = mk_tab("建模列表", LeftPanelTab::Models, active == LeftPanelTab::Models);
    let ops_tab = mk_tab("算子面板", LeftPanelTab::Operators, active == LeftPanelTab::Operators);

    let bar = row![models_tab, ops_tab]
        .width(Length::Fill)
        .height(Length::Fixed(30.0))
        .align_y(Alignment::Center)
        .spacing(0);

    container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(30.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::SIDEBAR_BG).into());
            s
        })
        .into()
}

/// 左侧面板「建模列表」子页：标题 + 列表 + 新建模按钮。
fn view_models_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;

    // 当前激活 tab 对应的 model_id，用于在列表中高亮选中态
    let active_model_id: Option<&str> = editor
        .active_tab()
        .map(|t| t.model_id.as_str());

    // 头部：左对齐标题
    let header = container(
        row![
            text("建模列表").color(theme::TEXT_STRONG).size(12.0),
            text(format!("({})", editor.models.len()))
                .color(theme::TEXT_WEAK)
                .size(10.0),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(30.0))
    .style(|_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(Color::from(theme::SIDEBAR_BG).into());
        s
    })
    .align_y(Alignment::Center)
    .padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 12.0,
        right: 12.0,
    });

    // 头部下方 1px 分隔线（iced 0.14 Border 不支持 per-side，用独立 container 实现）
    let header_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::DIVIDER).into());
            s
        });

    let mut list_col = column![].spacing(0).padding(Padding {
        top: 4.0,
        bottom: 4.0,
        left: 0.0,
        right: 0.0,
    });

    if !editor.models_loaded {
        list_col = list_col.push(
            container(
                text("(加载中…)").color(theme::TEXT_WEAK).size(11.0),
            )
            .width(Length::Fill)
            .padding(Padding {
                top: 8.0,
                bottom: 8.0,
                left: 12.0,
                right: 12.0,
            }),
        );
    } else if editor.models.is_empty() {
        list_col = list_col.push(
            container(
                text("(空，点击「+ 新建模」)").color(theme::TEXT_WEAK).size(11.0),
            )
            .width(Length::Fill)
            .padding(Padding {
                top: 8.0,
                bottom: 8.0,
                left: 12.0,
                right: 12.0,
            }),
        );
    } else {
        for m in &editor.models {
            let is_active = active_model_id == Some(m.id.as_str());
            list_col = list_col.push(view_model_item(m, is_active));
        }
    }

    // + 新建模按钮：透明默认 + accent 图标 + hover 底色
    let new_btn = button(
        row![
            text("+").color(theme::accent()).size(13.0),
            text("新建模").color(theme::TEXT_HOVER).size(11.0),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(30.0))
    .on_press(Message::NewModelClick)
    .style(|_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.background = Some(Color::TRANSPARENT.into());
        s.text_color = theme::TEXT_HOVER;
        if matches!(status, iced::widget::button::Status::Hovered) {
            s.background = Some(Color::from(theme::HOVER_BG).into());
            s.text_color = theme::TEXT_STRONG;
        }
        s
    })
    .padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 12.0,
        right: 12.0,
    });

    let body_scroll = scrollable(list_col)
        .width(Length::Fill)
        .height(Length::Fill);

    let body = column![header, header_divider, body_scroll, new_btn]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 单个建模列表项：[名称(点击打开) | ✎重命名 | ✕删除]。
///
/// `is_active` 为 true 时（该 model 是当前激活 tab 的来源）显示左侧 accent 竖条 +
/// 更亮的文字与底色，与 VS Code 资源管理器选中态一致。
fn view_model_item(m: &dag_store::DagModelMeta, is_active: bool) -> Element<'_, Message> {
    // 主名称按钮：占满剩余宽度，hover 时底色高亮
    let name_color = if is_active { theme::TEXT_STRONG } else { theme::TEXT_HOVER };
    let base_bg = if is_active {
        Color { r: 1.0, g: 1.0, b: 1.0, a: 8.0 / 255.0 }
    } else {
        Color::TRANSPARENT
    };
    let name_btn = button(
        column![
            text(m.name.clone()).color(name_color).size(11.0),
            text(dag_store::format_timestamp(m.updated_at))
                .color(theme::TEXT_WEAK)
                .size(9.0),
        ]
        .spacing(1),
    )
    .width(Length::Fill)
    .height(Length::Fixed(44.0))
    .on_press(Message::OpenModel(m.id.clone()))
    .padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 10.0,
        right: 4.0,
    })
    .style(move |_t, status| {
        let mut s = iced::widget::button::Style::default();
        s.background = Some(base_bg.into());
        s.text_color = name_color;
        if matches!(status, iced::widget::button::Status::Hovered) {
            s.background = Some(Color::from(theme::HOVER_BG).into());
            s.text_color = theme::TEXT_STRONG;
        }
        s
    });

    let rename_btn = icon_button("✎", Message::RenameModelClick(m.id.clone()));
    let delete_btn = icon_button(
        "✕",
        Message::DeleteModelClick(m.id.clone(), m.name.clone()),
    );

    // 右侧操作图标列：仅在 hover 时通过各自 button style 显示底色
    let actions = row![rename_btn, delete_btn]
        .spacing(0)
        .height(Length::Fill);

    // 左侧 accent 竖条（仅激活态绘制）
    let bar_w = if is_active { 2.0 } else { 0.0 };
    let accent_bar = container(text("").size(1.0))
        .width(Length::Fixed(bar_w))
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::ACCENT).into());
            s
        });

    row![accent_bar, name_btn, actions]
        .width(Length::Fill)
        .height(Length::Fixed(44.0))
        .spacing(0)
        .align_y(Alignment::Center)
        .into()
}

/// 小图标按钮（重命名 / 删除等），无背景，悬停高亮。
fn icon_button(label: &'static str, msg: Message) -> Element<'static, Message> {
    button(text(label).color(theme::TEXT_WEAK).size(11.0))
        .width(Length::Fixed(24.0))
        .height(Length::Fill)
        .style(|_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(Color::TRANSPARENT.into());
            s.text_color = theme::TEXT_WEAK;
            if matches!(status, iced::widget::button::Status::Hovered) {
                s.background = Some(Color::from(theme::HOVER_BG).into());
                s.text_color = theme::TEXT_STRONG;
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
    let log_panel = view_log_panel(state);

    let col = column![top_bar, middle, log_panel]
        .width(Length::Fill)
        .height(Length::Fill);

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

// ===== 顶部 Tab 栏 + 工具栏 =====

fn view_top_bar(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;

    // 左侧：Tab 列表
    let mut tabs_row = row![].spacing(2).align_y(Alignment::Center);
    if editor.tabs.is_empty() {
        tabs_row = tabs_row.push(
            text("(未打开 DAG)").color(theme::TEXT_WEAK).size(11.0),
        );
    } else {
        for (i, tab) in editor.tabs.iter().enumerate() {
            let is_active = editor.active_tab_index == Some(i);
            tabs_row = tabs_row.push(view_tab_item(i, tab.name.clone(), is_active, tab.dirty));
        }
    }

    // 右侧：工具栏按钮
    let debug_label = match editor.active_tab().map(|t| t.debug_mode) {
        Some(true) => "调试●",
        _ => "调试○",
    };
    let tools = row![
        tool_button("保存", Message::SaveTab, false),
        tool_button("执行 DAG", Message::RunAllClick, true),
        tool_button(debug_label, Message::ToggleDebug, false),
        tool_button("清空日志", Message::ClearLogs, false),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    let bar = row![
        tabs_row.width(Length::Fill),
        tools,
    ]
    .width(Length::Fill)
    .height(Length::Fixed(TOP_BAR_HEIGHT))
    .padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 8.0,
        right: 8.0,
    })
    .align_y(Alignment::Center)
    .spacing(8);

    container(bar)
        .width(Length::Fill)
        .height(Length::Fixed(TOP_BAR_HEIGHT))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::PANEL_BG).into());
            s
        })
        .into()
}

/// 单个 Tab：[名称(点击切换) | ×关闭]。dirty 时名称后加 •。
///
/// 激活态：文字强白 + 底部 2px accent 下划线 + 稍亮底色；
/// 非激活：弱化文字 + 透明底，hover 时弱底色。
fn view_tab_item(i: usize, name: String, is_active: bool, dirty: bool) -> Element<'static, Message> {
    let txt_color = if is_active { theme::TEXT_STRONG } else { theme::TEXT_WEAK };
    let base_bg = if is_active {
        Color::from(theme::HOVER_BG)
    } else {
        Color::TRANSPARENT
    };

    let label = if dirty {
        format!("{} •", name)
    } else {
        name
    };

    let name_btn = button(text(label).color(txt_color).size(11.0))
        .height(Length::Fill)
        .on_press(Message::SwitchTab(i))
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 10.0,
            right: 4.0,
        })
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(base_bg.into());
            s.text_color = txt_color;
            if !is_active && matches!(status, iced::widget::button::Status::Hovered) {
                s.background = Some(Color { r: 1.0, g: 1.0, b: 1.0, a: 10.0 / 255.0 }.into());
                s.text_color = theme::TEXT_HOVER;
            }
            s
        });

    let close_btn = button(text("✕").color(theme::TEXT_WEAK).size(10.0))
        .height(Length::Fill)
        .on_press(Message::CloseTab(i))
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 4.0,
            right: 8.0,
        })
        .style(|_t, status| {
            let mut s = iced::widget::button::Style::default();
            s.background = Some(Color::TRANSPARENT.into());
            s.text_color = theme::TEXT_WEAK;
            if matches!(status, iced::widget::button::Status::Hovered) {
                s.background = Some(Color::from(theme::danger()).into());
                s.text_color = Color::WHITE;
            }
            s
        });

    let tab_row = row![name_btn, close_btn]
        .spacing(0)
        .align_y(Alignment::Center)
        .height(Length::Fill);

    // 激活态：底部加 2px accent 下划线（column Shrink 跟随 tab 内容宽度）
    if is_active {
        let underline = container(row![])
            .width(Length::Fill)
            .height(Length::Fixed(2.0))
            .style(|_t| {
                let mut s = iced::widget::container::Style::default();
                s.background = Some(Color::from(theme::ACCENT).into());
                s
            });
        column![tab_row, underline]
            .height(Length::Fill)
            .spacing(0)
            .into()
    } else {
        tab_row.into()
    }
}

/// 工具栏按钮：`primary=true` 时用 accent 实色背景（主操作），否则透明 + hover 底色。
fn tool_button(label: &str, msg: Message, primary: bool) -> Element<'_, Message> {
    let txt_color = if primary { Color::WHITE } else { theme::TEXT_HOVER };
    button(text(label).color(txt_color).size(11.0))
        .height(Length::Fixed(24.0))
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 10.0,
            right: 10.0,
        })
        .style(move |_t, status| {
            let mut s = iced::widget::button::Style::default();
            if primary {
                s.background = Some(Color::from(theme::accent()).into());
                s.text_color = Color::WHITE;
                s.border.radius = theme::WIDGET_ROUNDING.into();
                if matches!(status, iced::widget::button::Status::Hovered) {
                    // 主按钮 hover：稍亮（混入白色）
                    s.background = Some(
                        Color::from_rgba(0.27, 0.62, 0.98, 1.0).into(),
                    );
                }
            } else {
                s.background = Some(Color::TRANSPARENT.into());
                s.text_color = theme::TEXT_HOVER;
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::HOVER_BG).into());
                    s.text_color = theme::TEXT_STRONG;
                }
            }
            s
        })
        .on_press(msg)
        .into()
}

// ===== 中间：DAG 画布（占满主区剩余空间） =====

fn view_middle(state: &UiState) -> Element<'_, Message> {
    let canvas = super::dag_canvas::view_dag_canvas(state);

    container(canvas)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 左侧面板「算子面板」子页：上方算子列表 + 下方选中节点参数。
///
/// 顶部高度固定 30px 标题，上部 ~60% 为算子目录（分类树 + 搜索 + 卡片点击添加），
/// 下部 ~40% 为节点参数编辑面板（未选中节点时展示占位提示）。
/// 中间 1px 分隔条区分两区域。外层宽度由父级 [`view_sidebar`] 约束为 `LEFT_PANEL_WIDTH`。
fn view_operator_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;
    let search_value = editor
        .active_tab()
        .map(|t| t.operator_search_filter.clone())
        .unwrap_or_default();

    // 标题栏：左对齐 + 底部细分隔线
    let header = container(text("算子面板").color(theme::TEXT_STRONG).size(12.0))
        .width(Length::Fill)
        .height(Length::Fixed(30.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::sidebar_bg()).into());
            s
        })
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 12.0,
            right: 12.0,
        });

    let header_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::DIVIDER).into());
            s
        });

    // 搜索框：用 card_bg 容器包裹，让输入框有"卡片感"
    let search = text_input("搜索算子名…", &search_value)
        .on_input(Message::OperatorSearchInput)
        .width(Length::Fill)
        .size(11.0)
        .padding(Padding {
            top: 5.0,
            bottom: 5.0,
            left: 8.0,
            right: 8.0,
        });
    let search = container(search)
        .width(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border.color = theme::card_stroke();
            s.border.width = 1.0;
            s.border.radius = theme::WIDGET_ROUNDING.into();
            s
        });
    let search_wrap = container(search)
        .width(Length::Fill)
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 8.0,
            right: 8.0,
        });

    // 算子目录递归渲染
    let categories = crate::dag::get_operator_categories();
    let filter = search_value.trim().to_lowercase();
    let mut op_col = column![].spacing(3).padding(Padding {
        top: 2.0,
        bottom: 6.0,
        left: 6.0,
        right: 6.0,
    });
    render_operator_categories(&categories, &filter, 0, &mut op_col);
    let op_scroll = scrollable(op_col)
        .width(Length::Fill)
        .height(Length::Fill);

    // 算子面板（搜索+分类树）用 FillPortion 占相对比重，参数面板另取一份
    let op_col_top = column![search_wrap, op_scroll]
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

    // 节点参数面板
    let params_body = if let Some(tab) = editor.active_tab() {
        view_params_body(tab)
    } else {
        container(
            text("(未打开建模)")
                .color(theme::text_weak())
                .size(11.0),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
    };

    let params = container(params_body)
        .width(Length::Fill)
        .height(Length::FillPortion(2))
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 8.0,
            right: 8.0,
        });

    let col = column![header, header_divider, op_col_top, divider, params]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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

            // 卡片内容：左色条 + 算子名 + 描述（用 button 才能拿到 Hovered 状态）
            let card_btn = button(
                row![
                    // 左色条（3px 宽，全高填色）
                    container(text("").size(1.0))
                        .width(Length::Fixed(3.0))
                        .height(Length::Fill)
                        .style(move |_t| {
                            let mut s = iced::widget::container::Style::default();
                            s.background = Some(color.into());
                            s
                        }),
                    column![
                        text(name_owned)
                            .color(theme::text_strong())
                            .size(11.0),
                        text(desc_owned)
                            .color(theme::text_weak())
                            .size(9.0),
                    ]
                    .spacing(1)
                    .width(Length::Fill),
                ]
                .align_y(Alignment::Center)
                .width(Length::Fill),
            )
            .width(Length::Fill)
            .on_press(Message::AddOperator(op_name_for_msg))
            .padding(Padding {
                top: 6.0,
                bottom: 6.0,
                left: 0.0,
                right: 6.0,
            })
            .style(move |_t, status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.border.color = theme::card_stroke();
                s.border.width = 1.0;
                s.border.radius = theme::WIDGET_ROUNDING.into();
                s.text_color = theme::text_strong();
                if matches!(status, iced::widget::button::Status::Hovered) {
                    // hover：底色变亮 + 边框变 accent 弱化色
                    s.background = Some(Color::from(theme::hover_bg()).into());
                    s.border.color = theme::accent_dim();
                }
                s
            });

            // 外层 indent 容器
            let card = container(card_btn).padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: indent,
                right: 0.0,
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

// ===== 底部：日志面板 =====

fn view_log_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;
    let active = editor.active_tab();

    // 子标签栏：提醒 / 算子运行 / 通信报文
    let cat_btn = |label: &str, cat: LogCategory, current: LogCategory| -> Element<'static, Message> {
        let is_active = cat == current;
        let txt_color = if is_active { theme::TEXT_STRONG } else { theme::TEXT_WEAK };
        let bg = if is_active { theme::HOVER_BG } else { theme::PANEL_BG };
        button(text(label.to_string()).color(txt_color).size(11.0))
            .height(Length::Fixed(22.0))
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 8.0,
                right: 8.0,
            })
            .on_press(Message::SwitchLogCategory(cat))
            .style(move |_t, _status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(bg).into());
                s.text_color = txt_color;
                s
            })
            .into()
    };

    let current_cat = active.map(|t| t.active_log_category).unwrap_or_default();
    let tabs = row![
        cat_btn("提醒", LogCategory::Action, current_cat),
        cat_btn("算子运行", LogCategory::Runtime, current_cat),
        cat_btn("通信报文", LogCategory::Json, current_cat),
    ]
    .spacing(2)
    .align_y(Alignment::Center);

    let header = container(tabs)
        .width(Length::Fill)
        .height(Length::Fixed(24.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::STATUS_BAR_BG).into());
            s
        })
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 8.0,
            right: 0.0,
        });

    // 日志正文（按当前分类渲染，限制最多 LOG_RENDER_LIMIT 条）
    let body: Element<'_, Message> = match active {
        None => text("(未打开 tab)").color(theme::TEXT_WEAK).size(11.0).into(),
        Some(tab) => match current_cat {
            LogCategory::Action => view_run_logs(&tab.action_logs),
            LogCategory::Runtime => view_run_logs(&tab.runtime_logs),
            LogCategory::Json => view_json_logs(&tab.json_logs),
        },
    };

    let body_scroll = scrollable(body)
        .width(Length::Fill)
        .height(Length::Fill);

    let col = column![header, body_scroll]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(col)
        .width(Length::Fill)
        .height(Length::Fixed(LOG_PANEL_HEIGHT))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::PANEL_BG).into());
            s
        })
        .into()
}

/// 渲染运行日志（action / runtime 共用）：每条 [时间戳 消息]，按 level 着色。
fn view_run_logs(logs: &[super::state::RunLogEntry]) -> Element<'_, Message> {
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
            super::state::LogLevel::Info => theme::TEXT_HOVER,
            super::state::LogLevel::Success => theme::success(),
            super::state::LogLevel::Warning => theme::warning(),
            super::state::LogLevel::Error => theme::danger(),
        };
        let line = row![
            text(entry.timestamp.clone()).color(theme::TEXT_WEAK).size(10.0),
            text(entry.message.clone()).color(msg_color).size(11.0),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        col = col.push(line);
    }
    col.into()
}

/// 渲染 JSON 通信报文日志：每条 [方向 时间戳 标题]，payload 缩进显示。
fn view_json_logs(logs: &[super::state::JsonLogEntry]) -> Element<'_, Message> {
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
            text(entry.title.clone()).color(theme::TEXT_HOVER).size(11.0),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let payload = text(entry.payload.clone())
            .color(theme::TEXT_WEAK)
            .size(10.0);
        col = col.push(column![head, payload].spacing(2));
    }
    col.into()
}

// ===== 对话框叠加层 =====
//
// 通用结构：半透明黑色遮罩（mouse_area 点击=取消）+ 居中卡片
// （container.align_x/y=Center，CARD_BG 背景）。对话框内容由各 helper 构造。

fn view_new_model_dialog(state: &UiState) -> Element<'_, Message> {
    let title = text("新建建模").color(theme::TEXT_STRONG).size(13.0);
    let input = text_input(
        "请输入建模名称",
        &state.dag_editor.new_model_name_input,
    )
    .on_input(Message::NewModelNameInput)
    .on_submit(Message::NewModelConfirm)
    .padding(Padding {
        top: 6.0,
        bottom: 6.0,
        left: 8.0,
        right: 8.0,
    });

    let confirm_btn = dialog_button("确认", Message::NewModelConfirm, true);
    let cancel_btn = dialog_button("取消", Message::NewModelCancel, false);
    let btns = row![cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fixed(28.0));

    let card = column![title, input, btns]
        .spacing(12)
        .padding(Padding {
            top: 16.0,
            bottom: 16.0,
            left: 16.0,
            right: 16.0,
        })
        .width(Length::Fixed(DIALOG_WIDTH));

    let card = container(card).style(|_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(Color::from(theme::CARD_BG).into());
        s
    });
    dialog_overlay(card.into(), Message::NewModelCancel)
}

fn view_rename_dialog(state: &UiState) -> Element<'_, Message> {
    let title = text("重命名建模").color(theme::TEXT_STRONG).size(13.0);
    let input = text_input("请输入新名称", &state.dag_editor.rename_input)
        .on_input(Message::RenameInput)
        .on_submit(Message::RenameConfirm)
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 8.0,
            right: 8.0,
        });

    let confirm_btn = dialog_button("确认", Message::RenameConfirm, true);
    let cancel_btn = dialog_button("取消", Message::RenameCancel, false);
    let btns = row![cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fixed(28.0));

    let card = column![title, input, btns]
        .spacing(12)
        .padding(Padding {
            top: 16.0,
            bottom: 16.0,
            left: 16.0,
            right: 16.0,
        })
        .width(Length::Fixed(DIALOG_WIDTH));

    let card = container(card).style(|_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(Color::from(theme::CARD_BG).into());
        s
    });
    dialog_overlay(card.into(), Message::RenameCancel)
}

fn view_delete_confirm_dialog(state: &UiState) -> Element<'_, Message> {
    let name = state
        .dag_editor
        .delete_model_target_name
        .clone()
        .unwrap_or_default();
    let title = text("删除建模").color(theme::danger()).size(13.0);
    let hint = text(format!("确定删除「{}」吗？此操作可手动恢复（.deleted）。", name))
        .color(theme::TEXT_HOVER)
        .size(11.0);

    let confirm_btn = dialog_button("删除", Message::DeleteModelConfirm, true);
    let cancel_btn = dialog_button("取消", Message::DeleteModelCancel, false);
    let btns = row![cancel_btn, confirm_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fixed(28.0));

    let card = column![title, hint, btns]
        .spacing(12)
        .padding(Padding {
            top: 16.0,
            bottom: 16.0,
            left: 16.0,
            right: 16.0,
        })
        .width(Length::Fixed(DIALOG_WIDTH));

    let card = container(card).style(|_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(Color::from(theme::CARD_BG).into());
        s
    });
    dialog_overlay(card.into(), Message::DeleteModelCancel)
}

/// 通用对话框遮罩层：半透明黑色填满 + 居中卡片，点击遮罩=取消。
fn dialog_overlay(card: Element<'_, Message>, cancel: Message) -> Element<'_, Message> {
    let centered = container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.6 }.into());
            s
        });
    mouse_area(centered).on_press(cancel).into()
}

/// 对话框按钮：确认用 accent 背景，取消用透明边框。
fn dialog_button(label: &str, msg: Message, primary: bool) -> Element<'_, Message> {
    button(text(label).size(11.0))
        .width(Length::Fixed(72.0))
        .height(Length::Fixed(28.0))
        .style(move |_t, _status| {
            let mut s = iced::widget::button::Style::default();
            if primary {
                s.background = Some(Color::from(theme::accent()).into());
                s.text_color = Color::WHITE;
            } else {
                s.background = Some(Color::TRANSPARENT.into());
                s.text_color = theme::TEXT_HOVER;
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
