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

use std::path::{Path, PathBuf};
use std::time::Duration;

use iced::window;
use iced::{
    Color, Element, Font, Length, Subscription, Task, Theme, Size,
    widget::{column, container, row},
};

use mining_app::ui::{
    DagTab, Message, UiState, ViewType, LogLevel,
    view_activity_bar, view_mining_analysis, view_settings,
    view_status_bar, view_title_bar,
};
use mining_app::ui::mining::dag_canvas::{hit_test_node, hit_test_port, screen_to_world};
use mining_app::ui::theme;
use mining_app::mining::dag_store;
use mining_app::mining::geom::Vec2;

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
        // 窗口图标按磁盘配置加载：默认走纯代码折线图，用户在设置页
        // 选了「从文件加载」则解码 png/jpg → RGBA。仅启动时生效，
        // 运行时改 LogoSource 需重启进程。
        icon: mining_app::icon::create_app_icon_from(&mining_app::config::load_brand()),
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
        let mut state = UiState::default();
        // 启动时从磁盘配置一次性读入 hide_mining 与品牌配置：
        // - hide_mining 写入 SettingsState，让活动栏首帧即按用户上次选择渲染
        //   （不闪一下挖掘按钮）；
        // - brand 同时写入 settings.brand（草稿，供设置页输入框回填）与
        //   brand_snapshot（只读快照，供 title / view_title_bar 即时引用，
        //   避免每次 view 都读盘）。设置页「应用并保存」后会刷新这两处。
        if let Ok(cfg) = mining_app::config::load_config() {
            state.settings.hide_mining = cfg.hide_mining;
            state.settings.brand = cfg.brand.clone();
            state.brand_snapshot = cfg.brand;
        } else {
            // 配置读取失败也兜底：从 load_brand 走默认值链路
            let brand = mining_app::config::load_brand();
            state.settings.brand = brand.clone();
            state.brand_snapshot = brand;
        }
        // 同步初始化设置页输入框草稿值（用 effective_* 的回退结果回填，
        // 让用户看到「当前生效的值」而非 Option 的原始字符串）
        let b = &state.settings.brand;
        state.settings.brand_name_input = b.app_name.clone().unwrap_or_default();
        state.settings.brand_subtitle_input = b.subtitle.clone().unwrap_or_default();
        state.settings.brand_logo_path_input = b.logo_path.clone().unwrap_or_default();
        (state, task)
    }

    fn title(state: &UiState) -> String {
        // 引用 brand_snapshot 即时返回当前生效的应用名；BrandApply 后
        // 立刻刷新 snapshot，下一帧 title 即变。空值由 effective_app_name
        // 兜底为 "青萝"。
        state.brand_snapshot.effective_app_name().to_string()
    }

    fn update(state: &mut UiState, message: Message) -> Task<Message> {
        match message {
            Message::SwitchView(vt) => {
                if state.current_view == ViewType::MiningAnalysis
                    && vt != ViewType::MiningAnalysis
                {
                    mining_app::ui::mining::mining_analysis_view::release_all_debug_sessions(
                        &mut state.dag_editor,
                    );
                }
                // 离开插件视图时隐藏 WebView2 子窗口（airspace 遮挡）；
                // 插件之间切换不隐藏，仅注入新插件内容。
                #[cfg(windows)]
                if state.current_view.is_webview_plugin() && !vt.is_webview_plugin() {
                    state.webview_menu.hide();
                }
                state.current_view = vt.clone();
                // 进入插件视图：懒创建 / 显示 WebView2，并注入对应插件内容
                #[cfg(windows)]
                if vt.is_webview_plugin() {
                    return enter_webview_plugin(state, vt.plugin_id().unwrap_or(""));
                }
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
                // 轮询网页菜单 JS → Rust IPC
                #[cfg(windows)]
                {
                    handle_webview_ipc(state);
                    // 网页层后台预热：启动约 2.5s 后的空闲期静默创建
                    // WebView2，把冷启动开销移出首次点击路径。DAG 执行中
                    // 跳过本拍，避免与运行动画争抢主线程。
                    let busy = state
                        .dag_editor
                        .active_tab()
                        .map_or(false, |tab| tab.io_registry.has_executing());
                    if !busy {
                        state.webview_menu.prewarm_on_tick(
                            state.current_view.is_webview_plugin(),
                            &state.brand_snapshot,
                        );
                    }
                }
            }
            Message::AnimTick => {
                // 高频轮询执行任务，及时回填节点状态（与主 Tick 合并 poll 无副作用）
                mining_app::ui::poll_dag_exec_task(&mut state.dag_editor);
                // 推进运行动画时间（~80ms → 0.08s）
                state.anim_time += 0.08;
                // 兜底刷新 Text 参数编辑器缓存：抽屉打开且 selected_node_id
                // 与上次预热不一致时（用户在抽屉开启时点击了其他节点 / AddNode），
                // 由 AnimTick 集中处理，避免在所有 selected_node_id 赋值点埋钩子。
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    if tab.params_drawer_open {
                        let need_refresh = tab.text_editors_node_id.as_deref()
                            != tab.selected_node_id.as_deref();
                        if need_refresh {
                            refresh_text_editors_for_current_node(tab);
                        }
                    }
                }
                #[cfg(windows)]
                handle_webview_ipc(state);
            }
            // 网页菜单可见期间的高频 IPC 轮询 + popup 位置兜底校准
            Message::WebViewTick => {
                #[cfg(windows)]
                {
                    handle_webview_ipc(state);
                    state.webview_menu.sync_bounds();
                }
            }
            // ===== wry WebView 菜单测试 =====
            Message::WebViewHwnd(hwnd) => {
                #[cfg(windows)]
                {
                    if let Some(hwnd) = hwnd {
                        state.webview_menu.set_hwnd(hwnd);
                        if let Some(id) = state.main_window_id {
                            return iced::window::size(id)
                                .map(Message::WebViewWindowSize);
                        }
                    } else {
                        state.webview_error =
                            Some("无法获取 Win32 窗口句柄，WebView 仅支持 Windows".into());
                        eprintln!("[webview-menu] 无法获取 Win32 窗口句柄");
                    }
                }
                #[cfg(not(windows))]
                let _ = hwnd;
            }
            Message::WebViewWindowSize(size) => {
                #[cfg(windows)]
                {
                    let inside = state.current_view.is_webview_plugin();
                    if state.webview_menu.is_ready() {
                        // 已创建：校准矩形；仅在用户正查看插件视图时显示并注入内容
                        state.webview_menu.set_size(size);
                        if inside {
                            state.webview_menu.show();
                            state.webview_error = None;
                            if let Some(id) = state.current_view.plugin_id() {
                                state.webview_menu.load_plugin(id);
                            }
                        }
                    } else if inside {
                        // 用户已进入视图：立即创建并显示，再注入当前插件内容
                        if let Err(e) = state.webview_menu.build(size, &state.brand_snapshot) {
                            state.webview_error = Some(e);
                        } else {
                            state.webview_error = None;
                            // popup 创建时无 WS_VISIBLE，build 成功后立即显示
                            state.webview_menu.show();
                            if let Some(id) = state.current_view.plugin_id() {
                                state.webview_menu.load_plugin(id);
                            }
                        }
                    } else {
                        // 启动预热链路：只缓存尺寸，静默创建交给 Tick 倒计时
                        state.webview_menu.cache_size(size);
                    }
                }
                #[cfg(not(windows))]
                let _ = size;
            }
            Message::WindowResized(size) => {
                // 仅插件视图可见时需要跟随（其他视图 wgpu 自行处理）
                #[cfg(windows)]
                if state.current_view.is_webview_plugin() {
                    state.webview_menu.set_size(size);
                }
                #[cfg(not(windows))]
                let _ = size;
            }
            Message::WindowMoved(_point) => {
                // 主窗口移动后 popup 重新定位（relocate 内部自行取屏幕坐标）
                #[cfg(windows)]
                if state.current_view.is_webview_plugin() {
                    state.webview_menu.relocate();
                }
                #[cfg(not(windows))]
                let _ = _point;
            }
            Message::SetMainWindowId(id) => {
                state.main_window_id = id;
                // 启动即预热网页层：先取 HWND（回填后顺带查尺寸），
                // 使用户首次点击「网页」前句柄链路已走完。
                #[cfg(windows)]
                if let Some(win_id) = id {
                    return iced::window::run(
                        win_id,
                        mining_app::ui::webview_menu::WebViewMenu::extract_hwnd,
                    )
                    .map(Message::WebViewHwnd);
                }
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

            // ===== 建模目录（分类文件夹） =====

            Message::OpenFolder(id) => {
                state.dag_editor.navigate_to_folder(&id);
            }
            Message::FolderNav(path) => {
                state.dag_editor.navigate_to_folder(&path);
            }
            Message::NewFolderClick => {
                state.dag_editor.show_new_folder_dialog = true;
                state.dag_editor.new_folder_name_input.clear();
            }
            Message::NewFolderNameInput(s) => {
                state.dag_editor.new_folder_name_input = s;
            }
            Message::NewFolderConfirm => {
                let name = state.dag_editor.new_folder_name_input.trim().to_string();
                if !name.is_empty() {
                    if let Err(e) = state.dag_editor.create_folder(&name) {
                        if let Some(tab) = state.dag_editor.active_tab_mut() {
                            tab.add_action_log(e, LogLevel::Error);
                        }
                    }
                }
                state.dag_editor.show_new_folder_dialog = false;
                state.dag_editor.new_folder_name_input.clear();
            }
            Message::NewFolderCancel => {
                state.dag_editor.show_new_folder_dialog = false;
                state.dag_editor.new_folder_name_input.clear();
            }
            Message::RenameFolderClick(id) => {
                let cur_name = state
                    .dag_editor
                    .folders
                    .iter()
                    .find(|f| f.id == id)
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                state.dag_editor.rename_folder_target_id = Some(id);
                state.dag_editor.rename_folder_input = cur_name;
            }
            Message::RenameFolderInput(s) => {
                state.dag_editor.rename_folder_input = s;
            }
            Message::RenameFolderConfirm => {
                if let Some(id) = state.dag_editor.rename_folder_target_id.take() {
                    let new_name = state.dag_editor.rename_folder_input.trim().to_string();
                    if !new_name.is_empty() {
                        if let Err(e) = state.dag_editor.rename_folder(&id, &new_name) {
                            if let Some(tab) = state.dag_editor.active_tab_mut() {
                                tab.add_action_log(e, LogLevel::Error);
                            }
                        }
                    }
                }
                state.dag_editor.rename_folder_input.clear();
            }
            Message::RenameFolderCancel => {
                state.dag_editor.rename_folder_target_id = None;
                state.dag_editor.rename_folder_input.clear();
            }
            Message::DeleteFolderClick(id, name) => {
                state.dag_editor.request_delete_folder(&id, &name);
            }
            Message::DeleteFolderConfirm => {
                if let Some(id) = state.dag_editor.delete_folder_target_id.take() {
                    if let Err(e) = state.dag_editor.delete_folder(&id) {
                        if let Some(tab) = state.dag_editor.active_tab_mut() {
                            tab.add_action_log(e, LogLevel::Error);
                        }
                    }
                }
                state.dag_editor.delete_folder_target_name = None;
                state.dag_editor.show_delete_folder_dialog = false;
            }
            Message::DeleteFolderCancel => {
                state.dag_editor.delete_folder_target_id = None;
                state.dag_editor.delete_folder_target_name = None;
                state.dag_editor.show_delete_folder_dialog = false;
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
            // ===== 长文本参数编辑（text_editor Action）=====
            // 对 `text_editors` 缓存中对应 `Content` 调用 `perform(action)` 应用编辑，
            // 然后把 `Content::text()` 同步到 `param_values`，与 text_input 落盘路径合流。
            Message::ParamTextEdit(node_id, param_name, action) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    let key = format!("{}::{}", node_id, param_name);
                    if let Some(content) = tab.text_editors.get_mut(&key) {
                        content.perform(action);
                        let new_text = content.text();
                        if let Some(node) = tab.graph.get_node_mut(&node_id) {
                            node.operator_type.set_param_value(&param_name, new_text);
                            tab.dirty = true;
                        }
                    }
                }
            }
            Message::CloseParamsDrawer => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.params_drawer_open = false;
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
            Message::ResetCanvasView => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.context_menu_screen_pos = None;
                    tab.context_menu_node_id = None;
                    tab.canvas_offset = Vec2::ZERO;
                    tab.canvas_zoom = 1.0;
                }
            }
            Message::DeleteNodeClick(node_id) => {
                if let Some(tab) = state.dag_editor.active_tab_mut() {
                    tab.context_menu_screen_pos = None;
                    tab.context_menu_node_id = None;
                    if tab.selected_node_id.as_deref() == Some(&node_id) {
                        tab.selected_node_id = None;
                    }
                    // 同步从多选列表中移除（若存在）
                    tab.selected_node_ids.retain(|id| id != &node_id);
                    tab.graph.remove_node(&node_id);
                    // 清理被删节点的长文本编辑器缓存，避免悬挂 key
                    let prefix = format!("{}::", node_id);
                    tab.text_editors.retain(|k, _| !k.starts_with(&prefix));
                    if tab.text_editors_node_id.as_deref() == Some(node_id.as_str()) {
                        tab.text_editors_node_id = None;
                    }
                    tab.dirty = true;
                    tab.add_action_log(
                        format!("已删除节点 {}", node_id),
                        LogLevel::Info,
                    );
                }
            }

            // ===== 多选与对齐 =====
            Message::Keyboard(kb_event) => {
                // 仅消费修饰键状态变化信息，其余按键事件忽略
                use iced::keyboard::Event as KbEvent;
                let mods = match kb_event {
                    KbEvent::ModifiersChanged(m) => Some(m),
                    KbEvent::KeyPressed { modifiers, .. }
                    | KbEvent::KeyReleased { modifiers, .. } => Some(modifiers),
                };
                if let Some(m) = mods {
                    state.modifiers = m;
                }
            }
            Message::AlignTop => handle_align_top(state),
            Message::AlignLeft => handle_align_left(state),
            Message::ToggleHideMining => {
                // 翻转内存开关 → 即时影响活动栏渲染
                state.settings.hide_mining = !state.settings.hide_mining;
                let now_hidden = state.settings.hide_mining;
                // 落盘：失败不阻断 UI，仅写入 settings.last_result 供后续提示
                let save_msg = match mining_app::config::save_hide_mining(now_hidden) {
                    Ok(()) => format!(
                        "已{}「挖掘」入口并保存",
                        if now_hidden { "隐藏" } else { "显示" }
                    ),
                    Err(e) => format!("保存失败：{e}"),
                };
                state.settings.last_result = Some((true, save_msg));
                // 若当前正在 MiningAnalysis 视图且切到隐藏，自动跳到 Settings，
                // 避免用户停留在已裁掉的视图里无入口返回。
                if now_hidden && state.current_view == ViewType::MiningAnalysis {
                    // 释放挖掘视图占用的调试会话再切走
                    mining_app::ui::mining::mining_analysis_view::release_all_debug_sessions(
                        &mut state.dag_editor,
                    );
                    state.current_view = ViewType::Settings;
                }
            }

            // ===== 品牌与外观：输入框草稿（不立即落盘，只改 SettingsState 内存值） =====

            Message::BrandNameInput(s) => {
                state.settings.brand_name_input = s;
            }
            Message::BrandSubtitleInput(s) => {
                state.settings.brand_subtitle_input = s;
            }
            Message::BrandLogoPathInput(s) => {
                state.settings.brand_logo_path_input = s;
                // 路径同步刷新 brand.logo_path（File 模式下生效）
                let trimmed = state.settings.brand_logo_path_input.trim().to_string();
                state.settings.brand.logo_path = if trimmed.is_empty() { None } else { Some(trimmed) };
            }
            Message::BrandLogoDefault => {
                // 切换到默认折线图：清掉 File 路径字段
                state.settings.brand.logo = mining_app::config::LogoSource::Default;
                state.settings.brand_logo_path_input.clear();
                state.settings.brand.logo_path = None;
                state.settings.last_result = Some((true, "已切到默认图标，重启后生效".to_string()));
            }
            Message::BrandLogoFilePick => {
                // 切换到 File 模式 + 打开文件选择对话框
                state.settings.brand.logo = mining_app::config::LogoSource::File;
                // rfd 异步打开文件对话框，返回 Task<PathBuf>，
                // resolve 后由 BrandLogoFilePicked 回填
                return open_logo_file_dialog();
            }
            Message::BrandLogoFilePicked(path) => {
                // 文件对话框回调：把选中路径写入输入框 + brand.logo_path
                let p = path.to_string_lossy().to_string();
                state.settings.brand_logo_path_input = p.clone();
                state.settings.brand.logo = mining_app::config::LogoSource::File;
                state.settings.brand.logo_path = if p.is_empty() { None } else { Some(p) };
                state.settings.last_result = Some((true, "已选择 Logo 文件，点击应用并保存生效".to_string()));
            }
            Message::BrandTitleLogoChange(mode) => {
                // 切换标题栏 Logo 样式：草稿立即生效（不落盘，需用户点应用才固化）
                state.settings.brand.title_logo = mode;
                // 但 brand_snapshot 也立即同步，让标题栏即时刷新
                state.brand_snapshot.title_logo = mode;
                state.settings.last_result = Some((true, "标题栏 Logo 样式已即时切换".to_string()));
            }
            Message::BrandApply => {
                // 把所有输入框草稿合并到 brand，落盘 + 刷新 snapshot
                let name = state.settings.brand_name_input.trim().to_string();
                let sub = state.settings.brand_subtitle_input.trim().to_string();
                state.settings.brand.app_name = if name.is_empty() { None } else { Some(name) };
                state.settings.brand.subtitle = if sub.is_empty() { None } else { Some(sub) };
                // logo_path 与 logo 在各自消息中已更新到 brand
                // logo_path 草稿兜底：从 brand_logo_path_input 同步
                let path_trim = state.settings.brand_logo_path_input.trim().to_string();
                state.settings.brand.logo_path = if path_trim.is_empty() { None } else { Some(path_trim) };

                let brand_clone = state.settings.brand.clone();
                let apply_msg = match mining_app::config::save_brand(&brand_clone) {
                    Ok(()) => {
                        // 落盘成功：刷新 snapshot 让 title / 标题栏 / WebView 即时生效
                        state.brand_snapshot = brand_clone;
                        "品牌设置已应用：标题栏与窗口标题即时生效".to_string()
                    }
                    Err(e) => format!("品牌保存失败：{e}"),
                };
                let ok = !apply_msg.starts_with("品牌保存失败");
                state.settings.last_result = Some((ok, apply_msg));
            }
            Message::BrandReset => {
                // 恢复默认：清空所有输入框 + 重置 brand + 重置 brand_snapshot
                state.settings.brand = mining_app::config::BrandConfig::default();
                state.settings.brand_name_input.clear();
                state.settings.brand_subtitle_input.clear();
                state.settings.brand_logo_path_input.clear();
                state.brand_snapshot = state.settings.brand.clone();
                state.settings.last_result = Some((true, "已恢复默认品牌设置".to_string()));
            }
        }
        Task::none()
    }

    fn view(state: &UiState) -> Element<'_, Message> {
        let title_bar = view_title_bar(state);

        let activity_bar = view_activity_bar(state);
        let main_content = match &state.current_view {
            ViewType::MiningAnalysis => view_mining_analysis(state),
            ViewType::Settings => view_settings(state),
            // 实际界面由 WebView2 子窗口覆盖渲染；这里只作为加载前/失败时的底层占位
            ViewType::Plugin(_) => match &state.webview_error {
                Some(e) => mining_app::ui::placeholder_view(
                    "插件加载失败",
                    e,
                ),
                // WebView2 透明控制器首帧到达前会透出这层加载视图
                None => mining_app::ui::webview_loading_view(state.anim_time),
            },
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
        // 键盘事件订阅：追踪 Ctrl/Shift 等修饰键状态，用于画布 Ctrl+Click 多选。
        // iced::keyboard::listen 只会派发被 widget 链未消费的键盘事件，
        // 不会干扰 text_input 等组件的按键处理。
        let keyboard = iced::keyboard::listen().map(Message::Keyboard);
        // 窗口尺寸变化：网页菜单可见时用于同步 popup 宿主窗口尺寸
        let resize = iced::window::resize_events()
            .map(|(_id, size)| Message::WindowResized(size));
        // 窗口移动：popup 是独立顶级窗口，必须跟随主窗口重新定位
        let moved = iced::window::events().filter_map(|(_id, event)| match event {
            window::Event::Moved(point) => Some(Message::WindowMoved(point)),
            _ => None,
        });

        // 网页层冷加载中也订阅高频 Tick，驱动加载页 spinner 旋转
        // （WebView2 就绪后即停止，避免常驻开销）。
        #[cfg(windows)]
        let webview_loading = state.current_view.is_webview_plugin()
            && state.webview_error.is_none()
            && !state.webview_menu.is_ready();
        #[cfg(not(windows))]
        let webview_loading = false;

        let mut subs = vec![base, keyboard, resize, moved];
        if needs_anim || webview_loading {
            // DAG 执行中 / 网页加载中追加高频动画 Tick（80ms）
            subs.push(
                iced::time::every(Duration::from_millis(80)).map(|_| Message::AnimTick),
            );
        }
        // 插件视图可见时追加 120ms IPC 轮询 Tick，其余时间不发出以节省开销
        if state.current_view.is_webview_plugin() {
            subs.push(
                iced::time::every(Duration::from_millis(120)).map(|_| Message::WebViewTick),
            );
        }
        Subscription::batch(subs)
    }

    fn theme(_state: &UiState) -> Theme {
        theme::dark_theme()
    }

    fn scale_factor(_state: &UiState) -> f32 {
        1.0
    }
}

