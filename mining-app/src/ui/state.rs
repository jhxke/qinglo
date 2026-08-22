use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::SystemTime;
use iced::Rectangle;
use iced::window::Id as WindowId;
use crate::geom::Vec2;
use operator_executor_client::protocol::{DagExecutionResult, DagNodeResult};
use operator_executor_client::PortData;
use operator_executor_client::runtime_client::DebugNodeMeta;
use crate::dag::{DagGraph, OperatorType, NodeIORegistry};
use crate::dag_store::{self, DagModelMeta, DagModelRecord};

use crate::debug_executor::DebugDiagnostics;

/// 后台 DAG 执行任务的工作线程 → UI 线程消息。
///
/// `Log` 携带阶段性日志（如「已将流程下发到服务端」），UI 线程立即追加到日志面板；
/// `NodeProgress` 携带服务端推送的单节点进度（Executing/Completed/Failed），UI 线程
/// 据此立即回填 `io_registry` 并重绘画布，实现「运行到哪个算子」的实时可视化；
/// `StreamChunk` 携带流式节点的实时 chunk（如 chat DSL 快照），UI 线程落盘预览缓存
/// 供聊天预览窗口逐 token 刷新（打字机效果）；
/// `Finished` 携带最终执行结果，UI 线程做收尾（去重回填未收到进度的节点 + 汇总日志）。
pub enum DagExecMessage {
    Log(String, LogLevel),
    NodeProgress(DagNodeResult),
    /// 流式 chunk：node_id + chunk 数据 + chunk 序号
    StreamChunk {
        node_id: String,
        chunk: PortData,
    },
    Finished(Result<DagExecutionResult, String>),
}

/// 后台任务类型，决定 `Finished` 到达时如何回填 registry。
#[derive(Clone)]
pub enum DagExecKind {
    /// 「执行 DAG」按钮：执行整张图，遍历所有节点逐一日志。
    RunAll,
    /// 右键「运行到此结点」：执行目标节点的上游子图。
    RunUpTo { target_node_id: String },
}

/// 一个正在运行的后台 DAG 执行任务。
///
/// `kind` 在 spawn 时确定、执行期间不变，供 UI 线程在收到 `Finished` 时分派回填逻辑；
/// `model_id` 标记任务归属哪个建模 tab，即便用户在执行期间切换到别的 tab，结果也能
/// 正确回填到发起任务的那个 tab；`rx` 接收工作线程发来的 `DagExecMessage`。
pub struct DagExecTask {
    pub kind: DagExecKind,
    pub rx: Receiver<DagExecMessage>,
    pub model_id: String,
}

/// 运行日志条目
#[derive(Clone)]
pub struct RunLogEntry {
    pub timestamp: String,
    pub message: String,
    pub level: LogLevel,
}

#[derive(Clone, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// 运行日志分类：底部「运行日志」面板按此划分三个子标签页分别展示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    /// 提醒日志：用户点击保存、验证、清空等 UI 操作的直接反馈。
    Action,
    /// 算子运行日志：服务端 DAG 执行进度与节点结果回填日志。
    Runtime,
    /// 通信报文：客户端与服务端交互的 JSON 请求 / 响应原文。
    Json,
}

/// 左侧合并面板的子标签页：建模列表与算子面板合并到同一侧栏，通过 tab 切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftPanelTab {
    /// 建模列表：磁盘建模历史 + 新建/重命名/删除入口。
    Models,
    /// 算子面板：算子分类树 + 搜索 + 节点参数编辑。
    Operators,
}

impl Default for LeftPanelTab {
    /// 默认聚焦「建模列表」：进入视图后通常先选择/新建一个建模。
    fn default() -> Self {
        LeftPanelTab::Models
    }
}

impl Default for LogCategory {
    /// 默认聚焦「算子运行」子页：执行场景下信息量最大。
    fn default() -> Self {
        LogCategory::Runtime
    }
}

/// JSON 通信报文方向，用于在「通信报文」子页区分请求与响应。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JsonDirection {
    /// 客户端 → 服务端
    Send,
    /// 服务端 → 客户端
    Receive,
}

