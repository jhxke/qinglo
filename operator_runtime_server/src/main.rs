use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::{from_str, to_string};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use operator_runtime::protocol::{
    RuntimeRequest, RuntimeResponse, RequestId, OperatorCategory, OperatorInfo,
    OperatorConfig, OperatorExecutionResult, OperatorExecutionStatus, ExecutionLogEntry,
    DagDefinition, DagNodeDef, DagNodeResult, DagExecutionResult,
};
use operator_runtime::PortData;
use operator_runtime::PREVIEW_ROW_LIMIT;
use operator_sdk as executor;

/// 服务版本
const VERSION: &str = "0.1.0";
/// 默认监听地址
const DEFAULT_ADDR: &str = "127.0.0.1:17890";
/// 最大帧大小 (16 MB)
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// 已加载的算子注册表
struct OperatorEntry {
    operator_id: String,
    dll_path: PathBuf,
}

/// 算子执行记录
#[allow(dead_code)]
struct ExecutionRecord {
    /// 执行 ID (算子 ID 或请求 ID)
    id: String,
    /// 执行结果（状态、日志、耗时、错误）
    result: OperatorExecutionResult,
}

/// Runtime 状态
struct RuntimeState {
    /// 下一个请求 ID
    next_request_id: AtomicU64,
    /// 已加载的算子
    operators: RwLock<Vec<OperatorEntry>>,
    /// 编译输出目录
    compile_dir: PathBuf,
    /// 算子库目录（存放算子 DLL 和 JSON 配置）
    lib_dir: PathBuf,
    /// 执行结果注册表（用于状态和日志查询）
    execution_records: RwLock<std::collections::HashMap<String, ExecutionRecord>>,
}

impl RuntimeState {
    fn new(compile_dir: PathBuf, lib_dir: PathBuf) -> Self {
        Self {
            next_request_id: AtomicU64::new(1),
            operators: RwLock::new(Vec::new()),
            compile_dir,
            lib_dir,
            execution_records: RwLock::new(std::collections::HashMap::new()),
        }
    }

    fn alloc_request_id(&self) -> RequestId {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    fn find_operator(&self, operator_id: &str) -> Option<PathBuf> {
        self.operators
            .read()
            .iter()
            .find(|e| e.operator_id == operator_id)
            .map(|e| e.dll_path.clone())
    }

    fn load_operator(&self, operator_id: String, dll_path: PathBuf) {
        let mut ops = self.operators.write();
        ops.retain(|e| e.operator_id != operator_id);
        ops.push(OperatorEntry {
            operator_id,
            dll_path,
        });
    }

    fn unload_operator(&self, operator_id: &str) {
        self.operators
            .write()
            .retain(|e| e.operator_id != operator_id);
    }

    /// 记录正在执行的状态
    fn set_executing(&self, id: &str) {
        let mut records = self.execution_records.write();
        records.insert(id.to_string(), ExecutionRecord {
            id: id.to_string(),
            result: OperatorExecutionResult::executing(),
        });
    }

    /// 更新执行结果
    fn update_execution_result(&self, id: &str, result: OperatorExecutionResult) {
        let mut records = self.execution_records.write();
        records.insert(id.to_string(), ExecutionRecord {
            id: id.to_string(),
            result,
        });
    }

    /// 获取执行状态
    fn get_execution_status(&self, id: &str) -> OperatorExecutionStatus {
        self.execution_records
            .read()
            .get(id)
            .map(|r| r.result.status)
            .unwrap_or(OperatorExecutionStatus::NotExecuted)
    }

    /// 获取执行日志
    fn get_execution_logs(&self, id: &str) -> Vec<ExecutionLogEntry> {
        self.execution_records
            .read()
            .get(id)
            .map(|r| r.result.logs.clone())
            .unwrap_or_default()
    }

    /// 扫描 lib 目录，返回层级化的算子列表。
    ///
    /// 目录结构规则：
    /// - 含 `operator.json` 的目录 → 算子叶子（直接加载为算子，不作为分类）
    /// - 不含 `operator.json` 但含子目录的目录 → 中间分类（保留为 Category，可展开）
    /// - `lib_dir` 直接下的算子归入一个空名分类（前端不渲染分类头，直接显示算子）
    fn scan_operators(&self) -> Vec<OperatorCategory> {
        let mut categories = Vec::new();
        let lib_dir = &self.lib_dir;

        if !lib_dir.exists() {
            return categories;
        }

        let Ok(entries) = std::fs::read_dir(lib_dir) else {
            return categories;
        };

        let mut top_operators = Vec::new();
        let mut sub_categories = Vec::new();

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            if has_operator_config(&path) {
                // 直接是算子目录 → 算子叶子
                if let Some(op) = load_operator_info(&path, &[name.clone()]) {
                    top_operators.push(op);
                }
            } else {
                // 分层目录 → 递归构建分类
                if let Some(cat) = build_category_tree(&path, &[name.clone()]) {
                    sub_categories.push(cat);
                }
            }
        }

        // 顶层算子放到空名分类里（前端识别空名后不渲染分类头，直接显示算子）
        if !top_operators.is_empty() {
            categories.push(OperatorCategory {
                name: String::new(),
                operators: top_operators,
                subcategories: Vec::new(),
            });
        }

        categories.extend(sub_categories);
        categories
    }

    /// 在算子库目录中按算子名递归查找其 DLL 路径。
    ///
    /// 用于 `ExecuteDag` 时节点未显式提供 `dll_path` 的兜底：服务端遍历 `lib_dir`，
    /// 读取每个 `operator.json` 中的 `name` 字段，命中后返回同目录下的 DLL。
    fn find_operator_dll_by_name(&self, operator_name: &str) -> Option<PathBuf> {
        if !self.lib_dir.exists() {
            return None;
        }
        find_dll_by_name_recursive(&self.lib_dir, operator_name)
    }
}

/// 递归扫描目录，按算子名查找 DLL 路径。
fn find_dll_by_name_recursive(dir: &std::path::Path, operator_name: &str) -> Option<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();

        if path.is_dir() {
            if let Some(p) = find_dll_by_name_recursive(&path, operator_name) {
                return Some(p);
            }
        } else if path.is_file() {
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if file_name == "operator.json" {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(config) = serde_json::from_str::<OperatorConfig>(&content) {
                        if config.name == operator_name {
                            // 找到匹配的算子，返回同目录下的 DLL
                            let dll = find_dll_in_dir(path.parent().unwrap_or(dir));
                            if dll.exists() {
                                return Some(dll);
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

/// 检查目录下是否包含 operator.json（标识为算子叶子）
fn has_operator_config(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        if entry.path().is_file() {
            if let Some(name) = entry.file_name().to_str() {
                if name == "operator.json" {
                    return true;
                }
            }
        }
    }
    false
}

/// 从目录加载单个算子的 OperatorInfo
fn load_operator_info(dir: &Path, category_path: &[String]) -> Option<OperatorInfo> {
    let json_path = dir.join("operator.json");
    let content = std::fs::read_to_string(&json_path).ok()?;
    let config: OperatorConfig = from_str(&content).ok()?;
    let dll_path = find_dll_in_dir(dir);

    Some(OperatorInfo {
        name: config.name,
        description: config.description,
        dll_path: dll_path.to_string_lossy().to_string(),
        color: config.color,
        port_params: config.port_params,
        category_path: category_path.to_vec(),
        summary: config.summary,
        description_md: config.description_md,
        stream: config.stream,
    })
}

/// 递归构建算子分类树。
///
/// 规则：
/// - 子目录含 `operator.json` → 算子叶子，扁平化到当前分类的 `operators` 中
/// - 子目录不含 `operator.json` → 中间分类，递归构建为子分类
/// - 空目录（既无算子也无子分类）→ 返回 None
fn build_category_tree(dir: &Path, category_path: &[String]) -> Option<OperatorCategory> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };

    let mut operators = Vec::new();
    let mut subcategories = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let sub_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let mut sub_path = category_path.to_vec();
        sub_path.push(sub_name);

        if has_operator_config(&path) {
            // 算子叶子目录
            if let Some(op) = load_operator_info(&path, &sub_path) {
                operators.push(op);
            }
        } else {
            // 中间分类目录
            if let Some(cat) = build_category_tree(&path, &sub_path) {
                subcategories.push(cat);
            }
        }
    }

    if operators.is_empty() && subcategories.is_empty() {
        return None;
    }

    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    Some(OperatorCategory {
        name,
        operators,
        subcategories,
    })
}

/// 在指定目录下查找算子 DLL 文件
///
/// 跳过 `operator_runtime` 这一共享运行时依赖 DLL——`run_srv.ps1` 会把它复制到
/// 每个算子目录作为动态链接依赖，但它本身不导出 `execute_operator`，不能当作
/// 算子加载。历史上只有名字字母序排在 `operator_runtime` 之后的算子（如
/// `shift_add_operator`）才会因 `read_dir` 顺序误命中它，这里统一排除以根治。
fn find_dll_in_dir(dir: &std::path::Path) -> PathBuf {
    let dll_ext = if cfg!(windows) { "dll" } else { "so" };

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == dll_ext {
                        // 跳过共享运行时依赖，它不是算子
                        if path.file_stem().and_then(|s| s.to_str()) == Some("operator_runtime") {
                            continue;
                        }
                        return path;
                    }
                }
            }
        }
    }

    // 回退：返回默认路径
    dir.join(format!("operator.{}", dll_ext))
}