// ===== wry WebView 菜单测试（仅 Windows）=====
//
// WebView 是 !Send，整个创建/调用链都留在 winit 主线程的 update 中：
//
//   SwitchView(WebViewMenu)
//     → window::run(id, extract_hwnd)        // Task<Option<isize>>
//     → Message::WebViewHwnd(Some(hwnd))
//     → window::size(id)                     // Task<Size>
//     → Message::WebViewWindowSize(size)
//     → WebViewMenu::build(size)             // build_as_child 子 HWND
//
// 再次进入时 webview 已存在，直接 set_visible(true) 并校准 bounds。

/// 「选择 Logo 文件」按钮触发的文件对话框。
///
/// 用 rfd 异步打开（与编译目录选择等一致），过滤 png/jpg/ico 三种扩展名。
/// 选中的路径通过 `BrandLogoFilePicked(PathBuf)` 回填到 update。
fn open_logo_file_dialog() -> Task<Message> {
    use rfd::AsyncFileDialog;

    let dialog = AsyncFileDialog::new()
        .set_title("选择 Logo 图片")
        .add_filter("图片文件", &["png", "jpg", "jpeg", "ico"])
        .add_filter("所有文件", &["*"]);

    // rfd::AsyncFileDialog.pick_file() 返回 Future<Output = Option<FileHandle>>；
    // iced Task::perform 把它包成 Task，resolve 后通过 map 转成 Message。
    // 这里用 iced::Task::perform 而非裸闭包，让 iced 调度器驱动 future。
    iced::Task::perform(
        async move { dialog.pick_file().await },
        |maybe_handle| match maybe_handle {
            Some(handle) => Message::BrandLogoFilePicked(PathBuf::from(handle.path())),
            None => Message::BrandLogoFilePicked(PathBuf::new()),
        },
    )
}

