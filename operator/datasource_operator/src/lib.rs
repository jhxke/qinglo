use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, ColumnData, DataType, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_to_c,
    c_pd_free, c_set_last_error,
};
use std::ffi::{CStr, CString, c_char};
use tokio_postgres::{Config, NoTls, Row};
use operator_runtime::tokio;
use operator_runtime::chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// 返回数据类型枚举
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum OutputDataType {
    DataFrame,
    DataFrameArray,
}

impl Default for OutputDataType {
    fn default() -> Self { OutputDataType::DataFrameArray }
}

/// 数据源算子参数结构体（与 operator.json 中定义的参数对应）
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct DataSourceParams {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    pub query: String,
    /// 排序字段名；为空表示不排序
    pub sort_column: String,
    /// 是否降序（true=降序，false=升序），默认升序
    #[serde(default)]
    pub sort_descending: bool,
    /// 分组字段名；为空表示不分组。若分组且输出类型为 DataFrameArray，
    /// 则每组返回一个 DataFrame 放入数组中。
    pub group_by_column: String,
    /// 返回数据类型：DataFrame 或 DataFrameArray
    #[serde(default)]
    pub output_data_type: OutputDataType,
}

fn parse_params(params_json: &str) -> DataSourceParams {
    if params_json.is_empty() {
        return DataSourceParams::default();
    }
    match serde_json::from_str::<DataSourceParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("解析参数 JSON 失败: {}", e);
            DataSourceParams::default()
        }
    }
}

/// 一次遍历推断所有列的类型（相比每列各自扫一遍）
fn infer_all_column_types(rows: &[Row]) -> Vec<(String, DataType)> {
    if rows.is_empty() { return Vec::new(); }
    let columns = rows[0].columns();
    let mut metas: Vec<(String, DataType)> = columns
        .iter()
        .map(|c| (c.name().to_string(), DataType::Null))
        .collect();
    // 每列还有待推断：遇到第一个非空值即确定类型；若整列空保留 Null
    let n_col = metas.len();
    for row in rows {
        for i in 0..n_col {
            if metas[i].1 != DataType::Null { continue; }
            let name = &*metas[i].0;
            if row.try_get::<&str, Option<i64>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::Int64; continue;
            }
            if row.try_get::<&str, Option<f64>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::Float64; continue;
            }
            if row.try_get::<&str, Option<bool>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::Bool; continue;
            }
            if row.try_get::<&str, Option<String>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::String; continue;
            }
            if row.try_get::<&str, Option<NaiveDateTime>>(name).is_ok_and(|v| v.is_some()) {
                metas[i].1 = DataType::String; continue;
            }
        }
        // 所有列都推断完成可提前退出
        if metas.iter().all(|m| m.1 != DataType::Null) { break; }
    }
    metas
}

/// 根据列元信息与行索引集合，构建 DataFrame。
///
/// 只遍历 `indices` 中的行索引，逐列填充。
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
                DataType::Int64 => {
                    match row.try_get::<&str, Option<i64>>(name) {
                        Ok(v) => column_data.push_i64(v),
                        Err(_) => column_data.push_i64(None),
                    }
                }
                DataType::Float64 => {
                    match row.try_get::<&str, Option<f64>>(name) {
                        Ok(v) => column_data.push_f64(v),
                        Err(_) => column_data.push_f64(None),
                    }
                }
                DataType::Bool => {
                    match row.try_get::<&str, Option<bool>>(name) {
                        Ok(v) => column_data.push_bool(v),
                        Err(_) => column_data.push_bool(None),
                    }
                }
                DataType::String => {
                    match row.try_get::<&str, Option<String>>(name) {
                        Ok(Some(ref v)) => column_data.push_string(Some(v)),
                        Ok(None) => column_data.push_string(None),
                        Err(_) => {
                            match row.try_get::<&str, Option<NaiveDateTime>>(name) {
                                Ok(Some(ref v)) => column_data.push_string(Some(&v.to_string())),
                                _ => column_data.push_string(None),
                            }
                        }
                    }
                }
                DataType::Null => {}
            }
        }
        df.add_column(column_data);
    }
    df
}