/// 递归遍历算子分类树，预加载其中所有算子的 DLL 到全局缓存。
///
/// 返回 `(成功数, 失败数)`。失败时在 stderr 打印具体算子与错误，但不中断
/// 整体预加载流程——个别算子加载失败不影响其他算子的正常使用。
fn preload_all_categories(categories: &[OperatorCategory]) -> (usize, usize) {
    let mut loaded = 0usize;
    let mut failed = 0usize;
    for cat in categories {
        for op in &cat.operators {
            let path = PathBuf::from(&op.dll_path);
            match executor::preload_operator(&path) {
                Ok(()) => loaded += 1,
                Err(e) => {
                    failed += 1;
                    eprintln!("[runtime] 预加载算子失败 {}: {}", op.name, e);
                }
            }
        }
        // 递归处理子分类
        let (sub_loaded, sub_failed) = preload_all_categories(&cat.subcategories);
        loaded += sub_loaded;
        failed += sub_failed;
    }
    (loaded, failed)
}

/// 读取一个完整的帧。
///
/// 返回值：
/// - `Ok(Some(json))`：成功读取一帧。
/// - `Ok(None)`：在帧边界处（尚未读取任何长度字节）连接即关闭，视为客户端正常断开，
///   上层应安静退出而不要打印错误，避免一次性客户端（如每帧拉取算子列表）关闭连接时刷屏。
/// - `Err(..)`：读取过程中发生真正的 IO/协议错误（含帧中途的 early eof）。
async fn read_frame(stream: &mut TcpStream) -> Result<Option<String>, String> {
    // 逐字节读取 4 字节长度前缀，以便区分「帧边界处的干净关闭」与「帧中途断开」。
    let mut len_buf = [0u8; 4];
    let mut filled = 0;
    loop {
        match stream.read(&mut len_buf[filled..]).await {
            Ok(0) => {
                if filled == 0 {
                    return Ok(None);
                }
                return Err(format!(
                    "读取帧长度失败: early eof (已读 {}/4 字节)",
                    filled
                ));
            }
            Ok(n) => {
                filled += n;
                if filled == 4 {
                    break;
                }
            }
            Err(e) => return Err(format!("读取帧长度失败: {}", e)),
        }
    }
    let len = u32::from_be_bytes(len_buf);

    if len > MAX_FRAME_SIZE {
        return Err(format!("帧过大: {} bytes (最大 {})", len, MAX_FRAME_SIZE));
    }

    let mut data = vec![0u8; len as usize];
    stream.read_exact(&mut data).await.map_err(|e| format!("读取帧数据失败: {}", e))?;

    String::from_utf8(data)
        .map_err(|e| format!("帧数据不是合法 UTF-8: {}", e))
        .map(Some)
}

/// 写入一个完整的帧
async fn write_frame(stream: &mut TcpStream, json: &str) -> Result<(), String> {
    let len = json.len() as u32;
    if len > MAX_FRAME_SIZE {
        return Err(format!("响应过大: {} bytes", len));
    }
    let len_bytes = len.to_be_bytes();
    stream.write_all(&len_bytes).await.map_err(|e| format!("写入帧长度失败: {}", e))?;
    stream.write_all(json.as_bytes()).await.map_err(|e| format!("写入帧数据失败: {}", e))?;
    stream.flush().await.map_err(|e| format!("flush 失败: {}", e))?;
    Ok(())
}

/// 处理单个请求
fn handle_request(state: Arc<RuntimeState>, request: RuntimeRequest) -> RuntimeResponse {
    match request {
        RuntimeRequest::Ping => RuntimeResponse::Pong {
            request_id: 0,
            version: VERSION.to_string(),
        },
        RuntimeRequest::Shutdown => RuntimeResponse::Ok { request_id: 0 },
        RuntimeRequest::LoadOperator {
            operator_id,
            dll_path,
        } => {
            let request_id = state.alloc_request_id();
            let dll_path_buf = PathBuf::from(&dll_path);
            if !dll_path_buf.exists() {
                return RuntimeResponse::Error {
                    request_id,
                    message: format!("DLL 不存在: {}", dll_path),
                };
            }
            // 仅注册到算子注册表；DLL 的加载由 execute_native_operator 按需进行：
            //   - 启动时已预加载（lib_dir）的算子 → 命中缓存，直接复用
            //   - 自定义算子（operator_dir）→ 即时加载并 drop，保持文件可覆盖
            // 此处不主动 preload，避免长期占用自定义算子的文件锁导致重新编译覆盖失败。
            state.load_operator(operator_id, dll_path_buf);
            RuntimeResponse::OperatorLoaded { request_id }
        }
        RuntimeRequest::UnloadOperator { operator_id } => {
            let request_id = state.alloc_request_id();
            state.unload_operator(&operator_id);
            RuntimeResponse::Ok { request_id }
        }
        RuntimeRequest::ExecuteNode {
            operator_id,
            inputs,
            max_outputs,
            params_json,
        } => {
            let request_id = state.alloc_request_id();
            // 先标记为正在执行
            state.set_executing(&operator_id);
            let dll_path = match state.find_operator(&operator_id) {
                Some(p) => p,
                None => {
                    let exec_result = OperatorExecutionResult::failed(
                        Vec::new(),
                        format!("算子未加载: {}", operator_id),
                        None,
                    );
                    state.update_execution_result(&operator_id, exec_result.clone());
                    return RuntimeResponse::Error {
                        request_id,
                        message: format!("算子未加载: {}", operator_id),
                    };
                }
            };
            let result = execute_dll(&dll_path, &inputs, max_outputs, &params_json, request_id);
            // 更新执行记录
            if let RuntimeResponse::NodeExecuted { ref execution_result, .. } = result {
                state.update_execution_result(&operator_id, execution_result.clone());
            }
            result
        }
        RuntimeRequest::ExecuteDll {
            dll_path,
            inputs,
            max_outputs,
            params_json,
        } => {
            let request_id = state.alloc_request_id();
            let exec_id = format!("dll_{}", request_id);
            // 先标记为正在执行
            state.set_executing(&exec_id);
            let dll_path_buf = PathBuf::from(&dll_path);
            if !dll_path_buf.exists() {
                let exec_result = OperatorExecutionResult::failed(
                    Vec::new(),
                    format!("DLL 不存在: {}", dll_path),
                    None,
                );
                state.update_execution_result(&exec_id, exec_result);
                return RuntimeResponse::Error {
                    request_id,
                    message: format!("DLL 不存在: {}", dll_path),
                };
            }
            let result = execute_dll(&dll_path_buf, &inputs, max_outputs, &params_json, request_id);
            // 更新执行记录
            if let RuntimeResponse::NodeExecuted { ref execution_result, .. } = result {
                state.update_execution_result(&exec_id, execution_result.clone());
            }
            result
        }
        RuntimeRequest::CompileAndExecute {
            code,
            algorithm_name,
            inputs,
            max_outputs,
            params_json: _,
        } => {
            let request_id = state.alloc_request_id();
            let exec_id = format!("compile_{}", request_id);
            // 先标记为正在执行
            state.set_executing(&exec_id);
            let runtime_path = match executor::find_runtime_path() {
                Ok(p) => p,
                Err(e) => {
                    let exec_result = OperatorExecutionResult::failed(Vec::new(), e.clone(), None);
                    state.update_execution_result(&exec_id, exec_result);
                    return RuntimeResponse::Error {
                        request_id,
                        message: e,
                    };
                }
            };
            let start = std::time::Instant::now();
            match executor::compile_and_execute(
                &code,
                &algorithm_name,
                &state.compile_dir,
                &inputs,
                max_outputs,
                &runtime_path,
            ) {
                Ok(outputs) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let exec_result = OperatorExecutionResult::completed(Vec::new(), duration_ms);
                    state.update_execution_result(&exec_id, exec_result.clone());
                    let output_row_count = outputs
                        .iter()
                        .find_map(|p| p.first_dataframe_row_count())
                        .unwrap_or(0);
                    RuntimeResponse::CompiledAndExecuted {
                        request_id,
                        outputs,
                        output_row_count,
                        dll_path: None,
                        execution_result: exec_result,
                    }
                },
                Err(e) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let exec_result = OperatorExecutionResult::failed(
                        Vec::new(),
                        e.clone(),
                        Some(duration_ms),
                    );
                    state.update_execution_result(&exec_id, exec_result);
                    RuntimeResponse::Error {
                        request_id,
                        message: e,
                    }
                },
            }
        }
        RuntimeRequest::ListOperators => {
            let request_id = state.alloc_request_id();
            let categories = state.scan_operators();
            RuntimeResponse::OperatorsList {
                request_id,
                categories,
            }
        }
        RuntimeRequest::ExecuteDag { dag } => {
            // 实际由 handle_client 拦截做流式推送，不应到达此处；保留兜底以防遗漏
            let request_id = state.alloc_request_id();
            let result = execute_dag(&state, &dag, |_| {});
            RuntimeResponse::DagExecuted {
                request_id,
                result,
            }
        }
        RuntimeRequest::ExecuteStream { .. } => {
            // 实际由 handle_client 拦截做流式推送，不应到达此单响应路径
            let request_id = state.alloc_request_id();
            RuntimeResponse::Error {
                request_id,
                message: "ExecuteStream 应由 handle_client 流式处理，不应走单响应路径".to_string(),
            }
        }
        RuntimeRequest::QueryExecutionStatus { id } => {
            let request_id = state.alloc_request_id();
            let status = state.get_execution_status(&id);
            RuntimeResponse::ExecutionStatus {
                request_id,
                id,
                status,
            }
        }
        RuntimeRequest::QueryExecutionLogs { id, start_index, max_count } => {
            let request_id = state.alloc_request_id();
            let all_logs = state.get_execution_logs(&id);
            let total_count = all_logs.len();
            let start = start_index.unwrap_or(0).min(total_count);
            let end = match max_count {
                Some(count) => (start + count).min(total_count),
                None => total_count,
            };
            let logs: Vec<ExecutionLogEntry> = all_logs[start..end].to_vec();
            RuntimeResponse::ExecutionLogs {
                request_id,
                id,
                logs,
                total_count,
                start_index: start,
            }
        }
    }
}

