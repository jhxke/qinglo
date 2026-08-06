use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{from_str, to_string};

use operator_runtime::protocol::{
    RuntimeRequest, RuntimeResponse, OperatorCategory,
    OperatorExecutionStatus, ExecutionLogEntry, OperatorExecutionResult,
    DagDefinition, DagExecutionResult, DagNodeResult,
};
use operator_runtime::PortData;

/// 默认 runtime 地址
pub const DEFAULT_RUNTIME_ADDR: &str = "127.0.0.1:17890";
/// 默认超时时间
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// 最大帧大小
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// Runtime 客户端错误
#[derive(Debug, thiserror::Error)]
pub enum RuntimeClientError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("序列化错误: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("连接失败: {0}")]
    ConnectionFailed(String),
    #[error("Runtime 错误: {0}")]
    RuntimeError(String),
    #[error("响应解析失败: {0}")]
    InvalidResponse(String),
    #[error("超时")]
    Timeout,
}

/// 算子执行返回结果（包含输出数据和完整的执行信息）
#[derive(Debug, Clone)]
pub struct ExecuteResult {
    /// 输出数据（已截断为预览）
    pub outputs: Vec<PortData>,
    /// 首个 DataFrame 输出的真实总行数（DataFrameArray 为所有 DataFrame 行数之和）
    pub output_row_count: usize,
    /// 执行结果（状态、日志、耗时、错误信息）
    pub execution_result: OperatorExecutionResult,
}

/// 查询执行日志返回结果（支持分页）
#[derive(Debug, Clone)]
pub struct LogQueryResult {
    /// 日志列表
    pub logs: Vec<ExecutionLogEntry>,
    /// 日志总条数
    pub total_count: usize,
    /// 当前返回的起始索引
    pub start_index: usize,
}

/// DAG 流式执行期间的事件（节点进度 + 流式 chunk）。
///
/// [`RuntimeClient::execute_dag_streaming`] 在收到服务端推送的 `DagNodeProgress` /
/// `StreamChunk` 帧时分别构造本枚举回调调用方，便于实时反馈进度与流式数据。
#[derive(Debug, Clone)]
pub enum DagStreamEvent {
    /// 节点进度（Executing / Completed / Failed）
    NodeProgress(DagNodeResult),
    /// 流式 chunk（流式节点产出 chunk 时实时回调）
    StreamChunk {
        /// 产出该 chunk 的节点 ID
        node_id: String,
        /// chunk 数据（通常为 `PortData::String`，如 LLM token）
        chunk: PortData,
        /// 该节点的 chunk 序号（从 0 起）
        chunk_index: u64,
    },
}

/// 独立单节点流式执行（`ExecuteStream`）的终止结果。
#[derive(Debug, Clone)]
pub struct StreamResult {
    /// 执行结果（状态、日志、耗时、错误信息）
    pub execution_result: OperatorExecutionResult,
    /// 累积所有 chunk 聚合后的预览输出（通常为拼接后的 `PortData::String`）
    pub final_outputs: Vec<PortData>,
    /// 首个 DataFrame 输出的真实行数（用于客户端预览提示）
    pub output_row_count: usize,
}

/// TCP 客户端（同步阻塞，便于在 GUI 线程中通过 spawn_blocking 调用）
pub struct RuntimeClient {
    addr: String,
    stream: Mutex<Option<TcpStream>>,
    timeout: Duration,
}

impl RuntimeClient {
    /// 创建客户端
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            stream: Mutex::new(None),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// 设置超时
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// 连接到 runtime 服务
    pub fn connect(&self) -> Result<(), RuntimeClientError> {
        let mut guard = self.stream.lock().unwrap();
        if guard.is_some() {
            return Ok(());
        }
        let stream = TcpStream::connect_timeout(
            &self.addr.parse().map_err(|e| RuntimeClientError::ConnectionFailed(format!("地址解析失败: {}", e)))?,
            self.timeout,
        )
        .map_err(|e| RuntimeClientError::ConnectionFailed(e.to_string()))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(RuntimeClientError::Io)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(RuntimeClientError::Io)?;
        *guard = Some(stream);
        Ok(())
    }

    /// 断开连接
    pub fn disconnect(&self) {
        let mut guard = self.stream.lock().unwrap();
        *guard = None;
    }

