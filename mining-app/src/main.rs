//! 青萝挖掘分析应用入口（Iced 0.14 版本）。
//!
//! iced 0.14 入口使用 builder API：
//! `iced::application(MyApp::default, MyApp::update, MyApp::view)
//!      .title(MyApp::title)
//!      .subscription(MyApp::subscription)
//!      .theme(|_| Theme::Dark)
//!      .scale_factor(|_| 1.0)
//!      .default_font(Font::with_name("Microsoft YaHei"))
//!      .font(bytes)   // 多此调用追加字体
//!      .antialiasing(true)
//!      .window(window::Settings { ... })
//!      .run()`
//!
//! 阶段 1 为简化泛型推断问题，所有回调都用 MyApp 的关联函数（fn item），
//! 不使用匿名闭包（闭包的 lifetime 注解经常导致 "implementation of FnOnce is not
//! general enough"）。

use std::path::Path;
use std::time::Duration;

use iced::window;
use iced::{
    Color, Element, Font, Length, Subscription, Task, Theme, Size,
    widget::{column, container, row},
};

use mining_app::ui::{
    Message, UiState, ViewType, LogLevel,
    view_activity_bar, view_mining_analysis, view_settings,
    view_status_bar, view_title_bar,
};
use mining_app::ui::dag_canvas::{hit_test_node, hit_test_port, screen_to_world};
use mining_app::ui::theme;
use mining_app::dag_store;
use mining_app::geom::Vec2;

// ===== 主入口 =====
fn main() -> iced::Result {
    // Iced 0.14 默认已启用 Reactive Rendering（按需渲染），CPU/GPU 使用率相比
    // 0.13 已降低 60-80%。无需显式开启 on_demand_rendering 字段（该字段仅在
    // 0.15+ 新版 ShellSettings 中存在）。
    //
    // 下面通过 Settings::default() 显式保留 vsync=true，避免空闲时无意义提交帧。
    // 注意：0.14 中 iced::Settings 仅包含 vsync 字段，其余窗口/字体等配置通过
    // application builder API 链式设置。

    let win = window::Settings {
        size: Size::new(1440.0, 900.0),
        min_size: Some(Size::new(980.0, 640.0)),
        resizable: true,
        decorations: false,
        icon: mining_app::icon::create_app_icon(),
        ..Default::default()
    };

    iced::application(MyApp::boot, MyApp::update, MyApp::view)
        .title(MyApp::title)
        .subscription(MyApp::subscription)
        .theme(MyApp::theme)
        .scale_factor(MyApp::scale_factor)
        .default_font(Font::with_name("Microsoft YaHei"))
        .antialiasing(true)
        .window(win)
        .settings(iced::Settings {
            id: None,
            fonts: vec![],
            default_font: Font::with_name("Microsoft YaHei"),
            default_text_size: 14.0.into(),
            antialiasing: true,
            vsync: true, // 开启垂直同步，空闲时停止无意义帧提交（进一步降低CPU）
        })
        .run()
}

// ===== MyApp 关联函数封装 =====
//
// iced::application(...) 第 1 个泛型参数 S = UiState；
// 第 2/3 个参数 update/view 要求 `for<'a> fn(&'a mut S, M) -> T<...>` /
// `for<'a> fn(&'a S) -> Element<'a, M>`。
//
// 为避免闭包高阶 lifetime 推断失败，这里通过一个最小 struct MyApp 把 fn items
// 显式包装为 `&UiState` / `&mut UiState` 方法，签名清晰。
struct MyApp;

impl MyApp {
    /// boot 函数：返回初始 UiState + 一个异步 Task，用于查询主窗口 Id。
    /// iced 0.14 中 `iced::window::Id` 字段私有，用户无法直接构造，
    /// 只能通过 `iced::window::oldest()` 异步查询。Task resolve 后
    /// 通过 `SetMainWindowId` 消息把 Id 落到 UiState，供窗口控制按钮使用。
    fn boot() -> (UiState, Task<Message>) {
        let task = iced::window::oldest()
            .map(Message::SetMainWindowId);
        (UiState::default(), task)
    }

    fn title(_state: &UiState) -> String {
        "青萝".to_string()
    }

