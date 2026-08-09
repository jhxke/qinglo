use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use operator_executor_client::{
    self as executor_lib,
    runtime_client::{RuntimeClient, RuntimeClientError, DEFAULT_RUNTIME_ADDR, ExecuteResult},
    CargoBuildResult,
};
use operator_executor_client::protocol::{
    OperatorExecutionStatus,
    DagDefinition, DagNodeDef, DagEdgeDef, DagExecutionResult, DagNodeResult,
};
use operator_executor_client::PortData;

use crate::config::get_compile_directory;
use crate::dag::{
    DagGraph, Node, OperatorType, NodeIORegistry,
    OperatorPortParamDef, PortDirection, CustomOperatorDef, ParamType,
};

/// 全局 Runtime 客户端（持久连接，所有请求复用以避免端口不断变化的短连接）。
///
/// 用 `Arc` 包裹：调用方克隆出引用后即可释放全局锁，使调用期间不阻塞心跳线程
/// 与其他调用方；真正的 TCP 收发由 `RuntimeClient` 内部 stream 锁串行化。
static RUNTIME_CLIENT: once_cell::sync::Lazy<Mutex<Option<Arc<RuntimeClient>>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

/// Runtime 进程句柄 (可选)
static RUNTIME_PROCESS: once_cell::sync::Lazy<Mutex<Option<std::process::Child>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

/// Runtime 是否已尝试启动过
static RUNTIME_START_ATTEMPTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 心跳间隔：后台线程周期性 ping runtime 服务，保活持久连接并及时发现失效。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// 心跳线程启动标记（仅启动一次）
static HEARTBEAT_STARTED: Once = Once::new();

/// 对 runtime 客户端执行操作（复用全局持久连接）。
///
/// 所有 runtime 访问都应走本函数，复用同一条 TCP 连接，避免各自 `RuntimeClient::new`
/// 产生端口不断变化的短连接。调用期间不持有全局锁，与心跳线程互不阻塞；真正的
/// TCP 收发由 `RuntimeClient` 内部 stream 锁串行化。
///
/// 若调用因 IO 错误失败（连接已失效），会主动断开持久连接，让下一次调用或心跳
/// 重新建立连接。
pub fn with_runtime_client<F, R>(f: F) -> Result<R, RuntimeClientError>
where
    F: FnOnce(&RuntimeClient) -> Result<R, RuntimeClientError>,
{
    ensure_heartbeat_started();
    let client = ensure_connected_client()?;
    let result = f(&client);
    if matches!(
        result,
        Err(RuntimeClientError::Io(_)) | Err(RuntimeClientError::ConnectionFailed(_))
    ) {
        // 持久连接可能已失效（服务端重启 / 网络中断）：主动断开，
        // 让下次调用 / 心跳重新建立连接，避免持续向死连接写入。
        client.disconnect();
    }
    result
}

/// 获取一个已连接的全局客户端（`Arc` 克隆，调用期间不持有全局锁）。
///
/// 若全局客户端不存在或未连接，则建立连接；首次连接失败时尝试拉起 runtime 服务
/// 后再连一次。
fn ensure_connected_client() -> Result<Arc<RuntimeClient>, RuntimeClientError> {
    // 快路径：已有且已连接
    {
        let guard = RUNTIME_CLIENT.lock().unwrap();
        if let Some(client) = guard.as_ref() {
            if client.is_connected() {
                return Ok(client.clone());
            }
        }
    }

    // 慢路径：取/建全局客户端，建立连接（必要时先拉起服务）
    let client = {
        let mut guard = RUNTIME_CLIENT.lock().unwrap();
        guard
            .get_or_insert_with(|| Arc::new(RuntimeClient::new(DEFAULT_RUNTIME_ADDR)))
            .clone()
    };
    if client.is_connected() {
        return Ok(client);
    }
    match client.connect() {
        Ok(()) => Ok(client),
        Err(first_err) => {
            // 连接失败：可能是服务未启动，尝试拉起后再连一次
            try_start_runtime();
            match client.connect() {
                Ok(()) => Ok(client),
                Err(_) => Err(first_err),
            }
        }
    }
}