/// 执行 DLL 的辅助函数（返回包含执行结果的响应）
fn execute_dll(
    dll_path: &PathBuf,
    inputs: &[PortData],
    max_outputs: usize,
    params_json: &str,
    request_id: RequestId,
) -> RuntimeResponse {
    let start = std::time::Instant::now();
    let mut logs = Vec::new();
    logs.push(ExecutionLogEntry::info(format!(
        "开始执行算子 DLL: {}",
        dll_path.display()
    )));
    logs.push(ExecutionLogEntry::debug(format!(
        "输入数量: {}, 最大输出数: {}, 参数长度: {}",
        inputs.len(),
        max_outputs,
        params_json.len()
    )));

    match executor::execute_operator(dll_path, inputs, max_outputs, params_json) {
        Ok(outputs) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            logs.push(ExecutionLogEntry::debug(format!(
                "输出数量: {}",
                outputs.len()
            )));
            let exec_result = OperatorExecutionResult::completed(logs, duration_ms);
            // 在完整 outputs 上计算行数（DataFrameArray 取各 DataFrame 中最大行数，
            // 与 preview() 的「逐 DataFrame 独立截断」语义一致，用于提示是否截断）
            let output_row_count = outputs
                .iter()
                .find_map(|p| p.first_dataframe_row_count())
                .unwrap_or(0);
            // 回传给客户端的仅是预览（前 PREVIEW_ROW_LIMIT 行），与 execute_dag 保持一致，
            // 避免大数据量（如 DataFrameArray 全量分组结果）导致响应超过帧大小上限。
            let preview_outputs: Vec<PortData> = outputs
                .iter()
                .map(|p| p.preview(PREVIEW_ROW_LIMIT))
                .collect();
            RuntimeResponse::NodeExecuted {
                request_id,
                outputs: preview_outputs,
                output_row_count,
                execution_result: exec_result,
            }
        },
        Err(e) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            logs.push(ExecutionLogEntry::error(format!(
                "执行错误: {}",
                e
            )));
            let exec_result = OperatorExecutionResult::failed(
                logs,
                e.clone(),
                Some(duration_ms),
            );
            // 对于执行失败，仍然返回 NodeExecuted 但状态为 Failed，这样前端能拿到完整日志
            RuntimeResponse::NodeExecuted {
                request_id,
                outputs: Vec::new(),
                output_row_count: 0,
                execution_result: exec_result,
            }
        },
    }
}

/// 对 DAG 定义进行拓扑排序（Kahn 算法）。
///
/// 入度 0 的节点按 `dag.nodes` 中的出现顺序入队，保证结果在相同输入下稳定。
/// 存在环时返回错误。
fn topological_sort_dag(dag: &DagDefinition) -> Result<Vec<String>, String> {
    use std::collections::{HashMap, VecDeque};

    let mut in_degree: HashMap<String, usize> = HashMap::new();
    for n in &dag.nodes {
        in_degree.insert(n.id.clone(), 0);
    }
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for e in &dag.edges {
        adj.entry(e.source_node_id.clone()).or_default().push(e.target_node_id.clone());
        let entry = in_degree
            .get_mut(&e.target_node_id)
            .ok_or_else(|| format!("边引用了不存在的目标节点: {}", e.target_node_id))?;
        *entry += 1;
        if !in_degree.contains_key(&e.source_node_id) {
            return Err(format!("边引用了不存在的源节点: {}", e.source_node_id));
        }
    }

    let mut queue: VecDeque<String> = dag
        .nodes
        .iter()
        .filter(|n| in_degree.get(&n.id).copied().unwrap_or(0) == 0)
        .map(|n| n.id.clone())
        .collect();

    let mut result = Vec::new();
    while let Some(id) = queue.pop_front() {
        result.push(id.clone());
        if let Some(targets) = adj.get(&id) {
            for t in targets {
                if let Some(d) = in_degree.get_mut(t) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(t.clone());
                    }
                }
            }
        }
    }

    if result.len() != dag.nodes.len() {
        return Err("DAG 中存在循环依赖，无法进行拓扑排序".to_string());
    }
    Ok(result)
}

/// 解析节点对应的算子 DLL 路径。
///
/// 优先级：节点显式 `dll_path` > 已加载算子注册表 > 算子库目录按名查找。
fn resolve_node_dll(state: &RuntimeState, node: &DagNodeDef) -> Result<PathBuf, String> {
    if let Some(p) = &node.dll_path {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }
    if let Some(p) = state.find_operator(&node.operator_name) {
        if p.exists() {
            return Ok(p);
        }
    }
    if let Some(p) = state.find_operator_dll_by_name(&node.operator_name) {
        return Ok(p);
    }
    Err(format!("找不到算子 \"{}\" 的 DLL 文件", node.operator_name))
}

/// DAG 执行过程中的事件（流式 + 节点进度统一通道）。
///
/// `execute_dag` 通过 `on_progress: FnMut(DagEvent)` 推送事件，`handle_client` 的
/// ExecuteDag 拦截分支将其分派为 `DagNodeProgress` / `StreamChunk` 响应帧。
enum DagEvent {
    /// 节点进度（Executing / Completed / Failed）
    NodeProgress(DagNodeResult),
    /// 流式 chunk（节点产出 chunk 时实时推送）
    StreamChunk { node_id: String, chunk: PortData, chunk_index: u64 },
}