    fn update(state: &mut UiState, message: Message) -> Task<Message> {
        match message {
            Message::SwitchView(vt) => {
                if state.current_view == ViewType::MiningAnalysis
                    && vt != ViewType::MiningAnalysis
                {
                    mining_app::ui::mining_analysis_view::release_all_debug_sessions(
                        &mut state.dag_editor,
                    );
                }
                state.current_view = vt;
            }
            Message::Tick => {
                // 先 spawn（消费 pending_run_all / pending_run_up_to）再 poll，
                // 避免同一帧内既挂载任务又消费 rx
                mining_app::ui::try_spawn_pending_dag_exec(&mut state.dag_editor);
                mining_app::ui::poll_dag_exec_task(&mut state.dag_editor);
                // 首入挖掘分析视图时懒加载建模列表（启动后首个 Tick 触发）
                if state.current_view == ViewType::MiningAnalysis
                    && !state.dag_editor.models_loaded
                {
                    state.dag_editor.refresh_models();
                }
                // 推进 Logo 动画时间（每 Tick 0.5s）
                state.logo_time += 0.5;
            }
            Message::AnimTick => {
                // 高频轮询执行任务，及时回填节点状态（与主 Tick 合并 poll 无副作用）
                mining_app::ui::poll_dag_exec_task(&mut state.dag_editor);
                // 推进运行动画时间（~80ms → 0.08s）
                state.anim_time += 0.08;
            }
            Message::SetMainWindowId(id) => {
                state.main_window_id = id;
            }
            Message::WindowClose => {
                if let Some(id) = state.main_window_id {
                    return iced::window::close(id);
                }
            }
            Message::WindowToggleMaximize => {
                if let Some(id) = state.main_window_id {
                    return iced::window::toggle_maximize(id);
                }
            }
            Message::WindowMinimize => {
                if let Some(id) = state.main_window_id {
                    return iced::window::minimize(id, true);
                }
            }
            Message::WindowDrag => {
                if let Some(id) = state.main_window_id {
                    return iced::window::drag(id);
                }
            }
            Message::CanvasPress(pos) => {
                handle_canvas_press(state, pos);
            }
            Message::CanvasRelease(_pos) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.dragging_node_id = None;
                }
                state.canvas_pan_anchor = None;
            }
            Message::CanvasMove(pos) => {
                handle_canvas_move(state, pos);
            }
            Message::CanvasWheel { delta_y, pos } => {
                handle_canvas_wheel(state, delta_y, pos);
            }

            // ===== 建模列表 sidebar =====

            Message::OpenModel(id) => {
                match dag_store::load_model(&id) {
                    Some(rec) => state.dag_editor.open_model(rec),
                    None => {
                        if let Some(tab) = state.dag_editor.active_tab_mut() {
                            tab.add_action_log(
                                format!("加载建模失败（可能已被删除）：{}", id),
                                LogLevel::Error,
                            );
                        }
                        state.dag_editor.refresh_models();
                    }
                }
            }
            Message::NewModelClick => {
                state.dag_editor.show_new_model_dialog = true;
                state.dag_editor.new_model_name_input.clear();
            }
            Message::NewModelNameInput(s) => {
                state.dag_editor.new_model_name_input = s;
            }
            Message::NewModelConfirm => {
                let name = state.dag_editor.new_model_name_input.trim().to_string();
                let name = if name.is_empty() {
                    "未命名建模".to_string()
                } else {
                    name
                };
                state.dag_editor.create_model(&name);
                state.dag_editor.show_new_model_dialog = false;
                state.dag_editor.new_model_name_input.clear();
            }
            Message::NewModelCancel => {
                state.dag_editor.show_new_model_dialog = false;
                state.dag_editor.new_model_name_input.clear();
            }
            Message::RenameModelClick(id) => {
                let cur_name = state
                    .dag_editor
                    .models
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                state.dag_editor.rename_target_id = Some(id);
                state.dag_editor.rename_input = cur_name;
            }
            Message::RenameInput(s) => {
                state.dag_editor.rename_input = s;
            }
            Message::RenameConfirm => {
                if let Some(id) = state.dag_editor.rename_target_id.take() {
                    let new_name = state.dag_editor.rename_input.trim().to_string();
                    if !new_name.is_empty() {
                        state.dag_editor.rename_model(&id, &new_name);
                    }
                }
                state.dag_editor.rename_input.clear();
            }
            Message::RenameCancel => {
                state.dag_editor.rename_target_id = None;
                state.dag_editor.rename_input.clear();
            }
            Message::DeleteModelClick(id, name) => {
                state.dag_editor.request_delete_model(&id, &name);
            }
            Message::DeleteModelConfirm => {
                if let Some(id) = state.dag_editor.delete_model_target_id.take() {
                    state.dag_editor.delete_model(&id);
                }
                state.dag_editor.delete_model_target_name = None;
                state.dag_editor.show_delete_model_dialog = false;
            }
            Message::DeleteModelCancel => {
                state.dag_editor.delete_model_target_id = None;
                state.dag_editor.delete_model_target_name = None;
                state.dag_editor.show_delete_model_dialog = false;
            }