/// JSON 通信日志条目：保存一次请求或响应的报文原文，便于排查协议问题。
#[derive(Clone)]
pub struct JsonLogEntry {
    pub timestamp: String,
    pub direction: JsonDirection,
    /// 报文标题（如「下发 DAG 执行请求」「DAG 执行结果」）
    pub title: String,
    /// 美化后的 JSON 原文（序列化失败时回退为原始字符串）
    pub payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewType {
    MiningAnalysis,
    Settings,
}

/// Iced Elm 架构的全局消息。
///
/// 从 egui 迁移到 Iced 后，所有 UI 事件（按钮点击、输入框编辑、画布交互、
/// 后台任务回填等）都通过 `Message` 派发，由 `MyApp::update` 集中处理。
///
/// 阶段 1 骨架只保留最小集：视图切换 + 后台轮询 Tick。
/// 后续阶段会按需扩展（算子面板、参数编辑、画布交互、预览窗口等）。
#[derive(Debug, Clone)]
pub enum Message {
    /// 切换主视图（活动栏按钮点击）。
    SwitchView(ViewType),
    /// 空闲/动画 Tick：用于推进 Logo 动画、轮询后台 DAG 任务等。
    /// 由 Iced subscription 按 500ms 间隔发出，空闲时也可保持低频刷新。
    Tick,
    /// 运行动画 Tick：DAG 执行中由 subscription 按 ~80ms 高频发出，
    /// 推进 `anim_time` 并及时 poll 执行任务回填节点状态，让画布呈现
    /// 节点呼吸 / 边数据流动等动态效果。无执行任务时不发出。
    AnimTick,
    /// 关闭窗口请求。
    WindowClose,
    /// 最大化/还原窗口。
    WindowToggleMaximize,
    /// 最小化窗口。
    WindowMinimize,
    /// 用户在自定义标题栏拖拽区按下鼠标左键，请求开始窗口拖拽。
    /// 由 `iced::widget::mouse_area` 的 on_press 触发，update 中调用
    /// `iced::window::drag()` 交给 winit 处理。
    WindowDrag,
    /// boot 阶段异步获取的主窗口 Id 回填。
    /// iced 0.14 中 `iced::window::Id` 由内部 `Id::unique()` 创建，
    /// 用户无法直接构造，只能通过 `iced::window::oldest()` 异步查询。
    /// boot 时返回该 Task，resolve 后通过此消息把 Id 落到 UiState。
    SetMainWindowId(Option<WindowId>),
    /// 画布鼠标按下：参数为鼠标相对于画布左上角的屏幕坐标（已扣除画布偏移）。
    /// 由 `DagProgram::update` 在 `ButtonPressed(Left)` 时发布，`MyApp::update`
    /// 据此判断是命中节点（选中 + 开始拖拽）还是命中空白（开始平移画布）。
    CanvasPress(Vec2),
    /// 画布鼠标释放：参数同 `CanvasPress`。
    /// 由 `DagProgram::update` 在 `ButtonReleased(Left)` 时发布，`MyApp::update`
    /// 据此清空 `dragging_node_id` 与 `canvas_pan_anchor`。
    CanvasRelease(Vec2),
    /// 画布鼠标移动：参数为鼠标相对于画布左上角的屏幕坐标。
    /// 鼠标在画布上方时每帧移动都会发布。`MyApp::update` 据此：
    /// - 若 `dragging_node_id` 非空：更新节点位置 = 世界坐标 - drag_offset
    /// - 若 `canvas_pan_anchor` 非空：更新 canvas_offset = anchor_offset + (pos - anchor_pos)
    CanvasMove(Vec2),
    /// 画布滚轮缩放：`delta_y` 为滚轮纵向滚动量（正向上/负向下），
    /// `pos` 为鼠标在画布内的相对坐标（缩放锚点）。
    /// `MyApp::update` 据此调整 `canvas_zoom`，并以 `pos` 为锚点调整 `canvas_offset`
    /// 使鼠标位置对应的世界坐标在缩放前后保持不变。
    CanvasWheel { delta_y: f32, pos: Vec2 },

    // ===== 建模列表 sidebar =====

    /// 点击建模列表项：打开（或切换到已打开的）指定 id 的建模。
    OpenModel(String),
    /// 点击「+ 新建模」按钮：弹出新建建模对话框。
    NewModelClick,
    /// 新建模对话框名字输入框内容变化。
    NewModelNameInput(String),
    /// 新建模对话框：确认（回车 / 确认按钮）。
    NewModelConfirm,
    /// 新建模对话框：取消（Esc / 取消按钮 / 点击遮罩）。
    NewModelCancel,
    /// 点击列表项右键「重命名」（或重命名图标）：弹出重命名对话框。
    /// 携带目标建模 id。
    RenameModelClick(String),
    /// 重命名对话框输入框内容变化。
    RenameInput(String),
    /// 重命名对话框：确认。
    RenameConfirm,
    /// 重命名对话框：取消。
    RenameCancel,
    /// 点击列表项右键「删除」（或删除图标）：弹出删除确认对话框。
    /// 携带 (id, name)。
    DeleteModelClick(String, String),
    /// 删除确认对话框：确认删除。
    DeleteModelConfirm,
    /// 删除确认对话框：取消。
    DeleteModelCancel,

    // ===== Tab 栏 =====

