use serde::{Deserialize, Serialize};
use chrono::{DateTime, Local};

use crate::PortData;

/// 请求 ID (用于匹配异步响应)
pub type RequestId = u64;

/// 算子执行状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperatorExecutionStatus {
    /// 未执行
    NotExecuted,
    /// 正在执行
    Executing,
    /// 已经完成（成功）
    Completed,
    /// 执行失败
    Failed,
    /// 状态已过期（依赖变更后需要重新执行）
    Stale,
}

impl OperatorExecutionStatus {
    pub fn to_str(&self) -> &str {
        match self {
            OperatorExecutionStatus::NotExecuted => "未执行",
            OperatorExecutionStatus::Executing => "正在执行",
            OperatorExecutionStatus::Completed => "已完成",
            OperatorExecutionStatus::Failed => "执行失败",
            OperatorExecutionStatus::Stale => "已过期",
        }
    }
}

impl Default for OperatorExecutionStatus {
    fn default() -> Self {
        OperatorExecutionStatus::NotExecuted
    }
}

/// 日志级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LogLevel {
    /// 调试信息
    Debug,
    /// 一般信息
    Info,
    /// 警告信息
    Warn,
    /// 错误信息
    Error,
}

impl LogLevel {
    pub fn to_str(&self) -> &str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

/// 单条执行日志记录
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionLogEntry {
    /// 日志级别
    pub level: LogLevel,
    /// 日志时间戳 (ISO 8601 格式的本地时间字符串)
    pub timestamp: String,
    /// 日志消息内容
    pub message: String,
}

impl ExecutionLogEntry {
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        let now: DateTime<Local> = Local::now();
        Self {
            level,
            timestamp: now.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            message: message.into(),
        }
    }

    pub fn debug(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Debug, message)
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Info, message)
    }

    pub fn warn(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Warn, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Error, message)
    }
}

/// 算子执行结果汇总（包含状态、日志和错误信息）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OperatorExecutionResult {
    /// 执行状态
    pub status: OperatorExecutionStatus,
    /// 执行日志列表（按时间顺序）
    pub logs: Vec<ExecutionLogEntry>,
    /// 执行耗时（毫秒）
    pub duration_ms: Option<u64>,
    /// 失败时的错误信息（与 status=Failed 配合使用）
    pub error_message: Option<String>,
}

impl OperatorExecutionResult {
    pub fn not_executed() -> Self {
        Self {
            status: OperatorExecutionStatus::NotExecuted,
            logs: Vec::new(),
            duration_ms: None,
            error_message: None,
        }
    }

    pub fn executing() -> Self {
        let mut result = Self::not_executed();
        result.status = OperatorExecutionStatus::Executing;
        result.logs.push(ExecutionLogEntry::info("开始执行算子"));
        result
    }

    pub fn completed(logs: Vec<ExecutionLogEntry>, duration_ms: u64) -> Self {
        let mut result = Self {
            status: OperatorExecutionStatus::Completed,
            logs,
            duration_ms: Some(duration_ms),
            error_message: None,
        };
        result.logs.push(ExecutionLogEntry::info(
            format!("算子执行成功，耗时 {} ms", duration_ms)
        ));
        result
    }

    pub fn failed(logs: Vec<ExecutionLogEntry>, error: impl Into<String>, duration_ms: Option<u64>) -> Self {
        let error_msg = error.into();
        let mut result = Self {
            status: OperatorExecutionStatus::Failed,
            logs,
            duration_ms,
            error_message: Some(error_msg.clone()),
        };
        // 与 completed() 保持一致：把执行耗时打印到运行日志，方便排查问题。
        // 算子真正执行过（duration_ms = Some）才输出耗时；
        // 执行前就失败的（如 DLL 未找到、输入端口缺失，传 None）不输出耗时。
        let msg = match duration_ms {
            Some(ms) => format!("算子执行失败 (耗时 {} ms): {}", ms, error_msg),
            None => format!("算子执行失败: {}", error_msg),
        };
        result.logs.push(ExecutionLogEntry::error(msg));
        result
    }

    pub fn append_log(&mut self, entry: ExecutionLogEntry) {
        self.logs.push(entry);
    }

    pub fn append_logs(&mut self, entries: impl IntoIterator<Item = ExecutionLogEntry>) {
        self.logs.extend(entries);
    }
}

