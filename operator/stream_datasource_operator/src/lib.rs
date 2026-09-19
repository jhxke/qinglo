//! 流式 SQL 数据源算子
//!
//! 功能与 datasource_operator 相同（连接 PostgreSQL、执行 SQL、结果转 DataFrame），
//! 区别在于：
//! - 输入端口 `input` 是**流式 String**：每收到一个字符串 chunk，就把它替换进 SQL
//!   模板的 `${input}` 占位符，执行一次查询，产出一个 `PortData::DataFrame` chunk。
//! - 输出端口 `output` 是 **DataFrame**（流式）；流结束后服务端把多个 DataFrame
//!   聚合为 DataFrameArray 供非流式下游消费。
//!
//! 导出 5 个流式 C ABI 符号（`execute_operator_stream_start/push/push_end/next/end`），
//! 同时导出批量 `execute_operator` 作为 `stream=false` 时的兜底（读一个 String 输入，
//! 执行一次查询输出单个 DataFrame）。
//!
//! ## 设计要点
//!
//! - **连接复用**：`stream_start` 时建立一次 tokio runtime + PostgreSQL 连接，整条流
//!   复用，避免每个 chunk 重连。
//! - **push 内同步查询**：`stream_push` 用 `runtime.block_on` 同步执行查询（流式编排
//!   本就运行在 spawn_blocking 线程），结果放入 handle 的待输出队列，随后由
//!   `stream_next` 逐个取出。
//! - **模板原文替换**：`${input}` 直接替换为上游字符串，不自动加引号/转义，字面量
//!   引号由用户写在 SQL 模板中（如 `WHERE code = '${input}'`）。
//! - **三态 `next`**：`0`=有 chunk；`1`=暂无可读（含上游 EOF 后队列排空）；`<0`=错误。

use std::collections::{HashSet, VecDeque};
use std::ffi::{c_char, c_void, CStr, CString};

use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::tokio;
use operator_runtime::chrono::NaiveDateTime;
use operator_runtime::{ColumnData, DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use serde::{Deserialize, Serialize};
use tokio_postgres::{Config, NoTls, Row};

/// SQL 模板中引用流式输入字符串的变量名（`${input}`）。
const INPUT_VAR: &str = "input";

// ===== 参数 =====

/// 流式 SQL 数据源算子参数（字段名与 operator.json 的 Param 项一致）。
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StreamDataSourceParams {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_database")]
    pub database: String,
    #[serde(default = "default_username")]
    pub username: String,
    #[serde(default = "default_password")]
    pub password: String,
    /// 含 `${input}` 占位符的 SQL 模板（必填）
    #[serde(default)]
    pub query: String,
    /// 内存排序字段；为空表示不排序
    #[serde(default)]
    pub sort_column: String,
    /// 是否降序，默认升序
    #[serde(default)]
    pub sort_descending: bool,
}

fn default_host() -> String {
    "localhost".to_string()
}
fn default_port() -> u16 {
    5432
}
fn default_database() -> String {
    "whatigo".to_string()
}
fn default_username() -> String {
    "postgres".to_string()
}
fn default_password() -> String {
    "difyai123456".to_string()
}

impl Default for StreamDataSourceParams {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            database: default_database(),
            username: default_username(),
            password: default_password(),
            query: String::new(),
            sort_column: String::new(),
            sort_descending: false,
        }
    }
}

/// 解析参数 JSON；空串返回默认值，非法 JSON 打印告警后返回默认值。
fn parse_params(params_json: &str) -> StreamDataSourceParams {
    if params_json.is_empty() {
        return StreamDataSourceParams::default();
    }
    match serde_json::from_str::<StreamDataSourceParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("[stream_datasource] 解析参数 JSON 失败: {}，使用默认值", e);
            StreamDataSourceParams::default()
        }
    }
}

// ===== 错误辅助 =====