    /// 点击 Tab：切换到指定索引的 tab。
    SwitchTab(usize),
    /// 点击 Tab 关闭按钮 ×：关闭指定索引的 tab。
    CloseTab(usize),
    /// 鼠标悬停在某个 tab 上：用于追踪 hover 状态以显示/隐藏关闭按钮。
    /// 参数为 tab 索引，None 表示鼠标移出所有 tab。
    TabHover(Option<usize>),

    // ===== 工具栏 =====

    /// 点击「保存」按钮：保存当前激活 tab 的 graph 到磁盘。
    SaveTab,
    /// 点击「执行 DAG」按钮：置 `pending_run_all=true`，由外层 Tick 轮询后 spawn。
    /// 阶段 3 暂只记日志 + 置标志，实际 spawn 留待接入 operator_executor 后补。
    RunAllClick,
    /// 点击「调试」按钮：切换当前 tab 的 `debug_mode`。
    ToggleDebug,
    /// 点击「清空日志」按钮：清空当前激活分类的日志。
    ClearLogs,

    // ===== 日志面板 =====

    /// 点击日志子标签：切换 `active_log_category`。
    SwitchLogCategory(LogCategory),
    /// 切换日志面板显示/隐藏（点击隐藏按钮或重新展开按钮）。
    ToggleLogPanel,

    // ===== 左侧合并面板 tab 切换 =====

    /// 点击左侧面板顶部 tab：在「建模列表」与「算子面板」之间切换。
    SwitchLeftPanel(LeftPanelTab),

    // ===== 画布 / DAG 编辑交互 =====

    /// 画布右键点击：`screen_pos` 是相对画布左上角的屏幕坐标。
    /// 接收后先在 `MyApp::update` 中做命中测试：命中节点 → 打开节点右键菜单（运行到此节点/删除节点/...）；
    /// 命中空白 → 打开画布右键菜单（加节点/重置视图）。
    CanvasRightClick(Vec2),
    /// 关闭当前打开的画布右键菜单（或节点右键菜单）。
    ContextMenuClose,

    /// 点击「运行到此节点」菜单项：参数为节点 id。
    RunUpToNode(String),
    /// 点击「删除节点」菜单项：参数为节点 id。
    DeleteNodeClick(String),

    /// 端口命中 & 连线创建的起点：(node_id, port_index, is_output=true)。
    /// 发布时机：`CanvasPress` 先在 `DagProgram::update` 层面做端口命中测试，
    /// 若命中输出端口则发布本消息，`MyApp::update` 据此把 `connecting_from` 写入当前 tab。
    ConnectStart { node_id: String, port_index: usize, is_output: bool },
    /// 连线创建过程中鼠标移动：更新临时贝塞尔线的终点。
    /// 由 `CanvasMove` 的处理分支在 `connecting_from` 非空时直接写 `connecting_drag_world`。
    /// （无需独立消息变体，复用 `CanvasMove` 即可。）
    /// 连线创建过程中鼠标移动时屏幕坐标转世界坐标更新临时贝塞尔线终点。
    /// 参数是相对画布左上角的屏幕坐标（与 CanvasMove 语义一致），在 MyApp::update
    /// 中再次转世界坐标写入 DagTab.connecting_drag_world。
    ConnectDrag(Vec2),
    /// 连线创建的终点：若 release 时命中有效输入端口则创建边；否则取消。
    /// 参数同 CanvasRelease（屏幕坐标释放位置）；在 MyApp::update 中再次用
    /// screen_to_world + hit_test_input_port 做命中判定，若成功则 graph.add_edge。
    ConnectRelease(Vec2),

    // ===== 算子面板 =====

    /// 算子面板顶部搜索框内容变化：用于过滤分类/算子卡片。
    OperatorSearchInput(String),
    /// 点击算子卡片（或双击）：把对应算子作为新节点添加到激活 tab 的图上。
    /// `operator_name` 用于匹配分类树；添加位置优先取 `pending_add_operator_world`
    /// （画布右键菜单项设置的世界坐标），否则取画布视口中心。
    AddOperator(String),

    // ===== 节点参数面板 =====