/// 进入插件视图：已创建则显示并注入插件内容，否则启动「取 HWND → 取尺寸 → 创建」链路。
///
/// 启动预热通常已把 HWND / 尺寸 / WebView2 全部备好，这里按就绪程度走
/// 最快路径：ready → show + load_plugin；HWND+尺寸就绪 → 同步 build+show+load_plugin
/// （零 Task 往返）；只有 HWND → 补查尺寸；都没有 → 从头取句柄。
#[cfg(windows)]
fn enter_webview_plugin(state: &mut UiState, plugin_id: &str) -> Task<Message> {
    use mining_app::ui::webview_menu::WebViewMenu;

    if state.webview_menu.is_ready() {
        state.webview_menu.show();
        state.webview_menu.load_plugin(plugin_id);
        return match state.main_window_id {
            Some(id) => iced::window::size(id).map(Message::WebViewWindowSize),
            None => Task::none(),
        };
    }

    if state.webview_menu.has_hwnd() {
        if let Some(size) = state.webview_menu.cached_size() {
            // 预热已缓存句柄与尺寸：当前帧直接创建，省掉两次异步往返
            match state.webview_menu.build(size, &state.brand_snapshot) {
                Ok(()) => {
                    state.webview_error = None;
                    state.webview_menu.show();
                    state.webview_menu.load_plugin(plugin_id);
                }
                Err(e) => state.webview_error = Some(e),
            }
            return Task::none();
        }
        // HWND 已取到、尺寸查询还在途：补一次尺寸查询，回填消息会
        // 识别到用户已在视图内并立即 build + show + load_plugin。
        if let Some(id) = state.main_window_id {
            return iced::window::size(id).map(Message::WebViewWindowSize);
        }
    }

    match state.main_window_id {
        Some(id) => {
            // 回调返回 Send 的 Option<isize>，不跨线程传递任何 !Send 资源
            iced::window::run(id, WebViewMenu::extract_hwnd).map(Message::WebViewHwnd)
        }
        None => Task::none(),
    }
}