/// 启动后台心跳线程（仅一次）。
fn ensure_heartbeat_started() {
    HEARTBEAT_STARTED.call_once(|| {
        std::thread::Builder::new()
            .name("runtime-heartbeat".into())
            .spawn(heartbeat_loop)
            .expect("启动 runtime 心跳线程失败");
    });
}

/// 心跳循环：周期性 ping 全局客户端以保活；失败则断开连接触发重连。
fn heartbeat_loop() {
    loop {
        std::thread::sleep(HEARTBEAT_INTERVAL);
        let client = RUNTIME_CLIENT
            .lock()
            .unwrap()
            .as_ref()
            .map(Arc::clone);
        let Some(client) = client else {
            continue; // 尚未建立过连接，无需保活
        };
        if !client.is_connected() {
            continue; // 已断开，交给下次实际调用重建
        }
        match client.ping() {
            Ok(_) => { /* 连接健康 */ }
            Err(e) => {
                eprintln!("[operator_executor] 心跳失败，断开持久连接以触发重连: {}", e);
                client.disconnect();
            }
        }
    }
}

/// 尝试启动 runtime 子进程。
///
/// 注意：本函数不再用一次性 `RuntimeClient::new().ping()` 探测（那会产生新的短连接，
/// 是客户端端口不断变化的来源之一）。连接探测统一由 [`ensure_connected_client`]
/// 的 `connect()` 完成：仅在 `connect()` 失败时才调用本函数拉起服务。
fn try_start_runtime() {
    if RUNTIME_START_ATTEMPTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return; // 已在尝试
    }

    // 尝试查找并启动 operator_runtime_server
    let search_paths = get_runtime_search_paths();

    for path in &search_paths {
        if path.exists() {
            let compile_dir = get_compile_directory();
            match executor_lib::runtime_client::spawn_runtime_server(path, DEFAULT_RUNTIME_ADDR, &compile_dir) {
                Ok(child) => {
                    println!("[operator_executor] 启动 runtime 进程成功: {:?}", child.id());
                    *RUNTIME_PROCESS.lock().unwrap() = Some(child);
                    // 等待 runtime 就绪
                    std::thread::sleep(Duration::from_millis(500));
                    break;
                }
                Err(e) => {
                    eprintln!("[operator_executor] 启动 runtime 失败: {}", e);
                }
            }
        }
    }

    RUNTIME_START_ATTEMPTED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// 获取 runtime 可执行文件的搜索路径
fn get_runtime_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            paths.push(exe_dir.join("operator_runtime_server"));
            paths.push(exe_dir.join("operator_runtime_server.exe"));
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join("target").join("debug").join("operator_runtime_server"));
        paths.push(cwd.join("target").join("debug").join("operator_runtime_server.exe"));
        paths.push(cwd.join("target").join("release").join("operator_runtime_server"));
        paths.push(cwd.join("target").join("release").join("operator_runtime_server.exe"));
        paths.push(cwd.join("operator_runtime").join("target").join("debug").join("operator_runtime_server"));
        paths.push(cwd.join("operator_runtime").join("target").join("debug").join("operator_runtime_server.exe"));
        paths.push(cwd.join("operator_runtime").join("target").join("release").join("operator_runtime_server"));
        paths.push(cwd.join("operator_runtime").join("target").join("release").join("operator_runtime_server.exe"));
    }

    // 搜索 target 下的 deps 目录
    if let Ok(cwd) = std::env::current_dir() {
        let deps_dir = cwd.join("target").join("debug").join("deps");
        if let Ok(entries) = fs::read_dir(&deps_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("operator_runtime_server"))
                    .unwrap_or(false)
                {
                    paths.push(path);
                }
            }
        }
    }

    paths
}