    /// 参数面板中某个参数输入框内容变化。参数为 (node_id, param_name, new_value)。
    ParamInput(String, String, String),
}

/// 自定义算子编辑器的 Debug 面板状态。
///
/// `input_text` 是 Debug 输入框中的文本 (逗号分隔数字、分号分隔多路输入)；
/// `diagnostics` 是最近一次 Debug 运行的诊断结果，`Option` 表示尚未运行过。
#[derive(Clone, Default)]
pub struct CustomOperatorDebugState {
    pub input_text: String,
    pub diagnostics: Option<DebugDiagnostics>,
}

pub struct UiState {
    pub current_view: ViewType,
    pub dag_editor: DagEditorState,
    pub settings: SettingsState,
    /// Logo 动画的累积时间（秒）。每 Tick (500ms) 推进 0.5。
    /// 标题栏 Canvas Program 据此计算折线最末点的呼吸 / 上升动画偏移。
    pub logo_time: f32,
    /// DAG 运行动画的累积时间（秒）。执行中由 `AnimTick`（~80ms）推进，
    /// 驱动画布节点呼吸发光 / 边数据流动光点等动态效果。
    /// 无执行任务时不推进，画布保持静态以降低 GPU 开销。
    pub anim_time: f32,
    /// 主窗口 Id。由 boot 阶段 `iced::window::oldest()` 异步查询得到，
    /// 通过 `SetMainWindowId` 消息回填。窗口控制按钮（关闭/最大化/最小化/
    /// 拖拽）点击后，update 中调用 `iced::window::xxx(id)` 时使用此 Id。
    /// None 表示尚未获取到（极短暂：boot 完成后立即 resolve）。
    pub main_window_id: Option<WindowId>,
    /// 画布平移锚点：左键按下空白处时记录 `(按下时的鼠标屏幕坐标, 按下时的 canvas_offset)`。
    /// 鼠标移动时据此计算 `canvas_offset = anchor_offset + (pos - anchor_pos)`。
    /// 左键释放或切换 tab 时清空。与 `DagTab.dragging_node_id` 互斥：
    /// 同一时刻只有一个为非空（拖节点或平移画布）。
    pub canvas_pan_anchor: Option<(Vec2, Vec2)>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            current_view: ViewType::MiningAnalysis,
            dag_editor: DagEditorState::default(),
            settings: SettingsState::default(),
            logo_time: 0.0,
            anim_time: 0.0,
            main_window_id: None,
            canvas_pan_anchor: None,
        }
    }
}

/// 系统设置视图的本地状态。
///
/// `rust_path_input` 是 Rust 工具链路径文本框中的内容（可能尚未保存）；
/// `compile_dir_input` 是编译目录文本框中的内容（可能尚未保存）；
/// `initialized` 用于首次进入设置页时从磁盘配置懒加载输入框内容；
/// `last_result` 记录最近一次「测试 / 保存 / 自动检测」操作的结果。
#[derive(Clone)]
pub struct SettingsState {
    pub rust_path_input: String,
    pub compile_dir_input: String,
    pub initialized: bool,
    pub last_result: Option<(bool, String)>,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            rust_path_input: String::new(),
            compile_dir_input: String::new(),
            initialized: false,
            last_result: None,
        }
    }
}

/// Debug 模式下数据预览的分页查询状态。
///
/// 当 `DagTab.debug_mode` 为 true 且 `debug_session_id` 为 Some 时，数据预览窗口
/// 不再读本地缓存文件，而是通过此状态向服务端分页查询完整输出数据。
/// `meta` 在首次打开预览时从服务端查询一次；`cached_page` 仅缓存最近一次查询的页，
/// 切换端口/页码时重新查询。预览窗口关闭时整体清空（`debug_preview = None`）。
#[derive(Default)]
pub struct DebugPreviewState {
    /// 当前预览的节点 ID
    pub node_id: String,
    /// 服务端返回的节点输出元信息（None 表示尚未查询或查询失败）
    pub meta: Option<DebugNodeMeta>,
    /// 当前选中的输出端口索引
    pub current_port_idx: usize,
    /// 各端口当前页码（key = port_idx, value = page_idx）
    pub current_pages: HashMap<usize, usize>,
    /// 缓存的当前页数据：(port_idx, page_idx, data)
    /// 仅缓存最近一次查询的页，切换页时重新查询服务端
    pub cached_page: Option<(usize, usize, Option<PortData>)>,
    /// 错误信息（查询失败时展示）
    pub error: Option<String>,
}