/// 把 Row 中的某列转成可比较的排序键（同列空值排在最后）
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
            (Null, _) => Some(Greater), // 空排最后
            (_, Null) => Some(Less),
            (Bool(a), Bool(b)) => a.partial_cmp(b),
            (Int64(a), Int64(b)) => a.partial_cmp(b),
            (Float64(a), Float64(b)) => a.partial_cmp(b),
            (Str(a), Str(b)) => Some(a.cmp(b)),
            _ => None, // 不同类型不比较（实际不会遇到）
        }
    }
}

/// 核心：加载 rows 时一次性完成分组，分组后组内排序，最后按组构建 DataFrame。
///
/// 遍历次数：
///   1. 一次完整扫描所有行：推断列类型 + 按 group_key 分发行索引
///   2. 组内排序：每组排小索引数组
///   3. 各组按索引构建列：只访问属于该组的行
///
/// 相比先整体构建 DataFrame → sort → group_by，避免了 2~3 次全量遍历重建。
fn rows_to_grouped_dataframes(
    rows: Vec<Row>,
    group_by: Option<&str>,
    sort_by: Option<&str>,
    sort_descending: bool,
) -> Vec<DataFrame> {
    if rows.is_empty() {
        return Vec::new();
    }

    // ---- 调试打印前 10 行（保持原行为）
    {
        let print_limit = 10.min(rows.len());
        let row0 = &rows[0];
        let columns = row0.columns();
        let col_names: Vec<&str> = columns.iter().map(|c| c.name()).collect();
        println!("=== 前{}行数据 (共{}行, {}列) ===", print_limit, rows.len(), col_names.len());
        println!("列名: {:?}", col_names);
        for (i, row) in rows.iter().take(print_limit).enumerate() {
            let mut vals: Vec<String> = Vec::new();
            for col in columns.iter() {
                let name = col.name();
                let val = if let Ok(Some(v)) = row.try_get::<&str, Option<i64>>(name) {
                    format!("{}", v)
                } else if let Ok(Some(v)) = row.try_get::<&str, Option<f64>>(name) {
                    format!("{}", v)
                } else if let Ok(Some(v)) = row.try_get::<&str, Option<bool>>(name) {
                    format!("{}", v)
                } else if let Ok(Some(v)) = row.try_get::<&str, Option<String>>(name) {
                    v
                } else if let Ok(Some(v)) = row.try_get::<&str, Option<NaiveDateTime>>(name) {
                    v.to_string()
                } else if let Ok(None) = row.try_get::<&str, Option<i64>>(name) {
                    "NULL".to_string()
                } else if let Ok(None) = row.try_get::<&str, Option<f64>>(name) {
                    "NULL".to_string()
                } else if let Ok(None) = row.try_get::<&str, Option<bool>>(name) {
                    "NULL".to_string()
                } else if let Ok(None) = row.try_get::<&str, Option<String>>(name) {
                    "NULL".to_string()
                } else if let Ok(None) = row.try_get::<&str, Option<NaiveDateTime>>(name) {
                    "NULL".to_string()
                } else {
                    "???".to_string()
                };
                vals.push(val);
            }
            println!("  行{}: {:?}", i, vals);
        }
        println!("=== 打印结束 ===\n");
    }

    // Step1：推断所有列类型（只扫需要的行，全部列推断完就提前停）
    let col_metas = infer_all_column_types(&rows);

    // Step2：一次遍历分组，分发行索引到各组
    // groups: HashMap<key, Vec<usize>> + group_order 保证首次出现顺序
    use std::collections::HashMap;
    let mut group_order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();

    // 如果没有 group_by：整批作为一组（空键）
    let need_group = group_by.is_some();

    for (i, row) in rows.iter().enumerate() {
        let key = if need_group {
            let gcol = group_by.unwrap();
            // 复用推断出的分组列类型（若未推断出来说明全空，键归到 "__NULL__"）
            row.try_get::<&str, Option<i64>>(gcol).ok().flatten()
                .map(|v| format!("i{}", v))
                .or_else(|| row.try_get::<&str, Option<f64>>(gcol).ok().flatten().map(|v| format!("f{:?}", v)))
                .or_else(|| row.try_get::<&str, Option<bool>>(gcol).ok().flatten().map(|v| format!("b{}", v)))
                .or_else(|| {
                    row.try_get::<&str, Option<String>>(gcol).ok().flatten()
                        .map(|v| format!("s{}", v))
                        .or_else(|| {
                            row.try_get::<&str, Option<NaiveDateTime>>(gcol).ok().flatten()
                                .map(|v| format!("t{}", v))
                        })
                })
                .unwrap_or_else(|| "__NULL__".to_string())
        } else {
            String::new() // 未分组：所有行归到同一个空键
        };
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
        }
        groups.entry(key).or_default().push(i);
    }

    // Step3：对每组的 indices 做组内排序（若指定了 sort_by）
    if let Some(sort_col) = sort_by {
        for indices in groups.values_mut() {
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
    }

    // Step4：按各组索引构建 DataFrame
    let mut result: Vec<DataFrame> = Vec::with_capacity(group_order.len());
    for key in &group_order {
        let indices = groups.get(key).unwrap();
        if indices.is_empty() { continue; }
        result.push(build_dataframe_from_indices(&rows, &col_metas, indices));
    }
    result
}