/// 根据算子定义和参数定义生成完整的代码（注入参数常量和运行时导入）
pub fn inject_params_into_code(code: &str, params: &[&OperatorPortParamDef]) -> String {
    let params_vec: Vec<executor_lib::OperatorPortParamDef> = params
        .iter()
        .map(|p| executor_lib::OperatorPortParamDef {
            name: p.name.clone(),
            param_type: match p.param_type {
                crate::dag::ParamType::Float => executor_lib::ParamType::Float,
                crate::dag::ParamType::Int => executor_lib::ParamType::Int,
                crate::dag::ParamType::Bool => executor_lib::ParamType::Bool,
                crate::dag::ParamType::String => executor_lib::ParamType::String,
                crate::dag::ParamType::DataFrame => executor_lib::ParamType::DataFrame,
                crate::dag::ParamType::DataFrameArray => executor_lib::ParamType::DataFrameArray,
            },
            default_value: p.default_value.clone(),
        })
        .collect();

    executor_lib::inject_params_into_code(code, &params_vec)
}

/// 使用 cargo build 编译临时项目（在本地进行编译，编译后 DLL 可通过 TCP 执行）
pub fn cargo_project_build(
    code: &str,
    algorithm_name: &str,
    compile_base_dir: &Path,
    debug: bool,
    temp_dir_prefix: &str,
) -> CargoBuildResult {
    let runtime_path = match executor_lib::find_runtime_path() {
        Ok(p) => p,
        Err(e) => {
            return CargoBuildResult {
                success: false,
                lib_path: None,
                temp_dir: None,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e),
            };
        }
    };

    executor_lib::cargo_project_build(code, algorithm_name, compile_base_dir, debug, temp_dir_prefix, &runtime_path)
}

/// 只编译自定义算子代码，不执行测试。
pub fn compile_only(code: &str, algorithm_name: &str) -> Result<PathBuf, String> {
    let compile_base_dir = get_compile_directory();
    let runtime_path = executor_lib::find_runtime_path()?;
    executor_lib::compile_only(code, algorithm_name, &compile_base_dir, &runtime_path)
}

/// 从算子定义的参数列表构造运行时参数 JSON
///
/// 将 `direction == Param` 的端口参数按 `(name, default_value)` 序列化为 JSON 对象，
/// 供算子运行时通过 `params_json` 读取。值类型按 `param_type` 转换：
/// Float -> f64、Int -> i64、Bool -> bool、String/DataFrame -> 字符串。
fn build_params_json(def: &CustomOperatorDef) -> String {
    let mut map = serde_json::Map::new();
    for p in &def.port_params {
        if p.direction != PortDirection::Param {
            continue;
        }
        let value = match p.param_type {
            ParamType::Float => p
                .default_value
                .parse::<f64>()
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
            ParamType::Int => p
                .default_value
                .parse::<i64>()
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
            ParamType::Bool => p
                .default_value
                .parse::<bool>()
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
            ParamType::String | ParamType::DataFrame | ParamType::DataFrameArray => {
                serde_json::Value::from(p.default_value.clone())
            }
        };
        map.insert(p.name.clone(), value);
    }
    serde_json::Value::Object(map).to_string()
}

/// 执行节点（通过 TCP 发送给 runtime）- 返回完整的执行结果
pub fn execute_node_with_result(node: &Node, inputs: &[PortData]) -> Result<ExecuteResult, String> {
    match &node.operator_type {
        OperatorType::Custom(def) => {
            let params: Vec<_> = def
                .port_params
                .iter()
                .filter(|p| p.direction == PortDirection::Param)
                .collect();
            let code_with_params = inject_params_into_code(&def.code, &params);

            let output_count = node.operator_type.output_count();
            let algorithm_name = node.operator_type.name();
            // 从算子定义中构造运行时参数 JSON，传递给算子（数据源算子等需要读取连接参数）
            let params_json = build_params_json(def);

            // 优先从服务器缓存的算子分类中查找 DLL 路径
            if let Some(dll_path) = crate::dag::find_operator_dll_path(algorithm_name) {
                let dll_path_buf = std::path::PathBuf::from(&dll_path);
                if dll_path_buf.exists() {
                    return with_runtime_client(|client| {
                        let _ = client.load_operator(algorithm_name, &dll_path);
                        client.execute_node(algorithm_name, inputs, output_count, &params_json)
                    })
                    .map_err(|e| e.to_string());
                }
            }

            // 检查是否有已启用的本地预编译算子
            let operator_dir = crate::config::get_operator_directory();
            let sanitized_name = executor_lib::sanitize_algorithm_name(algorithm_name);
            let op_dir = operator_dir.join(&sanitized_name);

            with_runtime_client(|client| {
                if op_dir.exists() {
                    // 使用预编译的 DLL
                    let dll_ext = match env::consts::OS {
                        "windows" => "dll",
                        "linux" => "so",
                        "macos" => "dylib",
                        _ => "so",
                    };
                    let dll_path = op_dir.join(format!("{}.{}", sanitized_name, dll_ext));
                    if dll_path.exists() {
                        let dll_path_str = dll_path.to_string_lossy().to_string();
                        // 先加载算子（如果还没加载）
                        let _ = client.load_operator(algorithm_name, &dll_path_str);
                        return client.execute_node(algorithm_name, inputs, output_count, &params_json);
                    }
                }

                // 否则：通过 TCP 发送代码进行远程编译并执行
                client.compile_and_execute(&code_with_params, algorithm_name, inputs, output_count)
            })
            .map_err(|e| e.to_string())
        }
    }
}