/// 一个打开的 DAG 标签页：持有「每图编辑状态」。
///
/// 多 tab 编辑器中，每个 tab 对应一张独立的 [`DagGraph`] 及其交互状态（选中节点、
/// 画布偏移、I/O 注册表、运行日志等）。`model_id` 关联磁盘建模文件；`dirty` 标记
/// 是否有未落盘的修改；`pending_run_*` 用于跨借用触发全局执行任务（画布/日志面板
/// 闭包内无法直接持有 `&mut DagEditorState`，改为写入标志、由外层统一处理）。
pub struct DagTab {
    pub model_id: String,
    pub name: String,
    pub graph: DagGraph,
    pub selected_node_id: Option<String>,
    /// 多选节点 ID 列表（Ctrl+Click 切换）。
    /// 与 `selected_node_id` 相互独立：单选仍走 `selected_node_id`（驱动参数面板等），
    /// 多选仅用于批量对齐等操作。普通 Click 会清空此列表回到单选语义。
    pub selected_node_ids: Vec<String>,
    /// 节点位置快照栈（用于撤销对齐操作）。
    /// 每次执行对齐前压入当前所有节点的 (id, position) 快照；
    /// 撤销时弹出栈顶恢复。容量上限 50，超出丢弃最旧。
    pub node_position_history: Vec<Vec<(String, Vec2)>>,
    /// 用户是否手动隐藏了右侧「算子运行参数」面板（点击面板标题栏 × 按钮后置 true；
    /// 选中不同节点时自动重置为 false，使新节点的参数重新展示）。
    pub hide_params_panel: bool,
    pub dragging_node_id: Option<String>,
    pub drag_offset: Vec2,
    /// 从算子面板拖拽中的算子类型; 在画布上释放时创建节点, 在画布外释放或状态失效时清空.
    pub dragging_operator: Option<OperatorType>,
    /// 上一帧画布的屏幕矩形, 供算子面板计算点击添加位置等用途 (每帧由画布刷新).
    pub canvas_viewport_rect: Option<Rectangle<f32>>,
    pub connecting_from: Option<(String, usize, bool)>,
    /// 连线创建拖拽中的终点（世界坐标，由 CanvasMove/ConnectDrag 更新）；
    /// `None` 表示使用当前鼠标位置（DagProgram.draw 拿不到光标，需由主循环传入）。
    pub connecting_drag_world: Option<Vec2>,
    pub canvas_offset: Vec2,
    pub canvas_zoom: f32,
    pub show_operator_panel: bool,
    pub error_message: Option<String>,
    pub operator_search_filter: String,
    /// 画布右键菜单（或节点右键菜单）的屏幕坐标（相对画布左上角）。
    /// None 表示未打开。`context_menu_node_id` 为 Some 时表示节点右键菜单，
    /// None 时表示画布空白右键菜单。
    pub context_menu_screen_pos: Option<Vec2>,
    /// 节点 I/O 全局注册表，集中管理所有节点的输入、输出、执行状态和缓存失效
    pub io_registry: NodeIORegistry,
    pub context_menu_node_id: Option<String>,
    /// 画布右键菜单「添加算子」暂存的世界坐标：AddOperator 消息处理时，
    /// 优先把新节点加到这个位置；缺失时加到画布视口中心。
    pub pending_add_operator_world: Option<Vec2>,
    /// 当前打开「数据预览」浮动窗口的节点 ID；为 None 时窗口关闭。
    pub preview_node_id: Option<String>,
    /// 当前打开「K线图预览」浮动窗口的节点 ID；为 None 时窗口关闭。
    /// 由算子右键菜单「K线图预览」触发，读取该节点预览缓存中的首个 String 输出
    /// （K线可视化算子返回的 DSL）并交由 kline_chart_view 解析渲染。
    pub kline_preview_node_id: Option<String>,
    /// 当前打开「折线图预览」浮动窗口的节点 ID；为 None 时窗口关闭。
    /// 由算子右键菜单「折线图预览」触发，读取该节点预览缓存中的首个 DataFrameArray
    /// 输出，并按节点参数（`date_col`/`close_col`）交由 line_chart_view 渲染折线图。
    pub line_chart_preview_node_id: Option<String>,
    /// 当前打开「聊天预览」浮动窗口的节点 ID；为 None 时窗口关闭。
    /// 由算子右键菜单「聊天预览」触发，读取该节点预览缓存中的首个 String 输出
    /// （DSL流式对话展示算子返回的 chat DSL）并交由 chat_view 解析渲染气泡界面。
    pub chat_preview_node_id: Option<String>,
    /// 当前打开「直方图预览」浮动窗口的节点 ID；为 None 时窗口关闭。
    /// 由算子右键菜单「直方图预览」触发，读取该节点预览缓存中的首个 DataFrame
    /// （直方图展示算子返回的 DataFrame）并按 x_col/y_col/left_col/right_col 渲染柱状图。
    pub histogram_preview_node_id: Option<String>,
    /// 自定义算子编辑器的 Debug 面板状态 (输入文本与最近一次诊断结果)
    pub custom_op_debug: CustomOperatorDebugState,
    /// 提醒日志：用户点击保存、验证、清空等 UI 操作的直接反馈。
    pub action_logs: Vec<RunLogEntry>,
    /// 算子运行日志：服务端 DAG 执行进度与节点结果回填日志。
    pub runtime_logs: Vec<RunLogEntry>,
    /// 通信报文日志：客户端与服务端交互的 JSON 请求 / 响应原文。
    pub json_logs: Vec<JsonLogEntry>,
    /// 底部「运行日志」面板当前激活的子标签页。
    pub active_log_category: LogCategory,
    /// 是否有未落盘的修改
    pub dirty: bool,
    /// 「执行 DAG」按钮待处理标志（外层轮询后触发 spawn_run_all）
    pub pending_run_all: bool,
    /// 「运行到此结点」右键菜单待处理标志（外层轮询后触发 spawn_run_up_to）
    pub pending_run_up_to: Option<String>,
    /// Debug 模式开关：开启后，下一次「执行 DAG」/「运行到此结点」会携带
    /// `debug_session_id` 下发到服务端，服务端保留各节点完整输出供分页查询。
    /// 数据预览窗口在 Debug 模式下不读缓存文件，而是直接向服务端查询分页数据。
    pub debug_mode: bool,
    /// 当前 Debug 会话 ID（UUID v4）。`Some` 表示服务端有对应会话数据可查；
    /// `None` 表示尚未在 Debug 模式下执行过，或会话已被 `EndDebugSession` 释放。
    /// 关闭预览窗口 / 关闭 Debug 模式 / 关闭 tab / 切换视图时必须清理并释放。
    pub debug_session_id: Option<String>,
    /// Debug 模式下数据预览的分页查询状态。预览窗口打开时初始化，关闭时清空。
    pub debug_preview: Option<DebugPreviewState>,
    /// 方向键连续移动选中节点的节流计时器: (首次按下时间, 上次移动时间)
    /// None 表示当前没有方向键被按住; 单击移动一步, 长按超过初始延迟后连续移动。
    pub arrow_move_timer: Option<(f64, f64)>,
}

