use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::{c_char, CStr, CString};

/// 最高价算子参数结构体
///
/// - `n`：滚动窗口周期（字符串形式，与前端字符串输入一致）。空串回退默认 20；
///   必须能解析为正整数，否则报错（-6）。常用 N=20（20 日最高价）。
/// - `column_name`：结果列名。为空时自动取 `highest_{n}`；
///   与现有列同名则覆盖，否则追加为新列（Float64 类型）。
/// - `source_column`：源价格列名。空串回退默认 `high`（最高价）。
///   Float64 / Int64 支持（Int64 会提升为 f64）。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct HighestParams {
    #[serde(default)]
    pub n: String,
    #[serde(default)]
    pub column_name: String,
    #[serde(default)]
    pub source_column: String,
}

/// 解析参数 JSON 为 HighestParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> HighestParams {
    if params_json.is_empty() {
        return HighestParams::default();
    }
    match serde_json::from_str::<HighestParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("最高价算子: 解析参数 JSON 失败: {}", e);
            HighestParams::default()
        }
    }
}

/// 解析 n（窗口周期）：空串回退默认 20；必须为正整数，否则返回 None（调用方报错 -6）
fn parse_n(raw: &str) -> Option<usize> {
    let t = raw.trim();
    if t.is_empty() {
        return Some(20);
    }
    match t.parse::<usize>() {
        Ok(v) if v >= 1 => Some(v),
        _ => None,
    }
}

/// 解析源列名；空串或纯空格回退默认指定列
fn resolve_column(raw: &str, default: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        default.to_string()
    } else {
        t.to_string()
    }
}

/// 提取 DataFrame 指定列为 f64 序列；支持 Float64/Int64（Int64 提升为 f64）。
/// 列不存在、或类型非数值 → 返回 None（由调用方决定跳过）
fn extract_f64_column(df: &DataFrame, column: &str) -> Option<Vec<Option<f64>>> {
    let col = df.column(column)?;
    match col.data_type {
        DataType::Float64 => Some(col.to_f64_vec()),
        DataType::Int64 => Some(
            col.to_i64_vec()
                .into_iter()
                .map(|v| v.map(|x| x as f64))
                .collect(),
        ),
        _ => None,
    }
}

/// 计算过去 n 个值的滚动最大值（窗口 `[i-n+1, i]`，含当前值）
///
/// 语义对齐 `pandas.Series.rolling(window=n).max()`：
/// - 前 n-1 行（窗口不足 n 个值）→ `None`
/// - 窗口内任一值为空 → `None`（空值传播，等价 `min_periods=window`）
///
/// 采用单调递减双端队列实现，时间复杂度 O(n)：
/// - 队列中存"有效值"的索引，队首始终为当前窗口最大值索引
/// - 新值入队时弹出队尾所有 ≤ 它的索引（保持单调递减）
/// - 窗口左端出界时弹出队首（若队首即离开索引）
/// - 同时维护窗口内 None 计数；none_count > 0 时整窗 None
fn compute_rolling_max(values: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    let len = values.len();
    if n == 0 || len == 0 {
        return vec![None; len];
    }

    let mut result = Vec::with_capacity(len);
    // 单调递减队列，存有效值（Some）的索引；队首为当前窗口最大值索引
    let mut deque: VecDeque<usize> = VecDeque::with_capacity(n);
    // 窗口内 None 值计数；none_count > 0 → 整窗 None
    let mut none_count = 0usize;

    for i in 0..len {
        // 窗口右端进入
        match values[i] {
            Some(v) => {
                // 弹出队尾所有 ≤ v 的索引（保持单调递减；相等值取较新索引更优）
                while let Some(&back) = deque.back() {
                    if values[back].unwrap() <= v {
                        deque.pop_back();
                    } else {
                        break;
                    }
                }
                deque.push_back(i);
            }
            None => {
                none_count += 1;
            }
        }

        // 窗口左端离开（i >= n 时，i-n 即将移出窗口）
        if i >= n {
            let out = i - n;
            match values[out] {
                Some(_) => {
                    // 若队首恰好是离开的索引，弹出
                    if deque.front() == Some(&out) {
                        deque.pop_front();
                    }
                }
                None => {
                    none_count -= 1;
                }
            }
        }

        // 窗口未填满（i+1 < n）→ None
        if i + 1 < n {
            result.push(None);
            continue;
        }

        // 窗口含空值 → None（空值传播）
        if none_count > 0 {
            result.push(None);
        } else {
            // none_count == 0 时窗口内全为有效值，队首必在窗口内
            let max_idx = *deque.front().unwrap();
            result.push(Some(values[max_idx].unwrap()));
        }
    }

    result
}