/// 执行节点（通过 TCP 发送给 runtime）- 仅返回输出数据（兼容旧接口）
pub fn execute_node(node: &Node, inputs: &[PortData]) -> Result<Vec<PortData>, String> {
    execute_node_with_result(node, inputs).map(|r| r.outputs)
}

/// 启用自定义算子：编译并将 DLL 和 JSON 复制到 operator/算子名称/ 目录
pub fn enable_operator(def: &CustomOperatorDef) -> Result<String, String> {
    let params: Vec<_> = def
        .port_params
        .iter()
        .filter(|p| p.direction == PortDirection::Param)
        .collect();
    let code_with_params = inject_params_into_code(&def.code, &params);

    let dll_path = compile_only(&code_with_params, &def.name)?;

    let dll_ext = match env::consts::OS {
        "windows" => "dll",
        "linux" => "so",
        "macos" => "dylib",
        _ => "so",
    };

    let operator_dir = crate::config::get_operator_directory();
    let sanitized_name = executor_lib::sanitize_algorithm_name(&def.name);
    let op_target_dir = operator_dir.join(&sanitized_name);
    fs::create_dir_all(&op_target_dir).map_err(|e| format!("创建算子目录失败: {}", e))?;

    let target_dll_path = op_target_dir.join(format!("{}.{}", sanitized_name, dll_ext));

    fs::copy(&dll_path, &target_dll_path).map_err(|e| format!("复制算子 DLL 失败: {}", e))?;

    // 通过 TCP 通知 runtime 加载新算子
    let _ = with_runtime_client(|client| {
        let dll_path_str = target_dll_path.to_string_lossy().to_string();
        client.load_operator(&def.name, &dll_path_str)
    });

    let json_path = op_target_dir.join("operator.json");
    let json_content = serde_json::to_string_pretty(def).map_err(|e| format!("序列化算子定义失败: {}", e))?;
    fs::write(&json_path, json_content).map_err(|e| format!("保存 JSON 失败: {}", e))?;

    crate::dag::refresh_operator_types_cache();
    // 同步失效服务器侧算子分类缓存，使新算子在下次渲染时立即出现
    crate::dag::refresh_operator_categories();

    Ok(format!("算子启用成功!\n算子目录: {}", op_target_dir.display()))
}

/// 「运行到此结点」的工作线程部分：构造目标节点的上游子图（含自身）并下发
/// 到服务端执行，返回完整结果供 UI 线程回填 registry。
///
/// 与「执行 DAG」路径统一：服务端按拓扑序执行整张子图（下游节点不执行）。
/// 算子间数据在服务端内存中传递（指针语义），响应只回传每个节点的前 200
/// 行预览 + 真实行数。
///
/// 本函数**不触碰 registry**——registry 由 UI 线程独占，跨线程无法 `&mut`，
/// 因此把回填拆分到 UI 线程通过 [`apply_dag_execution_result`] 完成。
///
/// 由于完整数据仅在服务端单次执行内存中存活、跨调用不持久化，每次「运行
/// 到此结点」都会重新执行整张上游子图，不做跳过。
///
/// `on_progress` 回调在服务端推送每个节点的执行进度（Executing/Completed/Failed）时
/// 被调用，调用方可据此实时反馈进度到 UI。
pub fn execute_dag_up_to_detached_with_progress<F>(
    graph: &DagGraph,
    target_node_id: &str,
    mut on_progress: F,
) -> Result<DagExecutionResult, String>
where
    F: FnMut(&DagNodeResult),
{
    // 目标节点的所有上游（含自身），已按拓扑序排列
    let ancestors = graph.get_ancestors(target_node_id)?;
    let ancestor_set: HashSet<String> = ancestors.iter().cloned().collect();

    let dag_name = format!("upto_{}", target_node_id);
    let subset = build_dag_definition_subset(graph, &dag_name, &ancestor_set);

    with_runtime_client(|client| client.execute_dag_with_progress(&subset, |p| on_progress(p)))
        .map_err(|e| e.to_string())
}