fn set_err(msg: &str) {
    eprintln!("[stream_datasource] {}", msg);
    let c = CString::new(msg).unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

fn clear_err() {
    let c = CString::new("").unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

// ===== SQL 模板渲染 =====

/// 判断字符是否允许出现在 `${...}` 变量名中（ASCII 字母/数字/下划线）。
fn is_var_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 把 SQL 模板中的 `${input}` 替换为上游字符串值（原文替换）。
///
/// - 语法不完整（没有闭合 `}`）或变量名含非法字符的 `${...}` 按普通文本保留。
/// - 未识别的合法变量名保持原样，每个名字只在 stderr 告警一次（避免流中刷屏）。
fn render_sql(template: &str, value: &str, unknown_vars: &mut HashSet<String>) -> String {
    let mut out = String::with_capacity(template.len() + value.len());
    let mut rest = template;
    loop {
        let Some(pos) = rest.find("${") else {
            out.push_str(rest);
            break;
        };
        let after_dollar = &rest[pos + 2..];
        // 仅当 `${` 之后存在闭合 `}` 且变量名合法时才视为占位符
        if let Some(end) = after_dollar.find('}') {
            let name = &after_dollar[..end];
            if !name.is_empty() && name.bytes().all(is_var_name_byte) {
                out.push_str(&rest[..pos]);
                if name == INPUT_VAR {
                    out.push_str(value);
                } else {
                    if unknown_vars.insert(name.to_string()) {
                        eprintln!(
                            "[stream_datasource] 警告: SQL 模板中存在未知变量 ${{{}}}，已原样保留（当前仅支持 ${{{}}}）",
                            name, INPUT_VAR
                        );
                    }
                    out.push_str(&rest[pos..pos + 2 + end + 1]);
                }
                rest = &after_dollar[end + 1..];
                continue;
            }
        }
        // 非占位符：输出到 '$' 为止（含），从 '$' 之后继续扫描，"${" 后续按普通文本处理
        out.push_str(&rest[..=pos]);
        rest = &rest[pos + 1..];
    }
    out
}

// ===== PostgreSQL 连接 / 查询 =====

fn build_pg_config(params: &StreamDataSourceParams) -> Config {
    let mut cfg = Config::new();
    cfg.host(&params.host)
        .port(params.port)
        .dbname(&params.database)
        .user(&params.username)
        .password(&params.password);
    cfg
}

/// 在给定 runtime 上建立 PostgreSQL 连接（连接对象由 runtime 后台任务驱动）。
fn connect_blocking(
    rt: &tokio::runtime::Runtime,
    params: &StreamDataSourceParams,
) -> Result<tokio_postgres::Client, String> {
    let cfg = build_pg_config(params);
    rt.block_on(async move {
        let (client, connection) = cfg.connect(NoTls).await.map_err(|e| {
            format!(
                "连接 PostgreSQL 失败: {}:{}/{} (用户: {}) - 错误: {}",
                params.host, params.port, params.database, params.username, e
            )
        })?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("[stream_datasource] PostgreSQL 连接错误: {}", e);
            }
        });
        Ok(client)
    })
}

/// 用已建立的连接同步执行一条 SQL，返回全部行。
fn query_blocking(
    rt: &tokio::runtime::Runtime,
    client: &tokio_postgres::Client,
    sql: &str,
) -> Result<Vec<Row>, String> {
    rt.block_on(async move {
        client
            .query(sql, &[])
            .await
            .map_err(|e| format!("执行 SQL 查询失败: {} - SQL: {}", e, sql))
    })
}

// ===== Row -> DataFrame（与 datasource_operator 同一套推断/构建/排序逻辑）=====

/// 一次遍历推断所有列的类型：遇到第一个非空值即确定；整列空保留 Null。
fn infer_all_column_types(rows: &[Row]) -> Vec<(String, DataType)> {
    if rows.is_empty() {
        return Vec::new();
    }
    let columns = rows[0].columns();
    let mut metas: Vec<(String, DataType)> = columns
        .iter()
        .map(|c| (c.name().to_string(), DataType::Null))
        .collect();
    let n_col = metas.len();
    for row in rows {
        for i in 0..n_col {
            if metas[i].1 != DataType::Null {
                continue;
            }
            let name = &*metas[i].0;
            if row.try_get::<&str, Option<i64>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::Int64;
                continue;
            }
            if row.try_get::<&str, Option<f64>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::Float64;
                continue;
            }
            if row.try_get::<&str, Option<bool>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::Bool;
                continue;
            }
            if row.try_get::<&str, Option<String>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::String;
                continue;
            }
            if row
                .try_get::<&str, Option<NaiveDateTime>>(name)
                .is_ok_and(|v| v.is_some())
            {
                metas[i].1 = DataType::String;
            }
        }
        if metas.iter().all(|m| m.1 != DataType::Null) {
            break;
        }
    }
    metas
}