impl DagTab {
    /// 新建一个空白 tab（用于新建建模）。
    pub fn new(model_id: String, name: String, graph: DagGraph) -> Self {
        Self {
            model_id,
            name,
            graph,
            selected_node_id: None,
            selected_node_ids: Vec::new(),
            node_position_history: Vec::new(),
            hide_params_panel: false,
            dragging_node_id: None,
            drag_offset: Vec2::ZERO,
            dragging_operator: None,
            canvas_viewport_rect: None,
            connecting_from: None,
            connecting_drag_world: None,
            canvas_offset: Vec2::ZERO,
            canvas_zoom: 1.0,
            show_operator_panel: true,
            error_message: None,
            operator_search_filter: String::new(),
            context_menu_screen_pos: None,
            io_registry: NodeIORegistry::new(),
            context_menu_node_id: None,
            pending_add_operator_world: None,
            preview_node_id: None,
            kline_preview_node_id: None,
            line_chart_preview_node_id: None,
            chat_preview_node_id: None,
            histogram_preview_node_id: None,
            custom_op_debug: CustomOperatorDebugState {
                input_text: "1, 2, 3, 4, 5".to_string(),
                diagnostics: None,
            },
            action_logs: Vec::new(),
            runtime_logs: Vec::new(),
            json_logs: Vec::new(),
            active_log_category: LogCategory::default(),
            dirty: false,
            pending_run_all: false,
            pending_run_up_to: None,
            debug_mode: false,
            debug_session_id: None,
            debug_preview: None,
            arrow_move_timer: None,
        }
    }

    /// 从磁盘加载的记录构造 tab。
    pub fn from_record(record: DagModelRecord) -> Self {
        Self::new(record.id, record.name, record.graph)
    }

    /// 追加一条提醒日志（用户点击保存、验证、清空等 UI 操作的直接反馈）。
    pub fn add_action_log(&mut self, message: String, level: LogLevel) {
        push_capped(&mut self.action_logs, RunLogEntry {
            timestamp: format_now_timestamp(),
            message,
            level,
        });
    }

    /// 追加一条算子运行日志（服务端 DAG 执行进度与节点结果回填）。
    pub fn add_runtime_log(&mut self, message: String, level: LogLevel) {
        push_capped(&mut self.runtime_logs, RunLogEntry {
            timestamp: format_now_timestamp(),
            message,
            level,
        });
    }

    /// 追加一条 JSON 通信报文日志（客户端 ↔ 服务端的请求 / 响应原文）。
    pub fn add_json_log(&mut self, direction: JsonDirection, title: String, payload: String) {
        push_capped(&mut self.json_logs, JsonLogEntry {
            timestamp: format_now_timestamp(),
            direction,
            title,
            payload,
        });
    }

    /// 清空当前激活分类的日志。
    pub fn clear_active_logs(&mut self) {
        match self.active_log_category {
            LogCategory::Action => self.action_logs.clear(),
            LogCategory::Runtime => self.runtime_logs.clear(),
            LogCategory::Json => self.json_logs.clear(),
        }
    }