/// 「运行到此结点」的便捷接口（不接收中间进度），等价于传空回调。
pub fn execute_dag_up_to_detached(
    graph: &DagGraph,
    target_node_id: &str,
) -> Result<DagExecutionResult, String> {
    execute_dag_up_to_detached_with_progress(graph, target_node_id, |_| {})
}

// ===== 流式执行接口（转发 StreamChunk 事件，供聊天预览等实时刷新场景使用）=====

use operator_executor_client::runtime_client::DagStreamEvent;

/// 与 [`execute_dag_on_server_with_progress`] 相同，但额外通过 `on_chunk` 回调转发
/// 服务端推送的 `StreamChunk` 帧（流式节点产出的实时 chunk），供调用方做实时预览。
pub fn execute_dag_on_server_streaming<F, G>(
    graph: &DagGraph,
    name: &str,
    mut on_progress: F,
    mut on_chunk: G,
) -> Result<DagExecutionResult, String>
where
    F: FnMut(&DagNodeResult),
    G: FnMut(&str, &PortData),
{
    if graph.nodes.is_empty() {
        return Err("DAG 为空，无节点可执行".to_string());
    }

    let dag = build_dag_definition(graph, name);

    // 落盘到 dag 目录
    let dag_dir = crate::config::get_dag_directory();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| as_millis_helper(d))
        .unwrap_or(0);
    let safe_name = sanitize_dag_file_name(name);
    let file_name = format!("dag_{}_{}.json", safe_name, timestamp);
    let dag_path = dag_dir.join(&file_name);
    match serde_json::to_string_pretty(&dag) {
        Ok(json) => {
            if let Err(e) = fs::write(&dag_path, &json) {
                eprintln!("写入 DAG 文件失败: {} (路径: {})", e, dag_path.display());
            }
            let latest_path = dag_dir.join("dag_latest.json");
            let _ = fs::write(&latest_path, &json);
        }
        Err(e) => {
            eprintln!("序列化 DAG 失败，跳过落盘: {}", e);
        }
    }

    with_runtime_client(|client| {
        client.execute_dag_streaming(&dag, |ev| match ev {
            DagStreamEvent::NodeProgress(p) => on_progress(&p),
            DagStreamEvent::StreamChunk { node_id, chunk, .. } => {
                on_chunk(&node_id, &chunk);
            }
        })
    })
    .map_err(|e| e.to_string())
}

/// 与 [`execute_dag_up_to_detached_with_progress`] 相同，但额外通过 `on_chunk` 回调转发
/// 服务端推送的 `StreamChunk` 帧。
pub fn execute_dag_up_to_detached_streaming<F, G>(
    graph: &DagGraph,
    target_node_id: &str,
    mut on_progress: F,
    mut on_chunk: G,
) -> Result<DagExecutionResult, String>
where
    F: FnMut(&DagNodeResult),
    G: FnMut(&str, &PortData),
{
    let ancestors = graph.get_ancestors(target_node_id)?;
    let ancestor_set: HashSet<String> = ancestors.iter().cloned().collect();

    let dag_name = format!("upto_{}", target_node_id);
    let subset = build_dag_definition_subset(graph, &dag_name, &ancestor_set);

    with_runtime_client(|client| {
        client.execute_dag_streaming(&subset, |ev| match ev {
            DagStreamEvent::NodeProgress(p) => on_progress(&p),
            DagStreamEvent::StreamChunk { node_id, chunk, .. } => {
                on_chunk(&node_id, &chunk);
            }
        })
    })
    .map_err(|e| e.to_string())
}