/// 把流式节点累积的 chunk 列表聚合为单个 `PortData`，供非流式下游消费与 DagNodeResult 预览。
///
/// 聚合规则（v1）：
/// - 全 `String` → 拼接
/// - 全 `DataFrame` → `DataFrameArray`
/// - 单 chunk → 原样
/// - 空 → `String("")`
/// - 标量/混合 → 取最后一个
fn aggregate_chunks(chunks: Vec<PortData>) -> PortData {
    if chunks.is_empty() {
        return PortData::String(String::new());
    }
    if chunks.len() == 1 {
        return chunks.into_iter().next().unwrap();
    }
    if chunks.iter().all(|c| matches!(c, PortData::String(_))) {
        let mut s = String::new();
        for c in chunks {
            if let PortData::String(part) = c {
                s.push_str(&part);
            }
        }
        return PortData::String(s);
    }
    if chunks.iter().all(|c| matches!(c, PortData::DataFrame(_))) {
        let dfs: Vec<_> = chunks
            .into_iter()
            .filter_map(|c| if let PortData::DataFrame(df) = c { Some(df) } else { None })
            .collect();
        return PortData::DataFrameArray(dfs);
    }
    // 标量/混合 → 取最后一个（v1 约定）
    chunks.into_iter().last().unwrap()
}

/// 从流式链头出发，沿 `streaming_out` 边收集整个流式子图的节点集合。
fn collect_streaming_subgraph(
    head: &str,
    streaming_out: &std::collections::HashMap<String, Vec<String>>,
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let mut stack = vec![head.to_string()];
    while let Some(n) = stack.pop() {
        if set.insert(n.clone()) {
            if let Some(downs) = streaming_out.get(&n) {
                for d in downs {
                    stack.push(d.clone());
                }
            }
        }
    }
    set
}

/// 流式编排预计算结果的只读视图，传递给 [`execute_streaming_subgraph`]。
struct StreamingPlan<'a> {
    node_map: &'a std::collections::HashMap<&'a String, &'a DagNodeDef>,
    node_dll: &'a std::collections::HashMap<String, PathBuf>,
    streaming_capable: &'a std::collections::HashMap<String, bool>,
    streaming_out: &'a std::collections::HashMap<String, Vec<String>>,
    sorted_ids: &'a [String],
}

/// 流式子图执行上下文，持有所有流式节点的 `StreamHandle` 与回调。
///
/// `handles` 含裸指针（`StreamHandle` 不实现 `Send`），故 `StreamCtx` 不可跨线程移动，
/// 必须在构造它的 `spawn_blocking` 线程内使用并销毁。
struct StreamCtx<'a> {
    handles: std::collections::HashMap<String, executor::StreamHandle>,
    streaming_out: &'a std::collections::HashMap<String, Vec<String>>,
    on_progress: &'a mut dyn FnMut(DagEvent),
    chunk_idx: std::collections::HashMap<String, u64>,
}

impl<'a> StreamCtx<'a> {
    fn emit_node_progress(&mut self, nr: DagNodeResult) {
        (self.on_progress)(DagEvent::NodeProgress(nr));
    }

    fn emit_chunk(&mut self, node_id: &str, chunk: PortData) {
        let idx = self.chunk_idx.entry(node_id.to_string()).or_insert(0);
        let cur = *idx;
        *idx += 1;
        (self.on_progress)(DagEvent::StreamChunk {
            node_id: node_id.to_string(),
            chunk,
            chunk_index: cur,
        });
    }

    /// push 一个 chunk 到 `node` 并排空其即时输出（直到 `next` 返回 None）。
    /// 每个输出 chunk 扇出到所有流式下游。
    fn push_and_drain(&mut self, node_id: &str, chunk: &PortData) -> Result<(), String> {
        {
            let h = self
                .handles
                .get_mut(node_id)
                .ok_or_else(|| format!("流式 handle 缺失: {}", node_id))?;
            h.push(chunk)?;
        }
        self.drain(node_id)
    }

    /// 排空 `node` 的即时输出（直到 `next` 返回 None），每个输出扇出到下游。
    fn drain(&mut self, node_id: &str) -> Result<(), String> {
        loop {
            let out = {
                let h = self
                    .handles
                    .get_mut(node_id)
                    .ok_or_else(|| format!("流式 handle 缺失: {}", node_id))?;
                h.next()?
            };
            match out {
                Some(out) => {
                    {
                        let h = self.handles.get_mut(node_id).unwrap();
                        h.accumulate(out.clone());
                    }
                    self.emit_chunk(node_id, out.clone());
                    let downs = self.streaming_out.get(node_id).cloned().unwrap_or_default();
                    for d in downs {
                        self.push_and_drain(&d, &out)?;
                    }
                }
                None => break,
            }
        }
        Ok(())
    }

    /// 排空 `node` 后向其下游 `push_end` 并递归排空（用于上游已 EOF 的级联）。
    fn drain_to_end(&mut self, node_id: &str) -> Result<(), String> {
        self.drain(node_id)?;
        let downs = self.streaming_out.get(node_id).cloned().unwrap_or_default();
        for d in downs {
            {
                let h = self
                    .handles
                    .get_mut(&d)
                    .ok_or_else(|| format!("流式 handle 缺失: {}", d))?;
                h.push_end()?;
            }
            self.drain_to_end(&d)?;
        }
        Ok(())
    }

    /// `push_end(node)` 后 `drain_to_end(node)`：用于上游 EOF 后通知下游并级联排空。
    fn push_end_then_drain_to_end(&mut self, node_id: &str) -> Result<(), String> {
        {
            let h = self
                .handles
                .get_mut(node_id)
                .ok_or_else(|| format!("流式 handle 缺失: {}", node_id))?;
            h.push_end()?;
        }
        self.drain_to_end(node_id)
    }

    /// 流式子图成功结束后：聚合各节点累积 chunk → 写入 `outputs_map`，发 Completed 进度。
    /// 消费 `self`（释放所有 handle）。
    fn finalize(
        mut self,
        state: &RuntimeState,
        plan: &StreamingPlan,
        subgraph_nodes: &[String],
        outputs_map: &mut std::collections::HashMap<String, Vec<PortData>>,
    ) -> Vec<DagNodeResult> {
        let mut node_results = Vec::new();
        for nid in subgraph_nodes {
            let node = *plan
                .node_map
                .get(nid)
                .expect("subgraph node missing in node_map");
            let acc = match self.handles.remove(nid) {
                Some(h) => h.into_accumulated(),
                None => Vec::new(),
            };
            let aggregated = aggregate_chunks(acc);
            let preview = aggregated.preview(PREVIEW_ROW_LIMIT);
            let row_count = aggregated.first_dataframe_row_count().unwrap_or(0);
            outputs_map.insert(nid.clone(), vec![aggregated]);
            let exec_result = OperatorExecutionResult::completed(
                vec![ExecutionLogEntry::info(format!("流式节点 {} ({}) 执行完成", nid, node.operator_name))],
                0,
            );
            state.update_execution_result(nid, exec_result.clone());
            let nr = DagNodeResult {
                node_id: nid.clone(),
                operator_name: node.operator_name.clone(),
                outputs: vec![preview],
                output_row_count: row_count,
                execution_result: exec_result,
            };
            self.emit_node_progress(nr.clone());
            node_results.push(nr);
        }
        node_results
    }
}