/// 把 DataFrame 数组按原组顺序纵向拼接为单个 DataFrame（用于 output=DataFrame 的场景）
fn concat_dataframes(dfs: &[DataFrame]) -> DataFrame {
    if dfs.is_empty() { return DataFrame::new(); }
    if dfs.len() == 1 { return dfs[0].clone(); }
    let col_metas: Vec<(String, DataType)> = dfs[0].columns
        .iter()
        .map(|c| (c.name.clone(), c.data_type.clone()))
        .collect();
    let mut out_cols: Vec<ColumnData> = col_metas.iter()
        .map(|(n, t)| ColumnData::new(n.clone(), t.clone()))
        .collect();
    for df in dfs {
        for (i, col) in out_cols.iter_mut().enumerate() {
            let src = &df.columns[i];
            match col.data_type {
                DataType::Int64 => for j in 0..src.len() { col.push_i64(src.get_i64(j)); },
                DataType::Float64 => for j in 0..src.len() { col.push_f64(src.get_f64(j)); },
                DataType::Bool => for j in 0..src.len() { col.push_bool(src.get_bool(j)); },
                DataType::String => for j in 0..src.len() { col.push_string(src.get_string(j)); },
                DataType::Null => {}
            }
        }
    }
    let row_count = out_cols.first().map(|c| c.len()).unwrap_or(0);
    DataFrame { columns: out_cols, row_count }
}