/// 对单个 DataFrame 就地写入最高价列。
///
/// - `source_column` 需为 Float64 或 Int64（Int64 提升为 f64）。
/// - 源列不存在或非数值类型 → 跳过并告警，DataFrame 原样保留。
/// - `result_col == source_column` → 就地覆盖对应列；
///   否则覆盖同名列或新增列（源列保留）。
fn apply_highest(df: &mut DataFrame, source_column: &str, n: usize, result_col: &str) {
    let source_values = match extract_f64_column(df, source_column) {
        Some(v) => v,
        None => {
            eprintln!(
                "最高价算子: 源列 '{}' 不存在或类型不支持 (现有列: {:?})，跳过",
                source_column,
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            return;
        }
    };

    let out = compute_rolling_max(&source_values, n);
    let new_col = DataFrame::new_float64_column(result_col, out);

    // 写入结果列：同名就地覆盖 source_column；否则覆盖已有列或新增列
    if result_col == source_column {
        if let Some(pos) = df.columns.iter().position(|c| c.name == result_col) {
            df.columns[pos] = new_col;
        } else {
            df.add_column(new_col);
        }
    } else {
        match df.columns.iter().position(|c| c.name == result_col) {
            Some(p) => df.columns[p] = new_col,
            None => df.add_column(new_col),
        }
    }
}

/// 最高价算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立计算过去 N 日最高价，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 单个 DataFrame 输入会被包装为单元素数组处理，输出仍为 DataFrameArray。
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
/// - -6: 参数 n 非法（非正整数）
#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        let err_msg = format!("{}", e);
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("最高价算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    // n：空串回退 20；非正整数报错
    let n = match parse_n(&params.n) {
        Some(v) => v,
        None => {
            let err = format!(
                "参数 n='{}' 非法 (需为正整数，如 20)；空串将回退默认 20",
                params.n
            );
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("最高价算子: {}", err);
            return -6;
        }
    };

    let source_column = resolve_column(&params.source_column, "high");

    // 结果列名：空串自动取 highest_{n}
    let result_col = {
        let t = params.column_name.trim();
        if t.is_empty() {
            format!("highest_{}", n)
        } else {
            t.to_string()
        }
    };

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("最高价算子: {}", err);
        return -3;
    }

    // 从输入中提取 DataFrame 数组（兼容单个 DataFrame）
    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let input_dfs: Vec<DataFrame> = match input_pd {
        PortData::DataFrame(df) => vec![df],
        PortData::DataFrameArray(dfs) => dfs,
        _ => {
            let err = "输入不是 DataFrame / DataFrameArray 类型".to_string();
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("最高价算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("最高价算子: {}", err);
        return -5;
    }

    println!(
        "最高价算子: n={}, source='{}', result='{}', 输入 DataFrame 数量={}, 首个行数={}",
        n, source_column, result_col, input_dfs.len(), input_dfs[0].row_count
    );

    // 逐个 DataFrame 计算最高价（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("最高价算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_highest(df, &source_column, n, &result_col);
    }

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出统一为 DataFrameArray（与端口声明一致）
    let port_data = PortData::DataFrameArray(out_dfs);
    if !outputs.is_null() && output_cap > 0 {
        // 使用 owned 变体，避免每个 DataFrame 被 clone
        let c_pd = portdata_to_c_owned(port_data);
        unsafe {
            *outputs = c_pd;
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
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
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

/// 获取最高价算子版本
#[no_mangle]
pub extern "C" fn highest_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