    /// 检查是否已连接
    pub fn is_connected(&self) -> bool {
        self.stream.lock().unwrap().is_some()
    }

    /// 重新连接
    fn ensure_connected(&self) -> Result<(), RuntimeClientError> {
        if self.stream.lock().unwrap().is_none() {
            self.connect()?;
        }
        Ok(())
    }

    /// 从已锁定的 stream 读取一帧响应（无锁，由调用方持锁）。
    fn read_frame_from_stream(stream: &mut TcpStream) -> Result<RuntimeResponse, RuntimeClientError> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf)?;
        let resp_len = u32::from_be_bytes(len_buf);
        if resp_len > MAX_FRAME_SIZE {
            return Err(RuntimeClientError::InvalidResponse(format!(
                "响应过大: {} bytes",
                resp_len
            )));
        }
        let mut resp_data = vec![0u8; resp_len as usize];
        stream.read_exact(&mut resp_data)?;

        let resp_str = String::from_utf8(resp_data)
            .map_err(|e| RuntimeClientError::InvalidResponse(format!("UTF-8 转换失败: {}", e)))?;

        let response: RuntimeResponse = from_str(&resp_str)?;
        Ok(response)
    }

    /// 发送请求并接收响应
    fn send_request(&self, request: &RuntimeRequest) -> Result<RuntimeResponse, RuntimeClientError> {
        self.ensure_connected()?;

        let json = to_string(request)?;
        let mut guard = self.stream.lock().unwrap();
        let stream = guard
            .as_mut()
            .ok_or_else(|| RuntimeClientError::ConnectionFailed("未连接".to_string()))?;

        // 写入帧
        let len = json.len() as u32;
        if len > MAX_FRAME_SIZE {
            return Err(RuntimeClientError::InvalidResponse(format!(
                "请求过大: {} bytes",
                len
            )));
        }
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(json.as_bytes())?;
        stream.flush()?;

        // 读取响应帧
        Self::read_frame_from_stream(stream)
    }

    /// Ping runtime
    pub fn ping(&self) -> Result<String, RuntimeClientError> {
        match self.send_request(&RuntimeRequest::Ping)? {
            RuntimeResponse::Pong { version, .. } => Ok(version),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 关闭 runtime
    pub fn shutdown(&self) -> Result<(), RuntimeClientError> {
        match self.send_request(&RuntimeRequest::Shutdown)? {
            RuntimeResponse::Ok { .. } => Ok(()),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 加载算子
    pub fn load_operator(&self, operator_id: &str, dll_path: &str) -> Result<(), RuntimeClientError> {
        match self.send_request(&RuntimeRequest::LoadOperator {
            operator_id: operator_id.to_string(),
            dll_path: dll_path.to_string(),
        })? {
            RuntimeResponse::OperatorLoaded { .. } => Ok(()),
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 卸载算子
    pub fn unload_operator(&self, operator_id: &str) -> Result<(), RuntimeClientError> {
        match self.send_request(&RuntimeRequest::UnloadOperator {
            operator_id: operator_id.to_string(),
        })? {
            RuntimeResponse::Ok { .. } => Ok(()),
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 执行已加载的算子（返回完整执行结果，包含状态和日志）
    pub fn execute_node(
        &self,
        operator_id: &str,
        inputs: &[PortData],
        max_outputs: usize,
        params_json: &str,
    ) -> Result<ExecuteResult, RuntimeClientError> {
        match self.send_request(&RuntimeRequest::ExecuteNode {
            operator_id: operator_id.to_string(),
            inputs: inputs.to_vec(),
            max_outputs,
            params_json: params_json.to_string(),
        })? {
            RuntimeResponse::NodeExecuted { outputs, output_row_count, execution_result, .. } => {
                // 如果执行状态是 Failed，将错误信息转换为错误返回，但保留执行结果信息
                if execution_result.status == OperatorExecutionStatus::Failed {
                    if let Some(err_msg) = &execution_result.error_message {
                        return Err(RuntimeClientError::RuntimeError(err_msg.clone()));
                    }
                }
                Ok(ExecuteResult { outputs, output_row_count, execution_result })
            },
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 执行已加载的算子（仅返回输出数据，兼容旧接口）
    pub fn execute_node_outputs_only(
        &self,
        operator_id: &str,
        inputs: &[PortData],
        max_outputs: usize,
        params_json: &str,
    ) -> Result<Vec<PortData>, RuntimeClientError> {
        self.execute_node(operator_id, inputs, max_outputs, params_json).map(|r| r.outputs)
    }

    /// 直接执行 DLL（返回完整执行结果，包含状态和日志）
    pub fn execute_dll(
        &self,
        dll_path: &str,
        inputs: &[PortData],
        max_outputs: usize,
        params_json: &str,
    ) -> Result<ExecuteResult, RuntimeClientError> {
        match self.send_request(&RuntimeRequest::ExecuteDll {
            dll_path: dll_path.to_string(),
            inputs: inputs.to_vec(),
            max_outputs,
            params_json: params_json.to_string(),
        })? {
            RuntimeResponse::NodeExecuted { outputs, output_row_count, execution_result, .. } => {
                if execution_result.status == OperatorExecutionStatus::Failed {
                    if let Some(err_msg) = &execution_result.error_message {
                        return Err(RuntimeClientError::RuntimeError(err_msg.clone()));
                    }
                }
                Ok(ExecuteResult { outputs, output_row_count, execution_result })
            },
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 直接执行 DLL（仅返回输出数据，兼容旧接口）
    pub fn execute_dll_outputs_only(
        &self,
        dll_path: &str,
        inputs: &[PortData],
        max_outputs: usize,
        params_json: &str,
    ) -> Result<Vec<PortData>, RuntimeClientError> {
        self.execute_dll(dll_path, inputs, max_outputs, params_json).map(|r| r.outputs)
    }

    /// 编译并执行（返回完整执行结果，包含状态和日志）
    pub fn compile_and_execute(
        &self,
        code: &str,
        algorithm_name: &str,
        inputs: &[PortData],
        max_outputs: usize,
    ) -> Result<ExecuteResult, RuntimeClientError> {
        match self.send_request(&RuntimeRequest::CompileAndExecute {
            code: code.to_string(),
            algorithm_name: algorithm_name.to_string(),
            inputs: inputs.to_vec(),
            max_outputs,
            params_json: String::new(),
        })? {
            RuntimeResponse::CompiledAndExecuted { outputs, output_row_count, execution_result, .. } => {
                if execution_result.status == OperatorExecutionStatus::Failed {
                    if let Some(err_msg) = &execution_result.error_message {
                        return Err(RuntimeClientError::RuntimeError(err_msg.clone()));
                    }
                }
                Ok(ExecuteResult { outputs, output_row_count, execution_result })
            },
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 编译并执行（仅返回输出数据，兼容旧接口）
    pub fn compile_and_execute_outputs_only(
        &self,
        code: &str,
        algorithm_name: &str,
        inputs: &[PortData],
        max_outputs: usize,
    ) -> Result<Vec<PortData>, RuntimeClientError> {
        self.compile_and_execute(code, algorithm_name, inputs, max_outputs).map(|r| r.outputs)
    }

    /// 查询可用算子列表（层级结构）
    pub fn list_operators(&self) -> Result<Vec<OperatorCategory>, RuntimeClientError> {
        match self.send_request(&RuntimeRequest::ListOperators)? {
            RuntimeResponse::OperatorsList { categories, .. } => Ok(categories),
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 下发完整 DAG 到服务端，由服务端解析拓扑并按序执行，并流式接收节点进度与流式 chunk。
    ///
    /// 服务端在节点开始/结束时推送 `DagNodeProgress`，流式节点产出 chunk 时实时推送
    /// `StreamChunk`，最后推送一条 `DagExecuted` 终止帧。本方法把这两类帧统一封装为
    /// [`DagStreamEvent`] 回调调用方；收到终止帧时返回最终结果。
    ///
    /// **整个会话持有 stream 锁**，避免心跳线程在此期间复用连接造成帧错位。
    /// 心跳线程短暂阻塞（DAG 执行时长）可接受。
    pub fn execute_dag_streaming<F>(
        &self,
        dag: &DagDefinition,
        mut on_event: F,
    ) -> Result<DagExecutionResult, RuntimeClientError>
    where
        F: FnMut(DagStreamEvent),
    {
        self.ensure_connected()?;

        let json = to_string(&RuntimeRequest::ExecuteDag { dag: dag.clone() })?;
        let mut guard = self.stream.lock().unwrap();
        let stream = guard
            .as_mut()
            .ok_or_else(|| RuntimeClientError::ConnectionFailed("未连接".to_string()))?;

        // 写请求帧（内联，持锁覆盖整个会话，避免心跳线程复用连接造成帧错位）
        let len = json.len() as u32;
        if len > MAX_FRAME_SIZE {
            return Err(RuntimeClientError::InvalidResponse(format!(
                "请求过大: {} bytes",
                len
            )));
        }
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(json.as_bytes())?;
        stream.flush()?;

        // 循环读帧：DagNodeProgress / StreamChunk 调回调，DagExecuted 返回结果
        loop {
            let resp = Self::read_frame_from_stream(stream)?;
            match resp {
                RuntimeResponse::DagNodeProgress { progress, .. } => {
                    on_event(DagStreamEvent::NodeProgress(progress));
                }
                RuntimeResponse::StreamChunk { node_id, chunk, chunk_index, .. } => {
                    on_event(DagStreamEvent::StreamChunk { node_id, chunk, chunk_index });
                }
                RuntimeResponse::DagExecuted { result, .. } => return Ok(result),
                RuntimeResponse::Error { message, .. } => {
                    return Err(RuntimeClientError::RuntimeError(message));
                }
                other => {
                    return Err(RuntimeClientError::InvalidResponse(format!(
                        "ExecuteDag 期间收到意外响应: {:?}",
                        std::mem::discriminant(&other)
                    )));
                }
            }
        }
    }

    /// 下发完整 DAG 到服务端，由服务端解析拓扑并按序执行，并流式接收每个节点的执行进度。
    ///
    /// 服务端在「开始执行某节点」和「某节点执行结束」时各推送一条 `DagNodeProgress`，
    /// 最后推送一条 `DagExecuted` 终止帧。本方法在收到每条进度时调用 `on_progress` 回调，
    /// 便于调用方实时反馈进度；收到终止帧时返回最终结果。
    ///
    /// **注意**：若 DAG 含流式节点，服务端会额外推送 `StreamChunk` 帧，本方法忽略它们
    /// （仅转发节点进度）。需要接收 chunk 请用 [`Self::execute_dag_streaming`]。
    pub fn execute_dag_with_progress<F>(
        &self,
        dag: &DagDefinition,
        mut on_progress: F,
    ) -> Result<DagExecutionResult, RuntimeClientError>
    where
        F: FnMut(&DagNodeResult),
    {
        self.execute_dag_streaming(dag, |ev| {
            if let DagStreamEvent::NodeProgress(p) = ev {
                on_progress(&p);
            }
        })
    }

    /// 下发完整 DAG 到服务端，由服务端解析拓扑并按序执行。
    ///
    /// 服务端返回各节点的输入/输出与执行结果，调用方可据此回填本地 registry 与预览缓存。
    /// 等价于 [`Self::execute_dag_with_progress`] 传空回调，不接收中间进度。
    pub fn execute_dag(&self, dag: &DagDefinition) -> Result<DagExecutionResult, RuntimeClientError> {
        self.execute_dag_with_progress(dag, |_| {})
    }

    /// 独立单节点流式执行（`ExecuteStream`）：逐 chunk 回调，流结束后返回聚合结果。
    ///
    /// 服务端对算子 DLL 调用流式 C ABI，每产出一个 chunk 推送一条 `StreamChunk`，
    /// 本方法在收到时调用 `on_chunk(chunk, chunk_index)`；流结束后推送一条
    /// `StreamCompleted` 终止帧，返回 [`StreamResult`]。算子必须导出 5 个流式符号，
    /// 否则终止帧的 `execution_result.status` 为 `Failed`。
    ///
    /// **整个会话持有 stream 锁**，避免心跳线程在此期间复用连接造成帧错位。
    pub fn execute_stream<F>(
        &self,
        operator_id: &str,
        inputs: &[PortData],
        max_outputs: usize,
        params_json: &str,
        mut on_chunk: F,
    ) -> Result<StreamResult, RuntimeClientError>
    where
        F: FnMut(&PortData, u64),
    {
        self.ensure_connected()?;

        let json = to_string(&RuntimeRequest::ExecuteStream {
            operator_id: operator_id.to_string(),
            inputs: inputs.to_vec(),
            max_outputs,
            params_json: params_json.to_string(),
        })?;
        let mut guard = self.stream.lock().unwrap();
        let stream = guard
            .as_mut()
            .ok_or_else(|| RuntimeClientError::ConnectionFailed("未连接".to_string()))?;

        // 写请求帧
        let len = json.len() as u32;
        if len > MAX_FRAME_SIZE {
            return Err(RuntimeClientError::InvalidResponse(format!(
                "请求过大: {} bytes",
                len
            )));
        }
        stream.write_all(&len.to_be_bytes())?;
        stream.write_all(json.as_bytes())?;
        stream.flush()?;

        // 循环读帧：StreamChunk 调回调，StreamCompleted 返回结果
        loop {
            let resp = Self::read_frame_from_stream(stream)?;
            match resp {
                RuntimeResponse::StreamChunk { chunk, chunk_index, .. } => {
                    on_chunk(&chunk, chunk_index);
                }
                RuntimeResponse::StreamCompleted {
                    execution_result,
                    final_outputs,
                    output_row_count,
                    ..
                } => {
                    return Ok(StreamResult {
                        execution_result,
                        final_outputs,
                        output_row_count,
                    });
                }
                RuntimeResponse::Error { message, .. } => {
                    return Err(RuntimeClientError::RuntimeError(message));
                }
                other => {
                    return Err(RuntimeClientError::InvalidResponse(format!(
                        "ExecuteStream 期间收到意外响应: {:?}",
                        std::mem::discriminant(&other)
                    )));
                }
            }
        }
    }

    /// 独立单节点流式执行的便捷变体：收集所有 chunk 到 Vec，流结束后一并返回。
    ///
    /// 适用于不需要逐 chunk 实时处理、只需最终完整 chunk 列表 + 聚合结果的场景。
    pub fn execute_stream_collect(
        &self,
        operator_id: &str,
        inputs: &[PortData],
        max_outputs: usize,
        params_json: &str,
    ) -> Result<(StreamResult, Vec<PortData>), RuntimeClientError> {
        let mut chunks = Vec::new();
        let result = self.execute_stream(operator_id, inputs, max_outputs, params_json, |chunk, _| {
            chunks.push(chunk.clone());
        })?;
        Ok((result, chunks))
    }

    /// 查询指定算子/节点的执行状态
    pub fn query_execution_status(&self, id: &str) -> Result<OperatorExecutionStatus, RuntimeClientError> {
        match self.send_request(&RuntimeRequest::QueryExecutionStatus {
            id: id.to_string(),
        })? {
            RuntimeResponse::ExecutionStatus { status, .. } => Ok(status),
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }

    /// 查询指定算子/节点的执行日志（支持分页）
    pub fn query_execution_logs(
        &self,
        id: &str,
        start_index: Option<usize>,
        max_count: Option<usize>,
    ) -> Result<LogQueryResult, RuntimeClientError> {
        match self.send_request(&RuntimeRequest::QueryExecutionLogs {
            id: id.to_string(),
            start_index,
            max_count,
        })? {
            RuntimeResponse::ExecutionLogs { logs, total_count, start_index: returned_start, .. } => {
                Ok(LogQueryResult {
                    logs,
                    total_count,
                    start_index: returned_start,
                })
            },
            RuntimeResponse::Error { message, .. } => Err(RuntimeClientError::RuntimeError(message)),
            other => Err(RuntimeClientError::InvalidResponse(format!(
                "Unexpected response: {:?}",
                std::mem::discriminant(&other)
            ))),
        }
    }
}

/// 启动 runtime 子进程（可选）
/// 如果 runtime 服务未运行，可以调用此函数启动
pub fn spawn_runtime_server(
    exe_path: &Path,
    addr: &str,
    compile_dir: &Path,
) -> Result<std::process::Child, std::io::Error> {
    std::process::Command::new(exe_path)
        .arg(addr)
        .env("RUNTIME_COMPILE_DIR", compile_dir)
        .spawn()
}