            // ===== Tab 栏 =====

            Message::SwitchTab(i) => {
                state.dag_editor.switch_to_tab(i);
            }
            Message::CloseTab(i) => {
                state.dag_editor.close_tab(i);
                state.dag_editor.hovered_tab = None;
            }
            Message::TabHover(idx) => {
                state.dag_editor.hovered_tab = idx;
            }

            // ===== 工具栏 =====

            Message::SaveTab => {
                state.dag_editor.save_active_tab();
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.add_action_log("已保存".to_string(), LogLevel::Success);
                }
            }
            Message::RunAllClick => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.pending_run_all = true;
                    tab.add_action_log(
                        "已请求执行 DAG，等待 Tick 轮询 spawn".to_string(),
                        LogLevel::Info,
                    );
                }
            }
            Message::ToggleDebug => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.debug_mode = !tab.debug_mode;
                    let msg = if tab.debug_mode {
                        "已开启调试模式"
                    } else {
                        "已关闭调试模式"
                    };
                    tab.add_action_log(msg.to_string(), LogLevel::Info);
                }
            }
            Message::ClearLogs => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.clear_active_logs();
                }
            }

            // ===== 日志面板 =====

            Message::SwitchLogCategory(cat) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.active_log_category = cat;
                }
            }
            Message::ToggleLogPanel => {
                state.dag_editor.log_panel_visible = !state.dag_editor.log_panel_visible;
            }

            // ===== 左侧合并面板 tab 切换 =====

            Message::SwitchLeftPanel(tab) => {
                state.dag_editor.active_left_panel = tab;
            }

            // ===== 画布右键菜单 / 菜单关闭 =====
            Message::CanvasRightClick(pos) => {
                handle_canvas_right_click(state, pos);
            }
            Message::ContextMenuClose => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.context_menu_screen_pos = None;
                    tab.context_menu_node_id = None;
                }
            }

            // ===== 连线创建（端口 → 拖动 → 端口命中 → add_edge） =====
            Message::ConnectStart {
                node_id,
                port_index,
                is_output,
            } => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.connecting_from = Some((node_id, port_index, is_output));
                    tab.connecting_drag_world = None;
                    tab.selected_node_id = None;
                    tab.context_menu_screen_pos = None;
                    tab.context_menu_node_id = None;
                }
            }
            Message::ConnectDrag(screen_pos) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    let world = screen_to_world(
                        screen_pos,
                        tab.canvas_offset,
                        tab.canvas_zoom,
                    );
                    tab.connecting_drag_world = Some(world);
                }
            }
            Message::ConnectRelease(screen_pos) => {
                handle_connect_release(state, screen_pos);
            }

            // ===== 算子面板：搜索 + 添加算子到画布 =====
            Message::OperatorSearchInput(s) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.operator_search_filter = s;
                }
            }
            Message::AddOperator(op_name) => {
                handle_add_operator_by_name(state, op_name);
            }

            // ===== 节点参数编辑（text_input 变更） =====
            Message::ParamInput(node_id, param_name, value) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    if let Some(node) = tab.graph.get_node_mut(&node_id) {
                        node.operator_type.set_param_value(&param_name, value);
                        tab.dirty = true;
                    }
                }
            }

            // ===== 节点右键菜单动作：运行到此节点 / 删除节点 =====
            Message::RunUpToNode(node_id) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.context_menu_screen_pos = None;
                    tab.context_menu_node_id = None;
                    tab.pending_run_up_to = Some(node_id.clone());
                    tab.add_action_log(
                        format!("已请求运行到节点 {}，等待 Tick 轮询 spawn", node_id),
                        LogLevel::Info,
                    );
                }
            }
            Message::DeleteNodeClick(node_id) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.context_menu_screen_pos = None;
                    tab.context_menu_node_id = None;
                    if tab.selected_node_id.as_deref() == Some(&node_id) {
                        tab.selected_node_id = None;
                    }
                    tab.graph.remove_node(&node_id);
                    tab.dirty = true;
                    tab.add_action_log(
                        format!("已删除节点 {}", node_id),
                        LogLevel::Info,
                    );
                }
            }
        }
        Task::none()
    }

    fn view(state: &UiState) -> Element<'_, Message> {
        let title_bar = view_title_bar(state);

        let activity_bar = view_activity_bar(state);
        let main_content = match state.current_view {
            ViewType::MiningAnalysis => view_mining_analysis(state),
            ViewType::Settings => view_settings(state),
        };

        // 活动栏和主体之间加极细分隔线
        let panel_divider = container(row![])
            .width(Length::Fixed(1.0))
            .height(Length::Fill)
            .style(|_t| {
                let mut s = iced::widget::container::Style::default();
                s.background = Some(Color {
                    r: 1.0, g: 1.0, b: 1.0, a: 18.0 / 255.0
                }.into());
                s
            });

        let main_panel = container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| {
                let mut s = iced::widget::container::Style::default();
                s.background = Some(Color::from(theme::panel_bg()).into());
                s
            });

        let body = row![activity_bar, panel_divider, main_panel]
            .width(Length::Fill)
            .height(Length::Fill)
            .spacing(0);

        let status_bar = view_status_bar(state);

        let inner = column![title_bar, body, status_bar]
            .width(Length::Fill)
            .height(Length::Fill);

        // 最外层：深蓝灰窗口底色，统一包裹
        container(inner)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| {
                let mut s = iced::widget::container::Style::default();
                s.background = Some(Color::from(theme::window_bg()).into());
                s
            })
            .into()
    }

    fn subscription(state: &UiState) -> Subscription<Message> {
        // 基础低频 Tick（500ms）：推进 Logo 动画 + 轮询 spawn/执行任务
        let base = iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick);
        // DAG 执行中追加高频动画 Tick（80ms）：推进运行动画 + 及时 poll 状态回填，
        // 让节点呼吸 / 边数据流动等动态效果流畅。无执行任务时不发出，降低 GPU 开销。
        let needs_anim = state.dag_editor.active_tab().map_or(false, |tab| {
            tab.pending_run_all
                || tab.pending_run_up_to.is_some()
                || tab.io_registry.has_executing()
        });
        if needs_anim {
            let anim = iced::time::every(Duration::from_millis(80)).map(|_| Message::AnimTick);
            Subscription::batch(vec![base, anim])
        } else {
            base
        }
    }

    fn theme(_state: &UiState) -> Theme {
        theme::dark_theme()
    }

    fn scale_factor(_state: &UiState) -> f32 {
        1.0
    }
}