/// 排空网页菜单 JS → Rust IPC：经插件注册表路由后，把各插件的回复
/// 回填页面，并执行插件请求的宿主动作（如返回主视图）。
#[cfg(windows)]
fn handle_webview_ipc(state: &mut UiState) {
    use mining_app::ui::webview_plugins::HostAction;

    let outcomes = state.webview_menu.drain_commands();
    for outcome in outcomes {
        if let Some((kind, text)) = outcome.reply {
            state
                .webview_menu
                .notify_reply(&outcome.plugin, &kind, &text);
        }
        if outcome.host == Some(HostAction::Back) {
            state.webview_menu.hide();
            state.current_view = ViewType::MiningAnalysis;
        }
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

/// 预热当前选中节点的长文本参数（`ParamType::Text`）`text_editor::Content` 缓存。
///
/// 双击节点打开抽屉、或切换 `selected_node_id`（单击其他节点 / 右键节点 / 添加节点）
/// 后由 AnimTick 兜底调用。若节点已删除则清空缓存并把 `text_editors_node_id` 置 None。
///
/// 复用既有 `Content` 实例：若编辑器已存在（同一节点再次打开抽屉）则不重建，
/// 避免抹掉光标 / 选区状态；外部修改了 `param_values`（如重置参数）时需手动
/// 清缓存后再调用本函数以从最新值重建。
fn refresh_text_editors_for_current_node(tab: &mut mining_app::ui::state::DagTab) {
    use mining_app::mining::dag::ParamType;

    let current_id = tab.selected_node_id.clone();
    // 1) 先用不可变借用收集 (param_name, current_value) 列表 + node_id
    //    避免 `&node` 与 `&mut tab.text_editors` 同时存活。
    let collected: Option<(String, Vec<(String, String)>)> = current_id
        .as_deref()
        .and_then(|nid| tab.graph.get_node(nid))
        .map(|node| {
            let node_id = current_id.clone().unwrap_or_default();
            let pairs = node
                .operator_type
                .param_defs()
                .iter()
                .filter(|d| d.param_type == ParamType::Text)
                .map(|d| {
                    let v = node
                        .operator_type
                        .get_param_value(&d.name)
                        .unwrap_or_default();
                    (d.name.clone(), v)
                })
                .collect::<Vec<_>>();
            (node_id, pairs)
        });

    let Some((node_id, text_params)) = collected else {
        // 节点已删除或无选中节点：清空缓存避免悬挂引用
        tab.text_editors.clear();
        tab.text_editors_node_id = None;
        return;
    };

    // 2) 若仍是同一节点（双击 / 再次打开抽屉），不抹掉既有编辑器；仅补齐缺漏
    if tab.text_editors_node_id.as_deref() == Some(node_id.as_str()) {
        for (pname, value) in text_params {
            let key = format!("{}::{}", node_id, pname);
            if !tab.text_editors.contains_key(&key) {
                tab.text_editors.insert(
                    key,
                    iced::widget::text_editor::Content::with_text(&value),
                );
            }
        }
        return;
    }

    // 3) 切换节点：整体重建（保留同节点既有 Content 的光标 / 选区）
    tab.text_editors.clear();
    for (pname, value) in text_params {
        let key = format!("{}::{}", node_id, pname);
        tab.text_editors.insert(
            key,
            iced::widget::text_editor::Content::with_text(&value),
        );
    }
    tab.text_editors_node_id = Some(node_id);
}

/// 画布鼠标按下：命中节点 → 选中 + 开始拖拽；命中空白 → 开始平移画布。
///
/// 双击节点（同一节点两次左键按下间隔 < 400ms）→ 弹出右侧节点参数抽屉
/// （`tab.params_drawer_open = true`）。普通单击仍走选中 + 拖拽流程。
///
/// Ctrl 多选语义：
/// - Ctrl 按下 + 命中节点：toggle 该节点在 `selected_node_ids` 中的选中状态，
///   不修改 `selected_node_id`（避免参数面板抖动），也不开始拖拽。
///   若该节点是 `selected_node_id`，同步清空它，使下一次普通点击进入干净状态。
/// - 普通点击 + 命中节点：清空 `selected_node_ids` 回到单选语义（旧行为）。
/// - 普通点击 + 命中空白：清空 `selected_node_id` 与 `selected_node_ids`。
fn handle_canvas_press(state: &mut UiState, pos: Vec2) {
    // 先取 offset/zoom（Copy），避免后续借用冲突
    let (offset, zoom) = match state.dag_editor.active_tab() {
        Some(tab) => (tab.canvas_offset, tab.canvas_zoom),
        None => return,
    };
    let world = screen_to_world(pos, offset, zoom);
    let ctrl_pressed = state.modifiers.control();

    // 在只读 tab 上做命中检测，并取出命中节点的当前位置
    let hit = state.dag_editor.active_tab().and_then(|tab| {
        hit_test_node(&tab.graph, world).and_then(|id| {
            tab.graph.get_node(&id).map(|n| (id, n.position))
        })
    });

    // 双击检测：若本次命中节点 == 上一次命中节点，且间隔 < 400ms → 弹出右侧参数抽屉。
    // 注意：双击并不打断单次点击的选中/拖拽语义，仅额外打开抽屉。
    let now = std::time::Instant::now();
    let hit_node_id: Option<String> = hit.as_ref().map(|(id, _)| id.clone());
    let is_double_click = match (hit_node_id.as_ref(), &state.last_canvas_press_node_id) {
        (Some(cur), Some(prev)) if cur == prev => {
            state
                .last_canvas_press_at
                .map(|t| now.duration_since(t).as_millis() < 400)
                .unwrap_or(false)
        }
        _ => false,
    };
    if is_double_click {
        if let Some(tab) = state.dag_editor.active_tab_mut() {
            tab.params_drawer_open = true;
            // 双击触发抽屉打开：立即为当前节点的 Text 参数预热 text_editor 缓存，
            // 避免首帧 view 因缓存缺失而临时降级为单行 text_input。
            refresh_text_editors_for_current_node(tab);
        }
    }
    // 不论是否双击，都刷新"上一次按下"记录供下次比对
    state.last_canvas_press_at = Some(now);
    state.last_canvas_press_node_id = hit_node_id;

    // 应用变更到可变 tab
    let mut hit_node = false;
    if let Some(tab) = state.dag_editor.active_tab_mut() {
        match hit {
            Some((node_id, node_pos)) => {
                if ctrl_pressed {
                    // Ctrl+Click：toggle 多选；保持参数面板稳定不动 selected_node_id；
                    // 同步：若被取消的正是 selected_node_id 自身则清空它，使参数面板隐藏
                    if let Some(pos_idx) = tab.selected_node_ids.iter().position(|id| id == &node_id) {
                        tab.selected_node_ids.remove(pos_idx);
                        if tab.selected_node_id.as_deref() == Some(&node_id) {
                            tab.selected_node_id = None;
                        }
                    } else {
                        tab.selected_node_ids.push(node_id.clone());
                        // 若当前无主选中节点，则把它升格为 selected_node_id 便于参数面板展示
                        if tab.selected_node_id.is_none() {
                            tab.selected_node_id = Some(node_id);
                        }
                    }
                    tab.dragging_node_id = None;
                    // 不触发平移锚点，但也不开始拖拽：直接 return
                    state.canvas_pan_anchor = None;
                    return;
                }
                // 普通 Click：清空多选，进入单选 + 拖拽
                tab.selected_node_ids.clear();
                tab.selected_node_id = Some(node_id.clone());
                tab.dragging_node_id = Some(node_id);
                tab.drag_offset =
                    Vec2::new(world.x - node_pos.x, world.y - node_pos.y);
                hit_node = true;
            }
            None => {
                // 点击空白：清空所有选中（含多选列表）
                tab.selected_node_id = None;
                tab.selected_node_ids.clear();
                tab.dragging_node_id = None;
            }
        }
    }

    // UiState 级别的平移锚点（与 tab.dragging_node_id 互斥）
    if hit_node {
        state.canvas_pan_anchor = None;
    } else if ctrl_pressed {
        // Ctrl+Click 空白：不开始平移，避免误拖动画布
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
    use mining_app::mining::dag::Edge;

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
    use mining_app::mining::dag::{get_all_operator_types, Node};

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

// ===== 多选对齐 =====
//
// 居上 / 居左对齐共用同一段"快照压栈 + 沿单轴重写位置"的逻辑，
// 仅维度（y / x）与取值方向（min）不同。撤销快照按字段约定压入
// `DagTab::node_position_history`，容量上限 50，超出丢弃最旧。

/// 把当前所有节点的 (id, position) 快照压入撤销栈，超出容量上限 50 时丢弃最旧。
fn push_node_position_snapshot(tab: &mut DagTab) {
    let snapshot: Vec<(String, Vec2)> = tab
        .graph
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.position))
        .collect();
    const HISTORY_CAP: usize = 50;
    tab.node_position_history.push(snapshot);
    while tab.node_position_history.len() > HISTORY_CAP {
        tab.node_position_history.remove(0);
    }
}

/// 多选对齐通用实现：对 `selected_node_ids` 中的所有节点，
/// 把 `pick_dim(position)` 字段统一改为这些节点的最小值；其他维度保持不变。
///
/// - 居上对齐：`pick_dim = |p| p.y`，把所有节点 y 改为最小 y → 顶端对齐
/// - 居左对齐：`pick_dim = |p| p.x`，把所有节点 x 改为最小 x → 左端对齐
///
/// 同时把 `new_pos` 回填到对应节点的 position 字段。
fn align_selected<F, G>(
    state: &mut UiState,
    pick_dim: F,
    set_dim: G,
    label: &str,
) where
    F: Fn(Vec2) -> f32,
    G: Fn(Vec2, f32) -> Vec2,
{
    let Some(tab) = state.dag_editor.active_tab_mut() else { return; };
    if tab.selected_node_ids.len() < 2 {
        tab.add_action_log(
            "至少选中 2 个算子再执行对齐".to_string(),
            LogLevel::Warning,
        );
        return;
    }

    // 收集选中节点的当前位置（仅取存在节点）
    let mut coords: Vec<(String, f32)> = Vec::with_capacity(tab.selected_node_ids.len());
    for id in &tab.selected_node_ids {
        if let Some(n) = tab.graph.get_node(id) {
            coords.push((id.clone(), pick_dim(n.position)));
        }
    }
    if coords.len() < 2 {
        tab.add_action_log(
            "可对齐的选中算子不足 2 个".to_string(),
            LogLevel::Warning,
        );
        return;
    }
    let target = coords.iter().map(|(_, v)| *v).fold(f32::INFINITY, f32::min);

    // 压栈快照（用于将来撤销）
    push_node_position_snapshot(tab);

    // 应用对齐
    for (id, _) in &coords {
        if let Some(n) = tab.graph.get_node_mut(id) {
            n.position = set_dim(n.position, target);
        }
    }
    tab.dirty = true;
    tab.add_action_log(
        format!("已{}对齐 {} 个算子", label, coords.len()),
        LogLevel::Info,
    );
}

/// 居上对齐：所有选中节点的 y 统一为最小值。
fn handle_align_top(state: &mut UiState) {
    align_selected(
        state,
        |p| p.y,
        |p, v| Vec2::new(p.x, v),
        "居上",
    );
}

/// 居左对齐：所有选中节点的 x 统一为最小值。
fn handle_align_left(state: &mut UiState) {
    align_selected(
        state,
        |p| p.x,
        |p, v| Vec2::new(v, p.y),
        "居左",
    );
}
