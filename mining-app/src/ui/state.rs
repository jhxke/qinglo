use std::sync::mpsc::Receiver;
use std::time::SystemTime;
use egui::{Rect, Vec2};
use operator_executor_client::protocol::{DagExecutionResult, DagNodeResult};
use operator_executor_client::PortData;
use crate::dag::{DagGraph, OperatorType, NodeIORegistry, CustomOperatorDef};
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
    /// 提醒日志：用户点击保存、验证、清空等 UI 操作的直接反馈。
    Action,
    /// 算子运行日志：服务端 DAG 执行进度与节点结果回填日志。
    Runtime,
    /// 通信报文：客户端与服务端交互的 JSON 请求 / 响应原文。
    Json,
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
    OperatorDevelopment,
    Settings,
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

#[derive(Clone)]
pub struct OperatorDevelopmentState {
    pub current_operator: CustomOperatorDef,
    pub debug_state: CustomOperatorDebugState,
    pub error_message: Option<String>,
    pub run_logs: Vec<RunLogEntry>,
}

impl Default for OperatorDevelopmentState {
    fn default() -> Self {
        Self {
            current_operator: CustomOperatorDef::default(),
            debug_state: CustomOperatorDebugState {
                input_text: "1, 2, 3, 4, 5".to_string(),
                diagnostics: None,
            },
            error_message: None,
            run_logs: Vec::new(),
        }
    }
}

impl OperatorDevelopmentState {
    pub fn add_log(&mut self, message: String, level: LogLevel) {
        self.run_logs.push(RunLogEntry {
            timestamp: format_now_timestamp(),
            message,
            level,
        });
        if self.run_logs.len() > 1000 {
            self.run_logs.remove(0);
        }
    }

    pub fn clear_logs(&mut self) {
        self.run_logs.clear();
    }
}

pub struct UiState {
    pub current_view: ViewType,
    pub dag_editor: DagEditorState,
    pub operator_development: OperatorDevelopmentState,
    pub settings: SettingsState,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            current_view: ViewType::MiningAnalysis,
            dag_editor: DagEditorState::default(),
            operator_development: OperatorDevelopmentState::default(),
            settings: SettingsState::default(),
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

/// 单个打开的 DAG 标签页：持有「每图编辑状态」。
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
    /// 用户是否手动隐藏了右侧「算子运行参数」面板（点击面板标题栏 × 按钮后置 true；
    /// 选中不同节点时自动重置为 false，使新节点的参数重新展示）。
    pub hide_params_panel: bool,
    pub dragging_node_id: Option<String>,
    pub drag_offset: Vec2,
    /// 从算子面板拖拽中的算子类型; 在画布上释放时创建节点, 在画布外释放或状态失效时清空.
    pub dragging_operator: Option<OperatorType>,
    /// 上一帧画布的屏幕矩形, 供算子面板计算点击添加位置等用途 (每帧由画布刷新).
    pub canvas_viewport_rect: Option<Rect>,
    pub connecting_from: Option<(String, usize, bool)>,
    pub canvas_offset: Vec2,
    pub canvas_zoom: f32,
    pub show_operator_panel: bool,
    pub error_message: Option<String>,
    pub operator_search_filter: String,
    /// 节点 I/O 全局注册表，集中管理所有节点的输入、输出、执行状态和缓存失效
    pub io_registry: NodeIORegistry,
    pub context_menu_node_id: Option<String>,
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
}

impl DagTab {
    /// 新建一个空白 tab（用于新建建模）。
    pub fn new(model_id: String, name: String, graph: DagGraph) -> Self {
        Self {
            model_id,
            name,
            graph,
            selected_node_id: None,
            hide_params_panel: false,
            dragging_node_id: None,
            drag_offset: Vec2::ZERO,
            dragging_operator: None,
            canvas_viewport_rect: None,
            connecting_from: None,
            canvas_offset: Vec2::ZERO,
            canvas_zoom: 1.0,
            show_operator_panel: true,
            error_message: None,
            operator_search_filter: String::new(),
            io_registry: NodeIORegistry::new(),
            context_menu_node_id: None,
            preview_node_id: None,
            kline_preview_node_id: None,
            line_chart_preview_node_id: None,
            chat_preview_node_id: None,
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
    /// 磁盘上的建模历史元数据列表（懒加载，首次进入挖掘分析视图时填充）
    pub models: Vec<DagModelMeta>,
    /// models 是否已从磁盘加载
    pub models_loaded: bool,
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
}

impl Default for DagEditorState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active_tab_index: None,
            models: Vec::new(),
            models_loaded: false,
            show_new_model_dialog: false,
            new_model_name_input: String::new(),
            rename_target_id: None,
            rename_input: String::new(),
            show_clear_confirm_dialog: false,
            show_delete_model_dialog: false,
            delete_model_target_id: None,
            delete_model_target_name: None,
            dag_exec_task: None,
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