// 让未使用的 import 保持（后续阶段要用）
#[allow(dead_code)]
fn _keep_font_load(_: ()) {
    let _ = load_chinese_font();
}

const FONT_CANDIDATES: &[&str] = &[
    "C:\\Windows\\Fonts\\msyh.ttc",
    "C:\\Windows\\Fonts\\simsun.ttc",
    "C:\\Windows\\Fonts\\msyhbd.ttc",
];

fn load_chinese_font() -> Vec<u8> {
    for path in FONT_CANDIDATES {
        if Path::new(path).exists() {
            if let Ok(bytes) = std::fs::read(path) {
                return bytes;
            }
        }
    }
    Vec::new()
}

// ===== 画布交互处理（拖拽节点 / 平移画布 / 滚轮缩放） =====
//
// 这些函数处理 `DagProgram::update` 转发来的 `Message::Canvas*` 消息，
// 集中在 `MyApp::update` 中通过 `&mut UiState` 修改状态。
//
// 坐标系约定（与 `dag_canvas::DagProgram::draw` 一致）：
// - 屏幕坐标 pos：鼠标相对画布左上角的像素位置（已扣除画布在窗口中的偏移）
// - 世界坐标 world：`world = (pos - offset) / zoom`，对应 graph 中节点的 position

/// 画布鼠标按下：命中节点 → 选中 + 开始拖拽；命中空白 → 开始平移画布。
fn handle_canvas_press(state: &mut UiState, pos: Vec2) {
    // 先取 offset/zoom（Copy），避免后续借用冲突
    let (offset, zoom) = match state.dag_editor.active_tab() {
        Some(tab) => (tab.canvas_offset, tab.canvas_zoom),
        None => return,
    };
    let world = screen_to_world(pos, offset, zoom);

    // 在只读 tab 上做命中检测，并取出命中节点的当前位置
    let hit = state.dag_editor.active_tab().and_then(|tab| {
        hit_test_node(&tab.graph, world).and_then(|id| {
            tab.graph.get_node(&id).map(|n| (id, n.position))
        })
    });

    // 应用变更到可变 tab
    let mut hit_node = false;
    if let Some(tab) = state.dag_editor.active_tab_mut() {
        match hit {
            Some((node_id, node_pos)) => {
                tab.selected_node_id = Some(node_id.clone());
                tab.dragging_node_id = Some(node_id);
                tab.drag_offset =
                    Vec2::new(world.x - node_pos.x, world.y - node_pos.y);
                hit_node = true;
            }
            None => {
                tab.selected_node_id = None;
                tab.dragging_node_id = None;
            }
        }
    }

    // UiState 级别的平移锚点（与 tab.dragging_node_id 互斥）
    if hit_node {
        state.canvas_pan_anchor = None;
    } else {
        state.canvas_pan_anchor = Some((pos, offset));
    }
}