/// 算子端口/参数定义（用于 JSON 配置文件）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorPortParamDef {
    pub name: String,
    pub direction: String,
    pub param_type: String,
    #[serde(default)]
    pub default_value: String,
}

/// 算子配置定义（从 operator.json 加载）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorConfig {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub code: String,
    pub color: [u8; 3],
    #[serde(default)]
    pub port_params: Vec<OperatorPortParamDef>,
    /// 摘要：一句话说明算子用途，展示在算子列表卡片上。
    #[serde(default)]
    pub summary: String,
    /// 详细描述（Markdown 格式）：包含用法、参数说明、示例等，
    /// 在算子运行参数面板中以阅读模式渲染，帮助用户快速上手。
    #[serde(default)]
    pub description_md: String,
    /// 是否为流式算子（导出 5 个流式 C ABI 符号，逐 chunk 产出数据）。
    /// `operator.json` 中缺省时为 `false`（批量算子）。
    #[serde(default)]
    pub stream: bool,
    /// 是否支持**动态新增输入端口**（如合并算子可按需扩展输入口数）。
    /// `operator.json` 中缺省时为 `false`（端口数固定）。
    /// 为 `true` 时客户端会在节点右键菜单显示「新增输入端口」、
    /// 在输入端口右键菜单显示「删除该输入端口」。
    #[serde(default)]
    pub dynamic_input_ports: bool,
}

/// 层级化算子目录项（文件夹）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorCategory {
    /// 分类名称（文件夹名）
    pub name: String,
    /// 分类下的算子列表
    pub operators: Vec<OperatorInfo>,
    /// 子分类（用于更深层级）
    #[serde(default)]
    pub subcategories: Vec<OperatorCategory>,
}

/// 单个算子的信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorInfo {
    /// 算子名称
    pub name: String,
    /// 算子描述
    pub description: String,
    /// DLL 文件路径
    pub dll_path: String,
    /// 算子颜色
    pub color: [u8; 3],
    /// 端口和参数定义
    pub port_params: Vec<OperatorPortParamDef>,
    /// 分类路径（层级路径）
    #[serde(default)]
    pub category_path: Vec<String>,
    /// 摘要：一句话说明算子用途，展示在算子列表卡片上。
    #[serde(default)]
    pub summary: String,
    /// 详细描述（Markdown 格式）：在算子运行参数面板中以阅读模式渲染。
    #[serde(default)]
    pub description_md: String,
    /// 是否为流式算子（客户端据此在 DAG 节点上设置 `DagNodeDef.stream`）。
    #[serde(default)]
    pub stream: bool,
    /// 是否支持**动态新增输入端口**（客户端据此在节点右键菜单显示
    /// 「新增输入端口」、在输入端口右键菜单显示「删除该输入端口」）。
    /// 由 `operator.json` 的 `dynamic_input_ports` 字段决定，缺省为 `false`。
    #[serde(default)]
    pub dynamic_input_ports: bool,
}

// ===== DAG 定义（可序列化，用于客户端→服务端的整体下发执行）=====

/// DAG 节点定义（服务端执行所需的全部信息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNodeDef {
    /// 节点唯一 ID（与客户端画布节点 ID 一致）
    pub id: String,
    /// 算子名称（用于在服务端查找 DLL）
    pub operator_name: String,
    /// 算子 DLL 路径（可选；为空时服务端按算子名在算子库目录中查找）
    #[serde(default)]
    pub dll_path: Option<String>,
    /// 输入端口数量
    pub input_count: usize,
    /// 输出端口数量
    pub output_count: usize,
    /// 参数 JSON（运行时传给算子的 params_json）
    #[serde(default)]
    pub params_json: String,
    /// 是否以流式方式执行（节点 DLL 必须导出 5 个流式 C ABI 符号，
    /// 否则服务端告警并降级为批量执行）。详见 `execute_operator_stream_*` 契约。
    #[serde(default)]
    pub stream: bool,
}

/// DAG 边定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagEdgeDef {
    pub source_node_id: String,
    pub source_port: usize,
    pub target_node_id: String,
    pub target_port: usize,
}

/// 完整的 DAG 定义（可序列化为文件，也可直接下发到服务端）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagDefinition {
    /// DAG 名称（用于落盘文件名和日志展示）
    pub name: String,
    /// 节点列表
    pub nodes: Vec<DagNodeDef>,
    /// 边列表（连接关系）
    pub edges: Vec<DagEdgeDef>,
}