/// `Duration::as_millis()` 的辅助函数（避免在闭包中类型推断歧义）。
fn as_millis_helper(d: std::time::Duration) -> u128 {
    d.as_millis()
}

/// 将单个节点的执行结果回填到 registry 与预览缓存。
///
/// 从 [`apply_dag_execution_result`] 的循环体抽出，供「最终结果回填」和「进度回填」共用。
/// 仅处理 `Completed` / `Failed` 终态；`Executing` 等中间态由调用方单独处理。
/// 返回 `Err` 仅对 `Failed` 状态（携带错误信息），调用方可据此决定是否中断后续。
pub(crate) fn apply_dag_node_result(
    graph: &DagGraph,
    nr: &DagNodeResult,
    registry: &mut NodeIORegistry,
) -> Result<(), String> {
    match nr.execution_result.status {
        OperatorExecutionStatus::Completed => {
            // 落盘预览缓存（前 200 行），失败不影响整体结果
            if let Err(e) = crate::data_preview::save_preview_from_truncated(
                &nr.node_id,
                &nr.operator_name,
                &nr.outputs,
                nr.output_row_count,
            ) {
                eprintln!("缓存预览数据失败 (节点 {}): {}", nr.node_id, e);
            }
            // registry 只存预览/状态/日志；inputs 不再回传，传空
            registry.set_result(
                &nr.node_id,
                Vec::new(),
                nr.outputs.clone(),
                nr.execution_result.clone(),
            );
            Ok(())
        }
        OperatorExecutionStatus::Failed => {
            registry.set_failed(
                &nr.node_id,
                Vec::new(),
                nr.execution_result.clone(),
            );
            let error_msg = nr.execution_result.error_message.clone()
                .unwrap_or_else(|| "执行失败（未知原因）".to_string());
            // 取节点显示名用于错误信息
            let display_name = graph
                .get_node(&nr.node_id)
                .map(|n| n.operator_type.name())
                .unwrap_or(&nr.operator_name);
            Err(format!(
                "节点 {} ({}) 执行失败: {}",
                nr.node_id, display_name, error_msg
            ))
        }
        other => {
            eprintln!(
                "节点 {} ({}) 处于非终态 {}，跳过 registry 回填",
                nr.node_id, nr.operator_name, other.to_str()
            );
            Ok(())
        }
    }
}

/// 将服务端 DAG 执行结果回填到 registry 与预览缓存。
///
/// 遍历各节点结果调用 [`apply_dag_node_result`]。已在进度回填阶段处理过的终态节点
/// （`Completed` / `Failed`）会被跳过，避免重复落盘预览和重复日志。
pub(crate) fn apply_dag_execution_result(
    graph: &DagGraph,
    result: &DagExecutionResult,
    registry: &mut NodeIORegistry,
) -> Result<(), String> {
    for nr in &result.node_results {
        // 去重：已通过进度帧回填过终态的节点跳过，避免重复落盘预览
        if matches!(
            registry.get_status(&nr.node_id),
            OperatorExecutionStatus::Completed | OperatorExecutionStatus::Failed
        ) {
            continue;
        }
        apply_dag_node_result(graph, nr, registry)?;
    }
    Ok(())
}

/// 解析算子名称对应的 DLL 路径（用于构造下发的 DAG 节点定义）。
///
/// 优先级：服务端缓存的算子分类 > 本地算子目录。两者都未命中时返回 None，
/// 由服务端在算子库目录中按名兜底查找。
fn resolve_operator_dll_path(operator_name: &str) -> Option<String> {
    // 1. 服务端缓存的算子分类
    if let Some(dll_path) = crate::dag::find_operator_dll_path(operator_name) {
        let path = std::path::PathBuf::from(&dll_path);
        if path.exists() {
            return Some(dll_path);
        }
    }
    // 2. 本地算子目录
    let operator_dir = crate::config::get_operator_directory();
    let sanitized_name = executor_lib::sanitize_algorithm_name(operator_name);
    let op_dir = operator_dir.join(&sanitized_name);
    if op_dir.exists() {
        let dll_ext = match env::consts::OS {
            "windows" => "dll",
            "linux" => "so",
            "macos" => "dylib",
            _ => "so",
        };
        let dll_path = op_dir.join(format!("{}.{}", sanitized_name, dll_ext));
        if dll_path.exists() {
            return Some(dll_path.to_string_lossy().to_string());
        }
    }
    None
}