/// 画布鼠标移动：拖拽中节点 → 更新节点位置；平移画布 → 更新 canvas_offset。
///
/// GPU 优化：前置快路径——非拖拽态直接 return。正常情况由 Program 的
/// CursorMoved 节流保证这类消息根本不会发，但作为双保险（例如未来
/// 改动后误发消息），这里也做判定避免任何 no-op 写入触发 view 重建。
fn handle_canvas_move(state: &mut UiState, pos: Vec2) {
    // 1) 画布平移（UiState 级别锚点）
    if let Some((anchor_pos, anchor_offset)) = state.canvas_pan_anchor {
        if let Some(tab) = state.dag_editor.active_tab_mut() {
            tab.canvas_offset = Vec2::new(
                anchor_offset.x + (pos.x - anchor_pos.x),
                anchor_offset.y + (pos.y - anchor_pos.y),
            );
        }
        return;
    }

    // 2) 节点拖拽（tab 级别 dragging_node_id）
    let Some(tab) = state.dag_editor.active_tab_mut() else { return; };
    let Some(node_id) = tab.dragging_node_id.clone() else {
        // 非拖拽态：无写入 → 立即退出，防止"发了消息但无事可做但仍触发
        // UiState 结构变化 → view → PartialEq 深比较"的 CPU 抖动
        return;
    };
    let offset = tab.canvas_offset;
    let zoom = tab.canvas_zoom;
    let drag_offset = tab.drag_offset;
    let world = screen_to_world(pos, offset, zoom);
    let new_pos = Vec2::new(world.x - drag_offset.x, world.y - drag_offset.y);
    if let Some(node) = tab.graph.get_node_mut(&node_id) {
        node.position = new_pos;
    }
    tab.dirty = true;
}

/// 画布滚轮缩放：以鼠标位置为锚点调整 zoom，并同步 offset 使锚点世界坐标不变。
fn handle_canvas_wheel(state: &mut UiState, delta_y: f32, pos: Vec2) {
    let Some(tab) = state.dag_editor.active_tab_mut() else { return; };
    let old_offset = tab.canvas_offset;
    let old_zoom = tab.canvas_zoom;

    // 每次滚轮 ±10%，向上（delta_y > 0）放大，向下缩小
    let factor = 1.0 + delta_y.signum() * 0.1;
    let new_zoom = (old_zoom * factor).clamp(0.2, 4.0);
    if (new_zoom - old_zoom).abs() < f32::EPSILON {
        return;
    }

    // 锚点世界坐标不变：world = (pos - old_offset) / old_zoom
    //                  new_offset = pos - world * new_zoom
    let safe_zoom = old_zoom.max(f32::EPSILON);
    let world_x = (pos.x - old_offset.x) / safe_zoom;
    let world_y = (pos.y - old_offset.y) / safe_zoom;
    let new_offset = Vec2::new(
        pos.x - world_x * new_zoom,
        pos.y - world_y * new_zoom,
    );

    tab.canvas_zoom = new_zoom;
    tab.canvas_offset = new_offset;
}