/// 单个节点的执行结果（服务端回传）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNodeResult {
    pub node_id: String,
    pub operator_name: String,
    /// 节点各输出端口的数据（已截断为前 [`PREVIEW_ROW_LIMIT`] 行的预览）。
    ///
    /// 完整输出保留在服务端内存中供下游算子消费，不再经网络回传，
    /// 以避免大数据量导致响应超过帧大小上限。
    pub outputs: Vec<PortData>,
    /// 首个 DataFrame 输出的真实行数，用于客户端预览提示"原始 N 行，仅展示前 M 行"。
    #[serde(default)]
    pub output_row_count: usize,
    /// 执行结果（状态、日志、耗时、错误信息）
    pub execution_result: OperatorExecutionResult,
}

/// DAG 整体执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagExecutionResult {
    /// 整体状态（任一节点失败则为 Failed）
    pub status: OperatorExecutionStatus,
    /// 各节点执行结果（按执行顺序）
    pub node_results: Vec<DagNodeResult>,
    /// 总耗时（毫秒）
    pub total_duration_ms: u64,
    /// 失败时的错误信息
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Runtime 请求枚举
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuntimeRequest {
    /// 健康检查
    Ping,
    /// 关闭 runtime 服务
    Shutdown,
    /// 预加载算子到 runtime
    LoadOperator {
        /// 算子唯一标识 (通常使用 sanitize 后的算法名)
        operator_id: String,
        /// DLL 文件路径
        dll_path: String,
    },
    /// 卸载算子
    UnloadOperator {
        operator_id: String,
    },
    /// 执行已加载的算子节点
    ExecuteNode {
        operator_id: String,
        inputs: Vec<PortData>,
        max_outputs: usize,
        params_json: String,
    },
    /// 直接指定 DLL 路径执行（不预加载）
    ExecuteDll {
        dll_path: String,
        inputs: Vec<PortData>,
        max_outputs: usize,
        params_json: String,
    },
    /// 编译算子代码并执行
    CompileAndExecute {
        code: String,
        algorithm_name: String,
        inputs: Vec<PortData>,
        max_outputs: usize,
        params_json: String,
    },
    /// 查询可用算子列表（层级结构）
    ListOperators,
    /// 下发完整 DAG 到服务端，由服务端解析拓扑并按序执行
    ExecuteDag {
        /// 完整的 DAG 定义
        dag: DagDefinition,
    },
    /// 独立单节点流式执行（无需组装完整 DAG）。
    ///
    /// 服务端对算子 DLL 调用流式 C ABI（`stream_start` → 循环 `stream_next`），
    /// 每产出一个 chunk 推送一条 [`RuntimeResponse::StreamChunk`]，
    /// 流结束后推送一条 [`RuntimeResponse::StreamCompleted`] 终止帧。
    /// 算子必须导出 5 个流式符号，否则返回 [`RuntimeResponse::Error`]。
    ExecuteStream {
        /// 算子唯一标识（需已 LoadOperator，或在算子库目录中按名可查）
        operator_id: String,
        /// 物化输入（非流式）
        inputs: Vec<PortData>,
        /// 最大输出端口数（流式场景通常为 1）
        max_outputs: usize,
        /// 参数 JSON
        params_json: String,
    },
    /// 查询指定算子的执行状态
    QueryExecutionStatus {
        /// 算子或节点的唯一标识
        id: String,
    },
    /// 查询指定算子的执行日志
    QueryExecutionLogs {
        /// 算子或节点的唯一标识
        id: String,
        /// 可选：从第 N 条日志开始返回（用于分页），不传则返回全部
        start_index: Option<usize>,
        /// 可选：最大返回日志条数，不传则不限制
        max_count: Option<usize>,
    },
}