/// 根据画布 DAG 构造可下发的 [`DagDefinition`]。
///
/// 将每个节点转换为服务端执行所需的精简定义（算子名、DLL 路径、端口数、参数 JSON），
/// 并保留完整的边连接关系。参数 JSON 由算子定义中 `direction == Param` 的端口项生成。
pub fn build_dag_definition(graph: &DagGraph, name: &str) -> DagDefinition {
    let nodes: Vec<DagNodeDef> = graph
        .nodes
        .iter()
        .map(|node| {
            let (operator_name, params_json, stream) = match &node.operator_type {
                OperatorType::Custom(def) => {
                    (def.name.clone(), build_params_json(def), def.stream)
                }
            };
            let input_count = node.operator_type.input_count();
            let output_count = node.operator_type.output_count();
            let dll_path = resolve_operator_dll_path(&operator_name);

            DagNodeDef {
                id: node.id.clone(),
                operator_name,
                input_count,
                output_count,
                params_json,
                dll_path,
                stream,
            }
        })
        .collect();

    let edges: Vec<DagEdgeDef> = graph
        .edges
        .iter()
        .map(|e| DagEdgeDef {
            source_node_id: e.source_node_id.clone(),
            source_port: e.source_port,
            target_node_id: e.target_node_id.clone(),
            target_port: e.target_port,
        })
        .collect();

    DagDefinition {
        name: name.to_string(),
        nodes,
        edges,
    }
}

/// 构造只包含指定节点集合的子图 [`DagDefinition`]。
///
/// 与 [`build_dag_definition`] 的区别：仅保留 `node_ids` 中的节点，以及两端
/// 都在该集合内的边。用于「运行到此结点」下发目标节点的上游子图——服务端
/// 只执行子图内的节点，下游节点不执行。
///
/// 节点定义（算子名、DLL 路径、端口数、参数 JSON）的构造逻辑与
/// [`build_dag_definition`] 完全一致，复用 [`resolve_operator_dll_path`]。
pub fn build_dag_definition_subset(
    graph: &DagGraph,
    name: &str,
    node_ids: &HashSet<String>,
) -> DagDefinition {
    let nodes: Vec<DagNodeDef> = graph
        .nodes
        .iter()
        .filter(|node| node_ids.contains(&node.id))
        .map(|node| {
            let (operator_name, params_json, stream) = match &node.operator_type {
                OperatorType::Custom(def) => {
                    (def.name.clone(), build_params_json(def), def.stream)
                }
            };
            let input_count = node.operator_type.input_count();
            let output_count = node.operator_type.output_count();
            let dll_path = resolve_operator_dll_path(&operator_name);

            DagNodeDef {
                id: node.id.clone(),
                operator_name,
                input_count,
                output_count,
                params_json,
                dll_path,
                stream,
            }
        })
        .collect();

    // 仅保留两端都在子图内的边，保证服务端拓扑完整且不引入悬挂引用
    let edges: Vec<DagEdgeDef> = graph
        .edges
        .iter()
        .filter(|e| node_ids.contains(&e.source_node_id) && node_ids.contains(&e.target_node_id))
        .map(|e| DagEdgeDef {
            source_node_id: e.source_node_id.clone(),
            source_port: e.source_port,
            target_node_id: e.target_node_id.clone(),
            target_port: e.target_port,
        })
        .collect();

    DagDefinition {
        name: name.to_string(),
        nodes,
        edges,
    }
}