/// 按列元信息与行索引集合构建 DataFrame。
fn build_dataframe_from_indices(
    rows: &[Row],
    col_metas: &[(String, DataType)],
    indices: &[usize],
) -> DataFrame {
    let mut df = DataFrame::new();
    for (name, data_type) in col_metas {
        let mut column_data = ColumnData::new(name.clone(), data_type.clone());
        for &ridx in indices {
            let row = &rows[ridx];
            match data_type {
                DataType::Int64 => match row.try_get::<&str, Option<i64>>(name) {
                    Ok(v) => column_data.push_i64(v),
                    Err(_) => column_data.push_i64(None),
                },
                DataType::Float64 => match row.try_get::<&str, Option<f64>>(name) {
                    Ok(v) => column_data.push_f64(v),
                    Err(_) => column_data.push_f64(None),
                },
                DataType::Bool => match row.try_get::<&str, Option<bool>>(name) {
                    Ok(v) => column_data.push_bool(v),
                    Err(_) => column_data.push_bool(None),
                },
                DataType::String => match row.try_get::<&str, Option<String>>(name) {
                    Ok(Some(ref v)) => column_data.push_string(Some(v)),
                    Ok(None) => column_data.push_string(None),
                    Err(_) => match row.try_get::<&str, Option<NaiveDateTime>>(name) {
                        Ok(Some(ref v)) => column_data.push_string(Some(&v.to_string())),
                        _ => column_data.push_string(None),
                    },
                },
                DataType::Null => {}
            }
        }
        df.add_column(column_data);
    }
    df
}

/// 把 Row 中某列转成可比较的排序键（空值排最后）。
fn sort_key_for_row(row: &Row, col: &str) -> SortKey {
    if let Ok(Some(v)) = row.try_get::<&str, Option<i64>>(col) {
        return SortKey::Int64(v);
    }
    if let Ok(Some(v)) = row.try_get::<&str, Option<f64>>(col) {
        return SortKey::Float64(v);
    }
    if let Ok(Some(v)) = row.try_get::<&str, Option<bool>>(col) {
        return SortKey::Bool(v);
    }
    if let Ok(Some(v)) = row.try_get::<&str, Option<String>>(col) {
        return SortKey::Str(v);
    }
    if let Ok(Some(v)) = row.try_get::<&str, Option<NaiveDateTime>>(col) {
        return SortKey::Str(v.to_string());
    }
    SortKey::Null
}