/// 执行以 `head_id` 为根的流式子图：start 全部成员 → 拉源 chunk 级联 propagate →
/// 源 EOF 后 push_end + drain_to_end → 聚合累积输出。
///
/// 返回 `(节点结果, 可选错误)`。错误时已为失败节点发 Failed 进度。所有 chunk 实时
/// 通过 `on_progress` 推送为 `DagEvent::StreamChunk`。
#[allow(clippy::too_many_arguments)]
fn execute_streaming_subgraph(
    state: &RuntimeState,
    dag: &DagDefinition,
    head_id: &str,
    plan: &StreamingPlan,
    outputs_map: &mut std::collections::HashMap<String, Vec<PortData>>,
    on_progress: &mut dyn FnMut(DagEvent),
) -> (Vec<DagNodeResult>, Option<String>) {
    let mut node_results: Vec<DagNodeResult> = Vec::new();
    let subgraph_set = collect_streaming_subgraph(head_id, plan.streaming_out);
    let subgraph_nodes: Vec<String> = plan
        .sorted_ids
        .iter()
        .filter(|id| subgraph_set.contains(id.as_str()))
        .cloned()
        .collect();

    let mut ctx = StreamCtx {
        handles: std::collections::HashMap::new(),
        streaming_out: plan.streaming_out,
        on_progress,
        chunk_idx: std::collections::HashMap::new(),
    };

    // ---- (1) start 全部成员（拓扑序）----
    for nid in &subgraph_nodes {
        let node = *plan
            .node_map
            .get(nid)
            .expect("subgraph node missing in node_map");
        // 收集输入：按 target_port 索引构造，长度 = node.input_count。流式上游端口与
        // 未连接端口填充占位（算子通过 push 接收流式 chunk，不应读取占位端口）；非流式
        // 上游端口物化。支持「一个输出端口 → 多个输入端口」的扇入与稀疏端口。
        let mut input_slots: Vec<Option<PortData>> = (0..node.input_count).map(|_| None).collect();
        let mut input_err: Option<String> = None;
        for edge in dag.edges.iter().filter(|e| e.target_node_id == *nid) {
            if edge.target_port >= node.input_count {
                input_err = Some(format!(
                    "节点 {} ({}) 的输入端口 {} 越界（input_count={}）",
                    node.id, node.operator_name, edge.target_port, node.input_count
                ));
                break;
            }
            if input_slots[edge.target_port].is_some() {
                input_err = Some(format!(
                    "节点 {} ({}) 的输入端口 {} 存在重复入边",
                    node.id, node.operator_name, edge.target_port
                ));
                break;
            }
            let src_streaming = plan
                .streaming_capable
                .get(&edge.source_node_id)
                .copied()
                .unwrap_or(false);
            if src_streaming {
                // 流式输入端口：占位（算子通过 push 接收实际 chunk）
                input_slots[edge.target_port] = Some(PortData::String(String::new()));
            } else {
                match outputs_map.get(&edge.source_node_id) {
                    Some(upstream_outputs) => match upstream_outputs.get(edge.source_port) {
                        Some(out) => input_slots[edge.target_port] = Some(out.clone()),
                        None => {
                            input_err = Some(format!(
                                "节点 {} ({}) 的上游节点 {} 输出端口 {} 不存在",
                                node.id, node.operator_name, edge.source_node_id, edge.source_port
                            ));
                            break;
                        }
                    },
                    None => {
                        input_err = Some(format!(
                            "节点 {} ({}) 的上游节点 {} 尚未执行或无输出",
                            node.id, node.operator_name, edge.source_node_id
                        ));
                        break;
                    }
                }
            }
        }
        let inputs: Vec<PortData> = input_slots
            .into_iter()
            .map(|slot| slot.unwrap_or_else(|| PortData::String(String::new())))
            .collect();
        if let Some(err) = input_err {
            let exec_result = OperatorExecutionResult::failed(Vec::new(), err.clone(), None);
            state.update_execution_result(&node.id, exec_result.clone());
            let nr = DagNodeResult {
                node_id: node.id.clone(),
                operator_name: node.operator_name.clone(),
                outputs: Vec::new(),
                output_row_count: 0,
                execution_result: exec_result,
            };
            ctx.emit_node_progress(nr.clone());
            node_results.push(nr);
            return (node_results, Some(err));
        }

        let dll = plan
            .node_dll
            .get(nid)
            .cloned()
            .unwrap_or_else(|| PathBuf::from(""));

        state.set_executing(&node.id);
        ctx.emit_node_progress(DagNodeResult {
            node_id: node.id.clone(),
            operator_name: node.operator_name.clone(),
            outputs: Vec::new(),
            output_row_count: 0,
            execution_result: OperatorExecutionResult::executing(),
        });

        match executor::StreamHandle::start(&dll, &inputs, &node.params_json) {
            Ok(h) => {
                ctx.handles.insert(node.id.clone(), h);
            }
            Err(e) => {
                let exec_result = OperatorExecutionResult::failed(Vec::new(), e.clone(), None);
                state.update_execution_result(&node.id, exec_result.clone());
                let nr = DagNodeResult {
                    node_id: node.id.clone(),
                    operator_name: node.operator_name.clone(),
                    outputs: Vec::new(),
                    output_row_count: 0,
                    execution_result: exec_result,
                };
                ctx.emit_node_progress(nr.clone());
                node_results.push(nr);
                return (node_results, Some(e));
            }
        }
    }

    // ---- (2) head 拉取 + 级联 propagate ----
    let mut stream_err: Option<(String, String)> = None;
    'head: loop {
        let out = {
            let h = match ctx.handles.get_mut(head_id) {
                Some(h) => h,
                None => {
                    stream_err = Some((head_id.to_string(), format!("流式链头 {} handle 缺失", head_id)));
                    break 'head;
                }
            };
            match h.next() {
                Ok(o) => o,
                Err(e) => {
                    stream_err = Some((head_id.to_string(), e));
                    break 'head;
                }
            }
        };
        match out {
            Some(out) => {
                {
                    let h = ctx.handles.get_mut(head_id).unwrap();
                    h.accumulate(out.clone());
                }
                ctx.emit_chunk(head_id, out.clone());
                let downs = ctx.streaming_out.get(head_id).cloned().unwrap_or_default();
                for d in downs {
                    if let Err(e) = ctx.push_and_drain(&d, &out) {
                        stream_err = Some((d.clone(), e));
                        break 'head;
                    }
                }
            }
            None => break 'head,
        }
    }

    // ---- (3) head EOF → push_end 下游 + drain_to_end 级联 ----
    if stream_err.is_none() {
        let downs = ctx.streaming_out.get(head_id).cloned().unwrap_or_default();
        for d in downs {
            if let Err(e) = ctx.push_end_then_drain_to_end(&d) {
                stream_err = Some((d.clone(), e));
                break;
            }
        }
    }

    // ---- 错误处理：为失败节点发 Failed ----
    if let Some((fail_node, err)) = stream_err {
        let op_name = plan
            .node_map
            .get(&fail_node)
            .map(|n| n.operator_name.clone())
            .unwrap_or_default();
        let exec_result = OperatorExecutionResult::failed(Vec::new(), err.clone(), None);
        state.update_execution_result(&fail_node, exec_result.clone());
        let nr = DagNodeResult {
            node_id: fail_node.clone(),
            operator_name: op_name,
            outputs: Vec::new(),
            output_row_count: 0,
            execution_result: exec_result,
        };
        ctx.emit_node_progress(nr.clone());
        node_results.push(nr);
        return (node_results, Some(err));
    }

    // ---- (4)(5) 聚合累积输出 + Completed ----
    node_results.extend(ctx.finalize(state, plan, &subgraph_nodes, outputs_map));
    (node_results, None)
}