/// 画布右键：命中节点 → 节点菜单；否则 → 画布空白菜单。
fn handle_canvas_right_click(state: &mut UiState, pos: Vec2) {
    let (offset, zoom) = match state.dag_editor.active_tab() {
        Some(tab) => (tab.canvas_offset, tab.canvas_zoom),
        None => return,
    };
    let world = screen_to_world(pos, offset, zoom);
    let hit_node = state
        .dag_editor
        .active_tab()
        .and_then(|tab| hit_test_node(&tab.graph, world));

    if let Some(tab) = state.dag_editor.active_tab_mut() {
        tab.context_menu_screen_pos = Some(pos);
        tab.context_menu_node_id = hit_node;
        if let Some(ref nid) = tab.context_menu_node_id {
            // 同时选中该节点，便于参数面板展示
            tab.selected_node_id = Some(nid.clone());
        }
    }
}

/// 连线创建：ConnectRelease → 命中端口，两端方向相反（Output→Input / Input→Output）
/// 则调用 tab.graph.add_edge 创建一条新边；否则只清空 dragging 状态。
fn handle_connect_release(state: &mut UiState, screen_pos: Vec2) {
    use mining_app::dag::Edge;

    let (from_info, offset, zoom) = match state.dag_editor.active_tab() {
        Some(tab) => (
            tab.connecting_from.clone(),
            tab.canvas_offset,
            tab.canvas_zoom,
        ),
        None => return,
    };
    let world = screen_to_world(screen_pos, offset, zoom);
    let target_port = state
        .dag_editor
        .active_tab()
        .and_then(|tab| hit_test_port(&tab.graph, world));

    let mut add_edge: Option<Edge> = None;
    if let (Some((from_node, from_idx, from_out)), Some((to_node, to_idx, to_out))) =
        (from_info.as_ref(), target_port)
    {
        // 两端必须是不同节点
        if from_node != &to_node && from_out != &to_out {
            // 统一规范化：边的 source=输出端，target=输入端
            let (src_node, src_idx, tgt_node, tgt_idx) = if *from_out {
                (from_node.clone(), *from_idx, to_node, to_idx)
            } else {
                (to_node, to_idx, from_node.clone(), *from_idx)
            };
            let edge = Edge::new(src_node, src_idx, tgt_node, tgt_idx);
            add_edge = Some(edge);
        }
    }

    if let Some(tab) = state.dag_editor.active_tab_mut() {
        tab.connecting_from = None;
        tab.connecting_drag_world = None;
        if let Some(edge) = add_edge {
            match tab.graph.add_edge(edge) {
                Ok(()) => {
                    tab.dirty = true;
                    tab.add_action_log("已创建连线".to_string(), LogLevel::Info);
                }
                Err(e) => {
                    tab.add_action_log(
                        format!("创建连线失败：{}", e),
                        LogLevel::Error,
                    );
                }
            }
        }
    }
}

/// AddOperator：在激活 tab 画布中心/鼠标最后位置（或世界原点）新增一个节点，
/// 从 `dag::get_all_operator_types()` 按名称匹配 OperatorType。
fn handle_add_operator_by_name(state: &mut UiState, op_name: String) {
    use mining_app::dag::{get_all_operator_types, Node};

    let op_type = match get_all_operator_types()
        .into_iter()
        .find(|op| op.name() == op_name)
    {
        Some(t) => t,
        None => {
            if let Some(tab) = state.dag_editor.active_tab_mut() {
                tab.add_action_log(
                    format!("未找到算子：{}", op_name),
                    LogLevel::Error,
                );
            }
            return;
        }
    };

    if let Some(tab) = state.dag_editor.active_tab_mut() {
        // 优先放置在画布可视区域中心（屏幕中心反推世界坐标）
        let world = if let Some(pp) = tab.pending_add_operator_world.take() {
            pp
        } else {
            // 约等于画布中心（300x200 近似）
            screen_to_world(Vec2::new(300.0, 200.0), tab.canvas_offset, tab.canvas_zoom)
        };
        let node = Node::new(op_type, world);
        let node_id = node.id.clone();
        tab.graph.add_node(node);
        tab.selected_node_id = Some(node_id);
        tab.dirty = true;
        tab.add_action_log(format!("已添加算子 {}", op_name), LogLevel::Info);
    }
}