/// DataSource 算子的执行函数（C ABI）
///
/// 参数:
/// - inputs: 输入 CPortData 数组指针
/// - input_count: 输入数量
/// - outputs: 输出 CPortData 数组指针（由调用方预分配）
/// - output_cap: 输出数组容量
/// - params_json: 参数 JSON 字符串（C 字符串）
///
/// 返回值:
/// - 0: 成功
/// - 非零: 失败
#[no_mangle]
pub extern "C" fn execute_operator(
    _inputs: *const CPortData,
    _input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        let err_msg = format!("{}", e);
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };

    let params = parse_params(params_json_str);

    eprintln!("[datasource] 数据源参数: host={}, port={}, database={}, user={}, query={}",
        params.host, params.port, params.database, params.username,
        if params.query.is_empty() { "<默认>" } else { &params.query });
    if !params.sort_column.is_empty() {
        eprintln!("[datasource] 排序字段: {} ({})", params.sort_column,
            if params.sort_descending { "降序" } else { "升序" });
    }
    if !params.group_by_column.is_empty() {
        eprintln!("[datasource] 分组字段: {}", params.group_by_column);
    }
    eprintln!("[datasource] 返回数据类型: {:?}", params.output_data_type);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let err_msg = format!("创建 tokio runtime 失败: {}", e);
            let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("[datasource] {}", err_msg);
            return -1;
        }
    };

    // 预取参数引用（避免 async block 内 move 前多次 clone）
    let group_by_ref: Option<String> = (!params.group_by_column.is_empty()).then(|| params.group_by_column.clone());
    let sort_by_ref: Option<String> = (!params.sort_column.is_empty()).then(|| params.sort_column.clone());
    let sort_desc_ref = params.sort_descending;

    let result: Result<Vec<DataFrame>, String> = rt.block_on(async move {
        let mut pg_config = Config::new();
        pg_config
            .host(&params.host)
            .port(params.port)
            .dbname(&params.database)
            .user(&params.username)
            .password(&params.password);

        eprintln!("[datasource] 正在连接 PostgreSQL: {}:{}/{} (用户: {})",
            params.host, params.port, params.database, params.username);

        let (client, connection) = match pg_config.connect(NoTls).await {
            Ok(c) => {
                eprintln!("[datasource] PostgreSQL 连接成功");
                c
            }
            Err(e) => {
                let err_detail = format!(
                    "连接 PostgreSQL 失败: {}:{}/{} (用户: {}) - 错误: {}",
                    params.host, params.port, params.database, params.username, e
                );
                eprintln!("[datasource] {}", err_detail);
                return Err(err_detail);
            }
        };

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("[datasource] PostgreSQL 连接错误: {}", e);
            }
        });

        let query = if params.query.is_empty() {
            "SELECT * FROM stock_prices LIMIT 100".to_string()
        } else {
            params.query.clone()
        };

        eprintln!("[datasource] 执行 SQL: {}", query);

        let rows = match client.query(&query, &[]).await {
            Ok(r) => {
                eprintln!("[datasource] SQL 执行成功，返回 {} 行", r.len());
                r
            }
            Err(e) => {
                let err_detail = format!("执行 SQL 查询失败: {} - SQL: {}", e, query);
                eprintln!("[datasource] {}", err_detail);
                return Err(err_detail);
            }
        };

        // 一次遍历：加载时即完成分组 + 组内排序
        let dfs = rows_to_grouped_dataframes(
            rows,
            group_by_ref.as_deref(),
            sort_by_ref.as_deref(),
            sort_desc_ref,
        );
        Ok(dfs)
    });

    let dfs = match result {
        Ok(dfs) => dfs,
        Err(e) => {
            let err_msg = format!("数据源算子执行失败: {}", e);
            let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("[datasource] {}", err_msg);
            return -2;
        }
    };

    if !params.group_by_column.is_empty() {
        eprintln!("[datasource] 分组完成 (字段: {})，共 {} 组", params.group_by_column, dfs.len());
    }
    if params.sort_column.is_empty() && !params.group_by_column.is_empty() {
        eprintln!("[datasource] 注意：未设置排序字段，组内行顺序按查询返回顺序");
    }

    // ---- 动态返回类型 ----
    let port_data: PortData = match params.output_data_type {
        OutputDataType::DataFrameArray => {
            eprintln!("[datasource] 返回类型: DataFrameArray（{} 个 DataFrame）", dfs.len());
            PortData::DataFrameArray(dfs)
        }
        OutputDataType::DataFrame => {
            // 合并多组为一个 DataFrame（组按出现顺序纵向拼接；若只有一组则直接解包）
            let single = concat_dataframes(&dfs);
            eprintln!("[datasource] 返回类型: DataFrame（{} 行，{} 列）", single.row_count, single.col_count());
            PortData::DataFrame(single)
        }
    };

    // 查询结果为空时给出明确日志（仍视为成功，空结果是合法的）
    let is_empty = match &port_data {
        PortData::DataFrame(df) => df.row_count == 0,
        PortData::DataFrameArray(arr) => arr.iter().all(|d| d.row_count == 0),
        _ => false,
    };
    if is_empty {
        eprintln!("警告: 查询返回 0 行数据，输出空结果");
    }

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 将结果封装为 C ABI PortData 并写入 outputs
    if !outputs.is_null() && output_cap > 0 {
        let c_pd = portdata_to_c(&port_data);
        unsafe {
            *outputs = c_pd;
            // 在第二个槽位置 TYPE_NULL 表示输出结束
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: operator_runtime::c_abi::TYPE_NULL,
                    value: CPortValue { str_ptr: std::ptr::null_mut() },
                };
            }
        }
    }

    0
}

/// 释放 C ABI PortData 内存（由调用方调用）
#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        c_pd_free(data_ptr);
    }
}

/// 获取数据源算子版本
#[no_mangle]
pub extern "C" fn datasource_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;