/// 解析并执行完整 DAG。
///
/// 按拓扑序逐节点执行：
/// - **流式子图**：以流式链头为根，通过 [`execute_streaming_subgraph`] 拉取 chunk 并
///   级联传播给下游流式节点，实时推送 `DagEvent::StreamChunk`；子图内节点由该函数
///   一次性执行完毕，主循环跳过已被覆盖的节点。
/// - **批量节点**：收集入边对应的上游输出作为输入，调用 `executor::execute_operator`
///   执行，缓存输出供下游节点使用。
///
/// 任一节点失败则停止后续执行，已完成的节点结果仍包含在返回值中。
fn execute_dag<F>(
    state: &RuntimeState,
    dag: &DagDefinition,
    mut on_progress: F,
) -> DagExecutionResult
where
    F: FnMut(DagEvent),
{
    let start = std::time::Instant::now();

    let sorted_ids = match topological_sort_dag(dag) {
        Ok(ids) => ids,
        Err(e) => {
            return DagExecutionResult {
                status: OperatorExecutionStatus::Failed,
                node_results: Vec::new(),
                total_duration_ms: start.elapsed().as_millis() as u64,
                error_message: Some(e),
            };
        }
    };

    let mut node_map: std::collections::HashMap<&String, &DagNodeDef> = std::collections::HashMap::new();
    for n in &dag.nodes {
        node_map.insert(&n.id, n);
    }

    // ---- 预解析所有节点 DLL + 探测流式能力 ----
    let mut node_dll: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    let mut streaming_capable: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut early_failure: Option<(DagNodeResult, String)> = None;

    for nid in &sorted_ids {
        let node = match node_map.get(nid) {
            Some(n) => *n,
            None => {
                let err = format!("节点 {} 不存在于 DAG 定义中", nid);
                let nr = DagNodeResult {
                    node_id: nid.clone(),
                    operator_name: String::new(),
                    outputs: Vec::new(),
                    output_row_count: 0,
                    execution_result: OperatorExecutionResult::failed(Vec::new(), err.clone(), None),
                };
                early_failure = Some((nr, err));
                break;
            }
        };
        let dll_path = match resolve_node_dll(state, node) {
            Ok(p) => p,
            Err(e) => {
                let exec_result = OperatorExecutionResult::failed(Vec::new(), e.clone(), None);
                let nr = DagNodeResult {
                    node_id: node.id.clone(),
                    operator_name: node.operator_name.clone(),
                    outputs: Vec::new(),
                    output_row_count: 0,
                    execution_result: exec_result,
                };
                early_failure = Some((nr, e));
                break;
            }
        };

        // 探测流式能力：node.stream=true 且 DLL 导出 5 个流式符号时才视为流式节点；
        // 探测失败或未导出符号则降级为批量执行（仅打印告警，不中断整体流程）。
        let capable = if node.stream {
            match executor::probe_streaming_operator(&dll_path) {
                Ok(Some(_)) => true,
                Ok(None) => {
                    eprintln!(
                        "[runtime] 节点 {} ({}) 声明 stream=true 但 DLL 未导出流式符号，降级为批量执行",
                        node.id, node.operator_name
                    );
                    false
                }
                Err(e) => {
                    eprintln!(
                        "[runtime] 节点 {} ({}) 流式探测失败: {}，降级为批量执行",
                        node.id, node.operator_name, e
                    );
                    false
                }
            }
        } else {
            false
        };
        node_dll.insert(nid.clone(), dll_path);
        streaming_capable.insert(nid.clone(), capable);
    }

    if let Some((nr, err)) = early_failure {
        on_progress(DagEvent::NodeProgress(nr.clone()));
        return DagExecutionResult {
            status: OperatorExecutionStatus::Failed,
            node_results: vec![nr],
            total_duration_ms: start.elapsed().as_millis() as u64,
            error_message: Some(err),
        };
    }

    // ---- 构造流式边 (streaming_out) 与流式链头集合 ----
    // 仅当 source 与 target 均为流式节点时，该边才视为流式边（chunk 沿其传播）；
    // 否则视为普通批量边，下游从 outputs_map 取聚合后的完整输出。
    let mut streaming_out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut streaming_targets: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &dag.edges {
        let src_cap = streaming_capable.get(&e.source_node_id).copied().unwrap_or(false);
        let tgt_cap = streaming_capable.get(&e.target_node_id).copied().unwrap_or(false);
        if src_cap && tgt_cap {
            // 同一源节点到同一目标节点的多条流式边（如「一个输出端口 → 多个输入端口」
            // 或同一对节点间的多条连线）只保留一条：流式 push 不区分端口，重复入队
            // 会导致目标节点对同一个 chunk 收到多次推送。跨节点的扇出仍由下游列表的
            // 多个不同目标节点自然完成。
            let downs = streaming_out.entry(e.source_node_id.clone()).or_default();
            if !downs.contains(&e.target_node_id) {
                downs.push(e.target_node_id.clone());
            }
            streaming_targets.insert(e.target_node_id.clone());
        }
    }

    let plan = StreamingPlan {
        node_map: &node_map,
        node_dll: &node_dll,
        streaming_capable: &streaming_capable,
        streaming_out: &streaming_out,
        sorted_ids: &sorted_ids,
    };

    // 各节点输出缓存（node_id -> outputs），供下游节点取输入
    let mut outputs_map: std::collections::HashMap<String, Vec<PortData>> = std::collections::HashMap::new();
    let mut node_results: Vec<DagNodeResult> = Vec::new();
    let mut failed_error: Option<String> = None;
    // 已被流式子图覆盖的节点（主循环跳过，避免重复执行）
    let mut executed: std::collections::HashSet<String> = std::collections::HashSet::new();

    for node_id in &sorted_ids {
        // 流式子图覆盖的节点（非头）已在头节点执行时处理完毕，直接跳过
        if executed.contains(node_id) {
            continue;
        }

        // 流式链头：流式节点且自身不是任何流式边的 target → 执行整个流式子图
        let capable = streaming_capable.get(node_id).copied().unwrap_or(false);
        let is_head = capable && !streaming_targets.contains(node_id);
        if is_head {
            let (sub_results, sub_err) = execute_streaming_subgraph(
                state,
                dag,
                node_id,
                &plan,
                &mut outputs_map,
                &mut on_progress,
            );
            node_results.extend(sub_results);
            for nid in collect_streaming_subgraph(node_id, &streaming_out) {
                executed.insert(nid);
            }
            if let Some(err) = sub_err {
                failed_error = Some(err);
                break;
            }
            continue;
        }

        let node = match node_map.get(node_id) {
            Some(n) => *n,
            None => {
                failed_error = Some(format!("节点 {} 不存在于 DAG 定义中", node_id));
                break;
            }
        };

        // 批量路径：按 target_port 索引构造输入向量，长度 = node.input_count，未连接
        // 的端口填充空 String 占位。这样「一个输出端口 → 多个输入端口」（跨节点扇出、
        // 同节点多端口扇入、稀疏端口）都能正确对齐到算子期望的端口下标，避免旧实现
        // 按 target_port 排序后顺序 push 导致的端口错位与长度不符。
        let mut input_slots: Vec<Option<PortData>> = (0..node.input_count).map(|_| None).collect();
        let mut input_error: Option<String> = None;
        for edge in dag.edges.iter().filter(|e| e.target_node_id == node.id) {
            if edge.target_port >= node.input_count {
                input_error = Some(format!(
                    "节点 {} ({}) 的输入端口 {} 越界（input_count={}）",
                    node.id, node.operator_name, edge.target_port, node.input_count
                ));
                break;
            }
            if input_slots[edge.target_port].is_some() {
                input_error = Some(format!(
                    "节点 {} ({}) 的输入端口 {} 存在重复入边",
                    node.id, node.operator_name, edge.target_port
                ));
                break;
            }
            match outputs_map.get(&edge.source_node_id) {
                Some(upstream_outputs) => match upstream_outputs.get(edge.source_port) {
                    Some(out) => input_slots[edge.target_port] = Some(out.clone()),
                    None => {
                        input_error = Some(format!(
                            "节点 {} ({}) 的上游节点 {} 输出端口 {} 不存在",
                            node.id, node.operator_name, edge.source_node_id, edge.source_port
                        ));
                        break;
                    }
                },
                None => {
                    input_error = Some(format!(
                        "节点 {} ({}) 的上游节点 {} 尚未执行或无输出",
                        node.id, node.operator_name, edge.source_node_id
                    ));
                    break;
                }
            }
        }

        if let Some(err) = input_error {
            let exec_result = OperatorExecutionResult::failed(Vec::new(), err.clone(), None);
            let nr = DagNodeResult {
                node_id: node.id.clone(),
                operator_name: node.operator_name.clone(),
                // 仅回传预览（此处无输出），完整数据保留在服务端内存供下游消费
                outputs: Vec::new(),
                output_row_count: 0,
                execution_result: exec_result,
            };
            on_progress(DagEvent::NodeProgress(nr.clone()));
            node_results.push(nr);
            failed_error = Some(err);
            break;
        }

        // 未连接的端口填充空 String 占位；算子按端口下标读取输入，占位端口不应被消费
        let inputs: Vec<PortData> = input_slots
            .into_iter()
            .map(|slot| slot.unwrap_or_else(|| PortData::String(String::new())))
            .collect();

        // DLL 已在预扫描阶段解析完毕（失败已在 early_failure 提前返回），此处直接取缓存
        let dll_path = node_dll.get(node_id).cloned().unwrap_or_else(|| PathBuf::from(""));

        // 标记正在执行
        state.set_executing(&node.id);

        // 推送 Executing 进度帧，让前端实时看到当前运行到哪个算子
        on_progress(DagEvent::NodeProgress(DagNodeResult {
            node_id: node.id.clone(),
            operator_name: node.operator_name.clone(),
            outputs: Vec::new(),
            output_row_count: 0,
            execution_result: OperatorExecutionResult::executing(),
        }));

        // 执行算子
        let node_start = std::time::Instant::now();
        let mut logs = Vec::new();
        logs.push(ExecutionLogEntry::info(format!(
            "开始执行节点 {} ({})", node.id, node.operator_name
        )));
        logs.push(ExecutionLogEntry::debug(format!("DLL: {}", dll_path.display())));
        logs.push(ExecutionLogEntry::debug(format!(
            "输入数量: {}, 最大输出: {}, 参数长度: {}",
            inputs.len(),
            node.output_count,
            node.params_json.len()
        )));

        let exec_out = executor::execute_operator(&dll_path, &inputs, node.output_count, &node.params_json);
        let duration_ms = node_start.elapsed().as_millis() as u64;

        let (outputs, execution_result) = match exec_out {
            Ok(outs) => {
                logs.push(ExecutionLogEntry::debug(format!("输出数量: {}", outs.len())));
                let r = OperatorExecutionResult::completed(logs, duration_ms);
                (outs, r)
            }
            Err(e) => {
                logs.push(ExecutionLogEntry::error(format!("执行错误: {}", e)));
                let r = OperatorExecutionResult::failed(logs, e.clone(), Some(duration_ms));
                (Vec::new(), r)
            }
        };

        // 更新服务端执行记录（供 QueryExecutionStatus/Logs 查询）
        state.update_execution_result(&node.id, execution_result.clone());

        let failed = execution_result.status == OperatorExecutionStatus::Failed;

        // 回传给客户端的仅是预览（前 PREVIEW_ROW_LIMIT 行），完整 outputs 保留在服务端
        // 内存中由 outputs_map 传递给下游算子，避免大数据量导致响应超过帧大小上限。
        let preview_outputs: Vec<PortData> = outputs
            .iter()
            .map(|p| p.preview(PREVIEW_ROW_LIMIT))
            .collect();
        let output_row_count = outputs
            .iter()
            .find_map(|p| p.first_dataframe_row_count())
            .unwrap_or(0);

        let nr = DagNodeResult {
            node_id: node.id.clone(),
            operator_name: node.operator_name.clone(),
            outputs: preview_outputs,
            output_row_count,
            execution_result,
        };
        // 推送 Completed/Failed 进度帧（与最终 node_results 内容完全一致，复用同一变量）
        on_progress(DagEvent::NodeProgress(nr.clone()));
        node_results.push(nr);

        if failed {
            failed_error = Some(format!("节点 {} ({}) 执行失败", node.id, node.operator_name));
            break;
        } else {
            // 完整输出（未截断）保留在服务端内存中，供下游节点消费
            outputs_map.insert(node.id.clone(), outputs);
        }
    }

    let total_duration_ms = start.elapsed().as_millis() as u64;
    let status = if failed_error.is_some() {
        OperatorExecutionStatus::Failed
    } else {
        OperatorExecutionStatus::Completed
    };

    DagExecutionResult {
        status,
        node_results,
        total_duration_ms,
        error_message: failed_error,
    }
}