#[derive(PartialEq)]
enum SortKey {
    Null,
    Bool(bool),
    Int64(i64),
    Float64(f64),
    Str(String),
}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering::*;
        use SortKey::*;
        match (self, other) {
            (Null, Null) => Some(Equal),
            (Null, _) => Some(Greater),
            (_, Null) => Some(Less),
            (Bool(a), Bool(b)) => a.partial_cmp(b),
            (Int64(a), Int64(b)) => a.partial_cmp(b),
            (Float64(a), Float64(b)) => a.partial_cmp(b),
            (Str(a), Str(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

/// 行集合转 DataFrame：推断列类型 -> 可选排序 -> 构建。
fn rows_to_dataframe(rows: &[Row], sort_by: Option<&str>, sort_descending: bool) -> DataFrame {
    if rows.is_empty() {
        return DataFrame::new();
    }
    let col_metas = infer_all_column_types(rows);
    let mut indices: Vec<usize> = (0..rows.len()).collect();
    if let Some(sort_col) = sort_by {
        if sort_descending {
            indices.sort_by(|&a, &b| {
                let ka = sort_key_for_row(&rows[a], sort_col);
                let kb = sort_key_for_row(&rows[b], sort_col);
                kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
            });
        } else {
            indices.sort_by(|&a, &b| {
                let ka = sort_key_for_row(&rows[a], sort_col);
                let kb = sort_key_for_row(&rows[b], sort_col);
                ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }
    build_dataframe_from_indices(rows, &col_metas, &indices)
}

/// 查询参数与连接的共享集合（流式 handle 与批量兜底都用它跑一次完整查询）。
fn run_query_to_dataframe(
    rt: &tokio::runtime::Runtime,
    client: &tokio_postgres::Client,
    sql: &str,
    sort_by: Option<&str>,
    sort_descending: bool,
) -> Result<DataFrame, String> {
    let rows = query_blocking(rt, client, sql)?;
    eprintln!(
        "[stream_datasource] SQL 执行成功，返回 {} 行: {}",
        rows.len(),
        sql
    );
    let df = rows_to_dataframe(&rows, sort_by, sort_descending);
    if df.row_count == 0 {
        eprintln!("[stream_datasource] 警告: 查询返回 0 行: {}", sql);
    }
    Ok(df)
}

// ===== 流式 handle =====

/// 流式执行的运行时状态（`stream_start` 返回的 `*mut c_void` 指向它）。
///
/// 持有整条流复用的 tokio runtime 与数据库连接；每个 push 进来的字符串触发一次
/// 查询，结果 DataFrame 进入 `pending` 队列等待 `next` 取出。
struct StreamDataSource {
    rt: tokio::runtime::Runtime,
    client: tokio_postgres::Client,
    /// SQL 模板（含 `${input}`）
    template: String,
    sort_by: Option<String>,
    sort_descending: bool,
    /// 已查询完成、等待 next 输出的 DataFrame 队列
    pending: VecDeque<DataFrame>,
    /// 上游是否已 EOF
    upstream_eof: bool,
    /// 已处理的输入 chunk 数
    processed: usize,
    /// 已告警过的未知模板变量名（每个名字只告警一次）
    unknown_vars: HashSet<String>,
}

impl StreamDataSource {
    /// 渲染模板 -> 执行查询 -> DataFrame 入队。
    fn push_value(&mut self, value: &str) -> Result<(), String> {
        let sql = render_sql(&self.template, value, &mut self.unknown_vars);
        let df = run_query_to_dataframe(
            &self.rt,
            &self.client,
            &sql,
            self.sort_by.as_deref(),
            self.sort_descending,
        )?;
        self.processed += 1;
        self.pending.push_back(df);
        Ok(())
    }
}

// ===== 流式 C ABI：5 个符号 =====

/// `stream_start`：解析参数、校验模板、建立数据库连接，返回不透明 handle。
///
/// 流式输入端口由服务端以占位填充，实际 chunk 通过 `stream_push` 到达，因此这里
/// 不读取任何 inputs。返回 null = 失败（用 `c_get_last_error` 取详情）。
#[no_mangle]
pub extern "C" fn execute_operator_stream_start(
    _inputs: *const CPortData,
    _input_count: usize,
    params_json: *const c_char,
) -> *mut c_void {
    if let Err(e) = ensure_runtime_loaded() {
        set_err(&format!("runtime 加载失败: {}", e));
        return std::ptr::null_mut();
    }

    let params_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_str);

    if params.query.trim().is_empty() {
        set_err("缺少必需参数 query（含 ${input} 占位符的 SQL 模板）");
        return std::ptr::null_mut();
    }

    eprintln!(
        "[stream_datasource] 启动流式查询: {}:{}/{} (用户: {})，模板: {}",
        params.host, params.port, params.database, params.username, params.query
    );

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            set_err(&format!("创建 tokio runtime 失败: {}", e));
            return std::ptr::null_mut();
        }
    };
    let client = match connect_blocking(&rt, &params) {
        Ok(client) => client,
        Err(e) => {
            set_err(&e);
            return std::ptr::null_mut();
        }
    };
    eprintln!("[stream_datasource] PostgreSQL 连接成功，等待上游字符串 chunk");

    let sort_by = (!params.sort_column.is_empty()).then_some(params.sort_column);
    let state = StreamDataSource {
        rt,
        client,
        template: params.query,
        sort_by,
        sort_descending: params.sort_descending,
        pending: VecDeque::new(),
        upstream_eof: false,
        processed: 0,
        unknown_vars: HashSet::new(),
    };
    clear_err();
    Box::into_raw(Box::new(state)) as *mut c_void
}

/// `stream_push`：接收上游 String chunk，渲染 SQL 并同步查询，结果入待输出队列。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push(
    handle: *mut c_void,
    chunk: *const CPortData,
) -> i32 {
    if handle.is_null() {
        set_err("stream_push: handle 为 null");
        return -1;
    }
    if chunk.is_null() {
        // 空 chunk 视为 no-op
        return 0;
    }
    let state: &mut StreamDataSource = unsafe { &mut *(handle as *mut StreamDataSource) };

    // take 语义：消费后 slot 被置 TYPE_NULL，SDK 随后的 c_pd_free 为 no-op
    let pd = unsafe { portdata_from_c(chunk as *mut CPortData) };
    let value = match pd {
        PortData::String(s) => s,
        other => {
            set_err(&format!(
                "stream_push: 上游 chunk 类型错误，期望 String，实际 {}",
                other.type_name()
            ));
            return -2;
        }
    };

    if let Err(e) = state.push_value(&value) {
        set_err(&e);
        return -3;
    }
    0
}

/// `stream_push_end`：通知上游 EOF。剩余结果由 next 排空后排版流结束。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push_end(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        set_err("stream_push_end: handle 为 null");
        return -1;
    }
    let state: &mut StreamDataSource = unsafe { &mut *(handle as *mut StreamDataSource) };
    state.upstream_eof = true;
    eprintln!(
        "[stream_datasource] 上游 EOF：共处理 {} 个输入 chunk，待输出 {} 个 DataFrame",
        state.processed,
        state.pending.len()
    );
    0
}