/// Runtime 响应枚举
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuntimeResponse {
    /// 通用成功
    Ok {
        request_id: RequestId,
    },
    /// 通用错误
    Error {
        request_id: RequestId,
        message: String,
    },
    /// 健康检查响应
    Pong {
        request_id: RequestId,
        version: String,
    },
    /// 执行节点成功（包含执行状态和日志）
    NodeExecuted {
        request_id: RequestId,
        outputs: Vec<PortData>,
        /// DataFrame 输出的真实行数（DataFrameArray 取各 DataFrame 中最大行数），用于客户端预览提示
        #[serde(default)]
        output_row_count: usize,
        /// 执行结果（状态、日志、耗时、错误信息）
        execution_result: OperatorExecutionResult,
    },
    /// 编译并执行成功（包含执行状态和日志）
    CompiledAndExecuted {
        request_id: RequestId,
        outputs: Vec<PortData>,
        /// DataFrame 输出的真实行数（DataFrameArray 取各 DataFrame 中最大行数），用于客户端预览提示
        #[serde(default)]
        output_row_count: usize,
        dll_path: Option<String>,
        /// 执行结果（状态、日志、耗时、错误信息）
        execution_result: OperatorExecutionResult,
    },
    /// 加载算子成功
    OperatorLoaded {
        request_id: RequestId,
    },
    /// 算子列表（层级结构）
    OperatorsList {
        request_id: RequestId,
        categories: Vec<OperatorCategory>,
    },
    /// DAG 整体执行结果响应（终止帧，一次 ExecuteDag 请求最后推送一条）
    DagExecuted {
        request_id: RequestId,
        result: DagExecutionResult,
    },
    /// DAG 执行过程中的单节点进度推送。
    ///
    /// 服务端在「开始执行某节点」和「某节点执行结束」时各推送一条，同一次
    /// `ExecuteDag` 请求会推送 0..N 条 `DagNodeProgress`，最后再推送一条
    /// [`RuntimeResponse::DagExecuted`] 终止帧。`request_id` 与终止帧一致。
    ///
    /// `progress.execution_result.status` 区分阶段：
    /// - `Executing`: outputs 为空，仅用于前端展示「运行中」高亮
    /// - `Completed`: outputs 为前 [`PREVIEW_ROW_LIMIT`] 行预览，前端落盘预览缓存 + set_result
    /// - `Failed`:    outputs 为空，`error_message` 携带错误信息
    DagNodeProgress {
        request_id: RequestId,
        progress: DagNodeResult,
    },
    /// 流式 chunk 推送（DAG 内流式节点与独立 [`RuntimeRequest::ExecuteStream`] 共用）。
    ///
    /// 一次 `ExecuteDag`（含流式节点）或 `ExecuteStream` 请求会推送 0..N 条 `StreamChunk`，
    /// 最后再推送一条终止帧（DAG 为 [`RuntimeResponse::DagExecuted`]，独立流式为
    /// [`RuntimeResponse::StreamCompleted`]）。`request_id` 与终止帧一致。
    ///
    /// `chunk` 通常为 `PortData::String`（如 LLM token），但也允许其他类型。
    /// `is_final` 在 v1 中恒为 `false`——节点/请求的结束由终止帧表示，字段保留以便未来无损升级。
    StreamChunk {
        request_id: RequestId,
        /// 产出该 chunk 的节点 ID（独立 ExecuteStream 时为 operator_id）
        node_id: String,
        /// chunk 数据
        chunk: PortData,
        /// 该节点/请求的 chunk 序号（从 0 起）
        chunk_index: u64,
        /// v1 恒 false
        is_final: bool,
    },
    /// 独立 [`RuntimeRequest::ExecuteStream`] 的终止帧（一次请求最后推送一条）。
    StreamCompleted {
        request_id: RequestId,
        /// 算子标识
        node_id: String,
        /// 执行结果（状态、日志、耗时、错误信息）
        execution_result: OperatorExecutionResult,
        /// 累积所有 chunk 聚合后的预览输出（通常为拼接后的 `PortData::String`）
        final_outputs: Vec<PortData>,
        /// 首个 DataFrame 输出的真实行数（用于客户端预览提示）
        #[serde(default)]
        output_row_count: usize,
    },
    /// 查询执行状态响应
    ExecutionStatus {
        request_id: RequestId,
        /// 查询的 ID
        id: String,
        /// 执行状态
        status: OperatorExecutionStatus,
    },
    /// 查询执行日志响应
    ExecutionLogs {
        request_id: RequestId,
        /// 查询的 ID
        id: String,
        /// 日志列表
        logs: Vec<ExecutionLogEntry>,
        /// 日志总条数（用于分页）
        total_count: usize,
        /// 当前返回的起始索引
        start_index: usize,
    },
}