    /// 清空全部三类日志。
    pub fn clear_all_logs(&mut self) {
        self.action_logs.clear();
        self.runtime_logs.clear();
        self.json_logs.clear();
    }
}

/// 把 `entry` 追加到 `buf` 末尾，并限制总长不超过 `CAP`，超出时丢弃最旧条目。
///
/// 三类日志共用同一上限策略，避免某类日志无限增长挤占内存。
fn push_capped<T>(buf: &mut Vec<T>, entry: T) {
    const CAP: usize = 1000;
    buf.push(entry);
    while buf.len() > CAP {
        buf.remove(0);
    }
}

/// 挖掘分析视图的全局编辑器状态：管理多个打开的 tab、磁盘建模历史列表、
/// 新建/重命名对话框状态，以及全局后台执行任务。
///
/// 注意：本结构不 `derive(Clone)`，因为 `dag_exec_task` 中的 `mpsc::Receiver` 非 `Clone`。
/// 全代码库仅以 `&mut` 传递，不需要克隆。
pub struct DagEditorState {
    /// 当前打开的 tab 列表（每个 tab 一张独立 DAG）
    pub tabs: Vec<DagTab>,
    /// 当前激活的 tab 索引；None 表示无打开的 tab
    pub active_tab_index: Option<usize>,
    /// 当前鼠标悬停的 tab 索引；None 表示无 hover
    pub hovered_tab: Option<usize>,
    /// 磁盘上的建模历史元数据列表（懒加载，首次进入挖掘分析视图时填充）
    pub models: Vec<DagModelMeta>,
    /// models 是否已从磁盘加载
    pub models_loaded: bool,
    /// 左侧合并面板当前激活的子标签页（建模列表 / 算子面板）。
    /// 由 [`Message::SwitchLeftPanel`] 切换；新建模后自动跳到 Operators 以便添加算子。
    pub active_left_panel: LeftPanelTab,
    /// 新建模对话框：是否显示
    pub show_new_model_dialog: bool,
    /// 新建模对话框：名字输入框内容
    pub new_model_name_input: String,
    /// 重命名对话框：目标建模 id
    pub rename_target_id: Option<String>,
    /// 重命名对话框：名字输入框内容
    pub rename_input: String,
    /// 清空确认对话框：是否显示
    pub show_clear_confirm_dialog: bool,
    /// 删除建模确认对话框：是否显示
    pub show_delete_model_dialog: bool,
    /// 删除建模确认对话框：目标建模 id
    pub delete_model_target_id: Option<String>,
    /// 删除建模确认对话框：目标建模名称（用于对话框展示）
    pub delete_model_target_name: Option<String>,
    /// 后台 DAG 执行任务（「执行 DAG」或「运行到此结点」）；None 表示无任务运行。
    /// 由 UI 线程持有，工作线程仅通过 mpsc `Sender` 回传消息。
    pub dag_exec_task: Option<DagExecTask>,
    /// 底部日志面板是否展开显示；false 表示已折叠隐藏。
    pub log_panel_visible: bool,
}

impl Default for DagEditorState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active_tab_index: None,
            hovered_tab: None,
            models: Vec::new(),
            models_loaded: false,
            active_left_panel: LeftPanelTab::default(),
            show_new_model_dialog: false,
            new_model_name_input: String::new(),
            rename_target_id: None,
            rename_input: String::new(),
            show_clear_confirm_dialog: false,
            show_delete_model_dialog: false,
            delete_model_target_id: None,
            delete_model_target_name: None,
            dag_exec_task: None,
            log_panel_visible: true,
        }
    }
}

impl DagEditorState {
    /// 当前激活的 tab（只读）。
    pub fn active_tab(&self) -> Option<&DagTab> {
        self.active_tab_index.and_then(|i| self.tabs.get(i))
    }

    /// 当前激活的 tab（可变）。
    pub fn active_tab_mut(&mut self) -> Option<&mut DagTab> {
        self.active_tab_index.and_then(move |i| self.tabs.get_mut(i))
    }