/// `stream_next`：取出下一个已就绪的 DataFrame chunk（三态返回）。
///
/// - `0`：有 chunk（写入 `*out_chunk`，owned DataFrame）
/// - `1`：当前暂无可读（队列空；上游未 EOF 时等待新 push，已 EOF 时即永久结束）
/// - `<0`：错误
#[no_mangle]
pub extern "C" fn execute_operator_stream_next(
    handle: *mut c_void,
    out_chunk: *mut CPortData,
) -> i32 {
    if handle.is_null() || out_chunk.is_null() {
        set_err("stream_next: handle 或 out_chunk 为 null");
        return -1;
    }
    let state: &mut StreamDataSource = unsafe { &mut *(handle as *mut StreamDataSource) };
    match state.pending.pop_front() {
        Some(df) => {
            let c_pd = portdata_to_c_owned(PortData::DataFrame(df));
            unsafe { *out_chunk = c_pd };
            0
        }
        None => 1,
    }
}

/// `stream_end`：释放 handle 及关联资源（断开数据库连接、关闭 runtime）。
#[no_mangle]
pub extern "C" fn execute_operator_stream_end(handle: *mut c_void) {
    if !handle.is_null() {
        unsafe {
            drop(Box::from_raw(handle as *mut StreamDataSource));
        }
    }
}

// ===== 批量兜底：execute_operator =====

/// 批量执行（`stream=false` 降级）：读取端口 [0] 的 String，渲染模板执行一次查询，
/// 输出单个 DataFrame。
///
/// 返回：`0`=成功；`-1`=runtime 加载失败；`-2`=缺少/非法输入；`-3`=参数错误；
/// `-4`=连接或查询失败。
#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        set_err(&format!("runtime 加载失败: {}", e));
        return -1;
    }

    let params_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_str);

    if input_count == 0 || inputs.is_null() {
        set_err("缺少输入数据（批量模式需要 1 个 String 输入）");
        return -2;
    }
    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let value = match input_pd {
        PortData::String(s) => s,
        other => {
            set_err(&format!(
                "输入类型错误：需要 String，实际为 {}",
                other.type_name()
            ));
            return -2;
        }
    };

    if params.query.trim().is_empty() {
        set_err("缺少必需参数 query（含 ${input} 占位符的 SQL 模板）");
        return -3;
    }

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            set_err(&format!("创建 tokio runtime 失败: {}", e));
            return -4;
        }
    };
    let client = match connect_blocking(&rt, &params) {
        Ok(client) => client,
        Err(e) => {
            set_err(&e);
            return -4;
        }
    };

    let mut unknown_vars = HashSet::new();
    let sql = render_sql(&params.query, &value, &mut unknown_vars);
    let sort_by = (!params.sort_column.is_empty()).then_some(params.sort_column.clone());
    let df = match run_query_to_dataframe(
        &rt,
        &client,
        &sql,
        sort_by.as_deref(),
        params.sort_descending,
    ) {
        Ok(df) => df,
        Err(e) => {
            set_err(&format!("流式 SQL 数据源算子执行失败: {}", e));
            return -4;
        }
    };

    eprintln!(
        "[stream_datasource] 批量执行完成: DataFrame（{} 行，{} 列）",
        df.row_count,
        df.col_count()
    );

    clear_err();
    let port_data = PortData::DataFrame(df);
    if !outputs.is_null() && output_cap > 0 {
        let c_pd = portdata_to_c_owned(port_data);
        unsafe {
            *outputs = c_pd;
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
                    value: CPortValue {
                        str_ptr: std::ptr::null_mut(),
                    },
                };
            }
        }
    }
    0
}

/// 释放 C ABI PortData 内存（由调用方调用）。
#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

/// 获取算子版本。
#[no_mangle]
pub extern "C" fn stream_datasource_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