/// 独立流式执行（`ExecuteStream`）的事件流，由 `run_standalone_stream` 在
/// `spawn_blocking` 线程内产出，`handle_client` 的 async 端消费并转为响应帧。
enum StreamEvent {
    /// 产出一个 chunk（实时推送）
    Chunk { chunk: PortData, chunk_index: u64 },
    /// 流结束（成功或失败），携带最终聚合输出与执行结果（终止帧）
    Done {
        execution_result: OperatorExecutionResult,
        final_outputs: Vec<PortData>,
        output_row_count: usize,
    },
}

/// 独立单节点流式执行的同步主体（运行在 `spawn_blocking` 线程内）。
///
/// 流程：`StreamHandle::start` → 循环 `next` 产出 chunk（实时推送 + 累积）→
/// 聚合累积 chunk → 推送 `StreamEvent::Done`。任一步失败时推送带 Failed 状态的 Done。
///
/// `StreamHandle` 持有裸指针、不可跨线程移动，故本函数必须在与构造它相同的线程内
/// 使用并销毁 handle（由调用方保证 `spawn_blocking`）。
fn run_standalone_stream(
    state: &RuntimeState,
    dll_path: &Path,
    operator_id: &str,
    inputs: &[PortData],
    params_json: &str,
    ev_tx: tokio::sync::mpsc::Sender<StreamEvent>,
) {
    state.set_executing(operator_id);

    let mut handle = match executor::StreamHandle::start(dll_path, inputs, params_json) {
        Ok(h) => h,
        Err(e) => {
            let exec_result = OperatorExecutionResult::failed(Vec::new(), e.clone(), None);
            state.update_execution_result(operator_id, exec_result.clone());
            let _ = ev_tx.blocking_send(StreamEvent::Done {
                execution_result: exec_result,
                final_outputs: Vec::new(),
                output_row_count: 0,
            });
            return;
        }
    };

    let start = std::time::Instant::now();
    let mut chunk_index = 0u64;
    loop {
        match handle.next() {
            Ok(Some(chunk)) => {
                handle.accumulate(chunk.clone());
                let _ = ev_tx.blocking_send(StreamEvent::Chunk { chunk, chunk_index });
                chunk_index += 1;
            }
            Ok(None) => break,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let exec_result =
                    OperatorExecutionResult::failed(Vec::new(), e.clone(), Some(duration_ms));
                state.update_execution_result(operator_id, exec_result.clone());
                let _ = ev_tx.blocking_send(StreamEvent::Done {
                    execution_result: exec_result,
                    final_outputs: Vec::new(),
                    output_row_count: 0,
                });
                return;
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let accumulated = handle.into_accumulated();
    let aggregated = aggregate_chunks(accumulated);
    let output_row_count = aggregated.first_dataframe_row_count().unwrap_or(0);
    let preview = aggregated.preview(PREVIEW_ROW_LIMIT);
    let exec_result = OperatorExecutionResult::completed(
        vec![ExecutionLogEntry::info(format!(
            "流式执行完成，共 {} 个 chunk",
            chunk_index
        ))],
        duration_ms,
    );
    state.update_execution_result(operator_id, exec_result.clone());
    let _ = ev_tx.blocking_send(StreamEvent::Done {
        execution_result: exec_result,
        final_outputs: vec![preview],
        output_row_count,
    });
}

/// 处理单个客户端连接
async fn handle_client(state: Arc<RuntimeState>, mut stream: TcpStream) {
    let peer = stream.peer_addr().ok();
    println!("[runtime] 客户端连接: {:?}", peer);

    loop {
        let frame = match read_frame(&mut stream).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                // 客户端在帧边界处正常关闭连接（一次性客户端常见），安静退出
                return;
            }
            Err(e) => {
                println!("[runtime] 读取帧失败: {}", e);
                return;
            }
        };

        let request: RuntimeRequest = match from_str(&frame) {
            Ok(r) => r,
            Err(e) => {
                println!("[runtime] 解析请求失败: {}", e);
                let response = RuntimeResponse::Error {
                    request_id: 0,
                    message: format!("JSON 解析失败: {}", e),
                };
                let json = match to_string(&response) {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                let _ = write_frame(&mut stream, &json).await;
                continue;
            }
        };

        // 拦截 ExecuteDag：流式推送节点进度（DagNodeProgress）与流式 chunk
        // （StreamChunk），最后推送终止帧 DagExecuted。其他请求仍走下方
        // spawn_blocking(handle_request) 单响应路径。
        if let RuntimeRequest::ExecuteDag { dag } = request {
            let request_id = state.alloc_request_id();
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<DagEvent>(32);
            let state_clone = state.clone();

            // spawn_blocking 跑同步 execute_dag，事件通过 ev_tx.blocking_send 推出；
            // 闭包退出时 ev_tx 随之 drop，async 端 recv 循环将收到 None 而退出。
            let exec_join = tokio::task::spawn_blocking(move || {
                execute_dag(&state_clone, &dag, |ev| {
                    // 接收端断开（写帧失败）时返回 Err，忽略即可，让 execute_dag 跑完
                    let _ = ev_tx.blocking_send(ev);
                })
            });

            // async 端：把 DagEvent 分派为 DagNodeProgress / StreamChunk 响应帧；
            // 写帧失败后标记并继续消费，避免 spawn_blocking 悬挂
            let mut write_failed = false;
            while let Some(ev) = ev_rx.recv().await {
                if write_failed {
                    continue;
                }
                let resp = match ev {
                    DagEvent::NodeProgress(progress) => {
                        RuntimeResponse::DagNodeProgress { request_id, progress }
                    }
                    DagEvent::StreamChunk { node_id, chunk, chunk_index } => {
                        RuntimeResponse::StreamChunk {
                            request_id,
                            node_id,
                            chunk,
                            chunk_index,
                            is_final: false,
                        }
                    }
                };
                match to_string(&resp) {
                    Ok(json) => {
                        if let Err(e) = write_frame(&mut stream, &json).await {
                            println!("[runtime] 推送进度帧失败: {}", e);
                            write_failed = true;
                        }
                    }
                    Err(e) => {
                        println!("[runtime] 序列化响应失败: {}", e);
                        write_failed = true;
                    }
                }
            }

            // 等待 execute_dag 完成，推送终止帧
            let result = match exec_join.await {
                Ok(r) => r,
                Err(e) => {
                    println!("[runtime] execute_dag 任务 panic: {}", e);
                    DagExecutionResult {
                        status: OperatorExecutionStatus::Failed,
                        node_results: Vec::new(),
                        total_duration_ms: 0,
                        error_message: Some(format!("execute_dag 任务异常: {}", e)),
                    }
                }
            };
            let final_resp = RuntimeResponse::DagExecuted { request_id, result };
            match to_string(&final_resp) {
                Ok(json) => {
                    let _ = write_frame(&mut stream, &json).await;
                }
                Err(e) => {
                    println!("[runtime] 序列化终止帧失败: {}", e);
                }
            }
            continue;
        }

        // 拦截 ExecuteStream：独立单节点流式执行，逐 chunk 推送 StreamChunk，
        // 最后推送 StreamCompleted 终止帧。算子必须导出 5 个流式符号，否则
        // StreamHandle::start 失败 → 推送带 Failed 状态的 StreamCompleted。
        if let RuntimeRequest::ExecuteStream { operator_id, inputs, params_json, .. } = request {
            let request_id = state.alloc_request_id();
            // 解析 DLL：优先已加载注册表，其次按名在算子库目录查找
            let dll_path = match state
                .find_operator(&operator_id)
                .or_else(|| state.find_operator_dll_by_name(&operator_id))
            {
                Some(p) if p.exists() => p,
                _ => {
                    let msg = format!("算子未加载或不存在: {}", operator_id);
                    let resp = RuntimeResponse::Error { request_id, message: msg };
                    if let Ok(json) = to_string(&resp) {
                        let _ = write_frame(&mut stream, &json).await;
                    }
                    continue;
                }
            };

            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<StreamEvent>(32);
            let state_clone = state.clone();
            let op_id = operator_id.clone();
            let exec_join = tokio::task::spawn_blocking(move || {
                run_standalone_stream(&state_clone, &dll_path, &op_id, &inputs, &params_json, ev_tx)
            });

            // async 端：把 StreamEvent 分派为 StreamChunk / StreamCompleted 响应帧
            let mut write_failed = false;
            while let Some(ev) = ev_rx.recv().await {
                if write_failed {
                    continue;
                }
                let resp = match ev {
                    StreamEvent::Chunk { chunk, chunk_index } => RuntimeResponse::StreamChunk {
                        request_id,
                        node_id: operator_id.clone(),
                        chunk,
                        chunk_index,
                        is_final: false,
                    },
                    StreamEvent::Done { execution_result, final_outputs, output_row_count } => {
                        RuntimeResponse::StreamCompleted {
                            request_id,
                            node_id: operator_id.clone(),
                            execution_result,
                            final_outputs,
                            output_row_count,
                        }
                    }
                };
                match to_string(&resp) {
                    Ok(json) => {
                        if let Err(e) = write_frame(&mut stream, &json).await {
                            println!("[runtime] 推送流式帧失败: {}", e);
                            write_failed = true;
                        }
                    }
                    Err(e) => {
                        println!("[runtime] 序列化流式响应失败: {}", e);
                        write_failed = true;
                    }
                }
            }
            // 等待 spawn_blocking 结束（资源回收），结果已通过 StreamEvent::Done 推送
            if let Err(e) = exec_join.await {
                println!("[runtime] run_standalone_stream 任务 panic: {}", e);
            }
            continue;
        }

        let is_shutdown = matches!(request, RuntimeRequest::Shutdown);

        let state_clone = state.clone();
        let response_json = tokio::task::spawn_blocking(move || {
            let response = handle_request(state_clone, request);
            to_string(&response).map_err(|e| format!("序列化响应失败: {}", e))
        })
        .await;

        let response_json = match response_json {
            Ok(Ok(j)) => j,
            Ok(Err(e)) => {
                println!("[runtime] 序列化响应失败: {}", e);
                return;
            }
            Err(e) => {
                println!("[runtime] spawn_blocking 失败: {}", e);
                return;
            }
        };

        if let Err(e) = write_frame(&mut stream, &response_json).await {
            println!("[runtime] 写入响应失败: {}", e);
            return;
        }

        if is_shutdown {
            println!("[runtime] 收到关闭请求，正在关闭...");
            return;
        }
    }
}

/// 打印帮助
fn print_usage() {
    println!("operator_runtime_server v{}", VERSION);
    println!("用法:");
    println!("  operator_runtime_server [地址]");
    println!();
    println!("参数:");
    println!("  地址    监听地址，默认 {}", DEFAULT_ADDR);
    println!();
    println!("环境变量:");
    println!("  RUNTIME_ADDR     监听地址");
    println!("  RUNTIME_PORT     监听端口 (覆盖地址中的端口)");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && (args[1] == "-h" || args[1] == "--help") {
        print_usage();
        return;
    }

    let addr = if let Some(addr) = std::env::var("RUNTIME_ADDR").ok() {
        addr
    } else if args.len() > 1 {
        args[1].clone()
    } else {
        DEFAULT_ADDR.to_string()
    };

    let port = std::env::var("RUNTIME_PORT").ok();
    let listen_addr = if let Some(p) = port {
        format!("{}:{}", addr.split(':').next().unwrap_or("127.0.0.1"), p)
    } else {
        addr
    };

    let compile_dir = std::env::var("RUNTIME_COMPILE_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("temp_compile")
        });

    if let Err(e) = std::fs::create_dir_all(&compile_dir) {
        eprintln!("[runtime] 创建编译目录失败: {}", e);
        return;
    }

    // 算子库目录
    let lib_dir = std::env::var("RUNTIME_LIB_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("lib")
        });

    // 初始化 runtime (确保 operator_runtime DLL 已加载)
    if let Err(e) = executor::ensure_runtime_loaded() {
        eprintln!("[runtime] 加载 operator_runtime DLL 失败: {}", e);
        eprintln!("[runtime] 请确保 operator_runtime.dll 位于可搜索路径中");
        return;
    }

    let state = Arc::new(RuntimeState::new(compile_dir, lib_dir.clone()));

    println!("[runtime] operator_runtime_server v{}", VERSION);
    println!("[runtime] 监听地址: {}", listen_addr);
    println!("[runtime] 编译目录: {}", state.compile_dir.display());
    println!("[runtime] 算子库目录: {}", state.lib_dir.display());

    // 服务启动时一次性预加载算子库目录下所有算子 DLL。
    //
    // 这样运行时执行节点直接复用已缓存的执行函数指针，避免每次执行都
    // `Library::new` 重新加载（消除 `[operator_exec] 加载算子 DLL` 日志刷屏，
    // 同时减少 IO 与动态库锁开销）。
    let categories = state.scan_operators();
    let (loaded, failed) = preload_all_categories(&categories);
    println!(
        "[runtime] 预加载算子完成: {} 个成功, {} 个失败",
        loaded, failed
    );

    let listener = match TcpListener::bind(&listen_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[runtime] 绑定端口失败: {}", e);
            return;
        }
    };

    println!("[runtime] 服务已启动，等待客户端连接...");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    handle_client(state, stream).await;
                });
            }
            Err(e) => {
                eprintln!("[runtime] 接受连接失败: {}", e);
            }
        }
    }
}