    /// 按 model_id 查找 tab 索引。
    pub fn find_tab_by_model(&self, model_id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.model_id == model_id)
    }

    /// 保存当前激活 tab 的 graph 到磁盘，并清除 dirty 标记。
    pub fn save_active_tab(&mut self) {
        let Some(idx) = self.active_tab_index else { return; };
        let (id, name, graph) = {
            let tab = &self.tabs[idx];
            (tab.model_id.clone(), tab.name.clone(), tab.graph.clone())
        };
        dag_store::save_model(&id, &name, &graph);
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.dirty = false;
        }
    }

    /// 保存指定索引 tab 的 graph 到磁盘（用于关闭非激活 tab）。
    pub fn save_tab(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else { return; };
        let (id, name, graph) = (tab.model_id.clone(), tab.name.clone(), tab.graph.clone());
        dag_store::save_model(&id, &name, &graph);
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.dirty = false;
        }
    }

    /// 切换到指定索引的 tab，切换前保存当前 tab。
    pub fn switch_to_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.active_tab_index != Some(i) {
            self.save_active_tab();
        }
        self.active_tab_index = Some(i);
    }

    /// 关闭指定索引的 tab。关闭前保存；若关的是激活 tab，自动选中相邻 tab。
    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.save_tab(i);
        self.tabs.remove(i);
        self.active_tab_index = match self.active_tab_index {
            Some(a) if a == i => {
                if self.tabs.is_empty() {
                    None
                } else if i >= self.tabs.len() {
                    Some(self.tabs.len() - 1)
                } else {
                    Some(i)
                }
            }
            Some(a) if a > i => Some(a - 1),
            Some(a) => Some(a),
            None => None,
        };
    }

    /// 打开（或切换到已打开的）指定建模记录。
    pub fn open_model(&mut self, record: DagModelRecord) {
        if let Some(pos) = self.tabs.iter().position(|t| t.model_id == record.id) {
            self.switch_to_tab(pos);
            return;
        }
        self.save_active_tab();
        let tab = DagTab::from_record(record);
        self.tabs.push(tab);
        self.active_tab_index = Some(self.tabs.len() - 1);
    }

    /// 新建一个建模：生成 id、落盘空图、打开 tab、刷新历史列表。
    pub fn create_model(&mut self, name: &str) {
        let id = dag_store::new_model_id();
        let graph = DagGraph::new();
        dag_store::save_model(&id, name, &graph);
        self.save_active_tab();
        let tab = DagTab::new(id, name.to_string(), graph);
        self.tabs.push(tab);
        self.active_tab_index = Some(self.tabs.len() - 1);
        self.refresh_models();
    }

    /// 重命名磁盘建模 + 同步已打开 tab 的名字。
    pub fn rename_model(&mut self, id: &str, new_name: &str) {
        if let Some(rec) = dag_store::load_model(id) {
            dag_store::save_model(id, new_name, &rec.graph);
        }
        if let Some(pos) = self.tabs.iter().position(|t| t.model_id == id) {
            self.tabs[pos].name = new_name.to_string();
        }
        self.refresh_models();
    }

    /// 删除磁盘建模 + 关闭对应 tab。
    ///
    /// 实际为软删除：磁盘文件由 `<id>.json` 改名为 `<id>.deleted`，可手动恢复。
    ///
    /// 注意：关闭 tab 时**不调用** `close_tab`，因为 `close_tab` 内部会先 `save_tab`，
    /// 那会把刚重命名为 `.deleted` 的文件又重新写回为 `.json`，导致删除失效。
    pub fn delete_model(&mut self, id: &str) {
        dag_store::delete_model(id);
        if let Some(pos) = self.tabs.iter().position(|t| t.model_id == id) {
            // 手动移除 tab，跳过 save_tab（否则删除会被覆盖回写）
            self.tabs.remove(pos);
            self.active_tab_index = match self.active_tab_index {
                Some(a) if a == pos => {
                    if self.tabs.is_empty() {
                        None
                    } else if pos >= self.tabs.len() {
                        Some(self.tabs.len() - 1)
                    } else {
                        Some(pos)
                    }
                }
                Some(a) if a > pos => Some(a - 1),
                Some(a) => Some(a),
                None => None,
            };
        }
        self.refresh_models();
    }

    /// 弹出删除确认对话框，等待用户确认后再调用 [`delete_model`]。
    ///
    /// 列表项的删除图标与右键菜单均走此入口，避免误触直接删除。
    pub fn request_delete_model(&mut self, id: &str, name: &str) {
        self.delete_model_target_id = Some(id.to_string());
        self.delete_model_target_name = Some(name.to_string());
        self.show_delete_model_dialog = true;
    }

    /// 重新扫描磁盘建模列表。
    pub fn refresh_models(&mut self) {
        self.models = dag_store::list_models();
        self.models_loaded = true;
    }
}

/// 生成 UTC+8 格式的 `HH:MM:SS.mmm` 时间戳，供日志条目使用。
fn format_now_timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| {
            let seconds = d.as_secs();
            let nanos = d.subsec_nanos();
            let millis = nanos / 1_000_000;
            // UTC+8
            let total_seconds = seconds + 8 * 3600;
            let secs = total_seconds % 60;
            let mins = (total_seconds / 60) % 60;
            let hours = (total_seconds / 3600) % 24;
            format!("{:02}:{:02}:{:02}.{:03}", hours, mins, secs, millis)
        })
        .unwrap_or_else(|_| "00:00:00.000".to_string())
}