/// 过滤 DAG 文件名中的非法字符，仅保留字母、数字、`-`、`_`，其余替换为 `_`。
fn sanitize_dag_file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 将编排好的 DAG 落盘到 dag 目录，并下发到服务端解析执行（带进度回调）。
///
/// 流程：
/// 1. 从 [`DagGraph`] 构造 [`DagDefinition`]（含每个节点的算子名、DLL 路径、端口数、参数 JSON）。
/// 2. 以 `dag_<name>_<timestamp>.json` 落盘到 [`crate::config::get_dag_directory`]，
///    同时刷新 `dag_latest.json` 副本便于排查。
/// 3. 通过 TCP 下发到 runtime 服务端，由服务端拓扑排序并按序执行。
/// 4. 返回各节点执行结果（含输入/输出/状态/日志），供调用方回填 registry 与预览缓存。
///
/// `on_progress` 回调在服务端推送每个节点的执行进度（Executing/Completed/Failed）时
/// 被调用，调用方可据此实时反馈进度到 UI。
pub fn execute_dag_on_server_with_progress<F>(
    graph: &DagGraph,
    name: &str,
    mut on_progress: F,
) -> Result<DagExecutionResult, String>
where
    F: FnMut(&DagNodeResult),
{
    if graph.nodes.is_empty() {
        return Err("DAG 为空，无节点可执行".to_string());
    }

    let dag = build_dag_definition(graph, name);

    // 落盘到 dag 目录
    let dag_dir = crate::config::get_dag_directory();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let safe_name = sanitize_dag_file_name(name);
    let file_name = format!("dag_{}_{}.json", safe_name, timestamp);
    let dag_path = dag_dir.join(&file_name);
    match serde_json::to_string_pretty(&dag) {
        Ok(json) => {
            if let Err(e) = fs::write(&dag_path, &json) {
                eprintln!("写入 DAG 文件失败: {} (路径: {})", e, dag_path.display());
            }
            // 刷新 latest 副本，便于快速查看最近一次下发的流程
            let latest_path = dag_dir.join("dag_latest.json");
            let _ = fs::write(&latest_path, &json);
        }
        Err(e) => {
            eprintln!("序列化 DAG 失败，跳过落盘: {}", e);
        }
    }

    // 下发到服务端执行（流式接收进度）
    with_runtime_client(|client| client.execute_dag_with_progress(&dag, |p| on_progress(p)))
        .map_err(|e| e.to_string())
}

/// 将编排好的 DAG 落盘并下发执行的便捷接口（不接收中间进度），等价于传空回调。
pub fn execute_dag_on_server(graph: &DagGraph, name: &str) -> Result<DagExecutionResult, String> {
    execute_dag_on_server_with_progress(graph, name, |_| {})
}

/// 执行预编译的 DLL（通过 TCP 直接指定路径）- 返回完整执行结果
pub fn execute_native_operator_with_result(
    dll_path: &Path,
    inputs: &[PortData],
    max_outputs: usize,
    params_json: &str,
) -> Result<ExecuteResult, String> {
    with_runtime_client(|client| {
        let dll_path_str = dll_path.to_string_lossy().to_string();
        client.execute_dll(&dll_path_str, inputs, max_outputs, params_json)
    })
    .map_err(|e| e.to_string())
}

/// 执行预编译的 DLL（通过 TCP 直接指定路径）- 仅返回输出数据（兼容旧接口）
pub fn execute_native_operator(
    dll_path: &Path,
    inputs: &[PortData],
    max_outputs: usize,
    params_json: &str,
) -> Result<Vec<PortData>, String> {
    execute_native_operator_with_result(dll_path, inputs, max_outputs, params_json)
        .map(|r| r.outputs)
}

/// 关闭 runtime 服务（可选）
pub fn shutdown_runtime() {
    let _ = with_runtime_client(|client| client.shutdown());
    if let Some(mut child) = RUNTIME_PROCESS.lock().unwrap().take() {
        let _ = child.kill();
    }
}

// 使用 Once 标记确保在程序退出时尝试清理
static CLEANUP_REGISTERED: Once = Once::new();

pub fn register_cleanup() {
    CLEANUP_REGISTERED.call_once(|| {
        // 注意：在实际 GUI 应用中，可以通过 at_exit 或窗口关闭回调来触发
        // 这里只是预留接口
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_params() {
        // 简单测试参数注入
        let code = "fn main() {}";
        let params = vec![];
        let result = inject_params_into_code(code, &params);
        assert!(result.contains("fn main()"));
    }
}
