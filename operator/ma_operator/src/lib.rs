use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_from_c,
    c_set_last_error, TYPE_NULL,
};
use std::ffi::{CStr, CString, c_char};
use serde::{Deserialize, Serialize};

/// MA 算子参数结构体
///
/// `ma_periods` 为逗号分隔的周期串（如 "5,10,20"），与前端 String 参数渲染一致；
/// 留空表示不计算（原样返回输入）。
/// `source_column` 指定计算所用源列名，留空回退默认 "close"。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct MaParams {
    /// MA 均线周期列表，逗号分隔，如 "5,10,20"；空则不计算
    #[serde(default)]
    pub ma_periods: String,
    /// 指标计算所用的源列名；空则回退默认 "close"
    #[serde(default)]
    pub source_column: String,
}

/// 解析参数 JSON 为 MaParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> MaParams {
    if params_json.is_empty() {
        return MaParams::default();
    }
    match serde_json::from_str::<MaParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("解析参数 JSON 失败: {}", e);
            MaParams::default()
        }
    }
}

/// 解析逗号分隔的周期字符串，如 "5,10,20" -> vec![5, 10, 20]；非正数自动过滤
fn parse_periods(periods_str: &str) -> Vec<usize> {
    periods_str
        .split(',')
        .filter_map(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                trimmed.parse::<usize>().ok().filter(|&p| p > 0)
            }
        })
        .collect()
}

/// 计算简单移动平均线 (SMA) —— 滚动求和实现
///
/// 窗口内忽略空值，按有效值求平均；窗口不足 N 行时返回 None。
/// 使用滚动求和（窗口滑出时减去离开元素、滑入时加上进入元素），
/// 将时间复杂度从 O(n×period) 降低到 O(n)，对长周期（如 MA250）提升显著。
/// 浮点累积误差在金融数据量级下可忽略（period ≤ 1000 时相对误差 < 1e-12）。
fn compute_sma(values: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
    let n = values.len();
    if period == 0 || n == 0 {
        return vec![None; n];
    }

    let mut result = Vec::with_capacity(n);
    let mut sum = 0.0f64;
    let mut valid_count = 0usize;

    for i in 0..n {
        // 窗口右端进入
        if let Some(val) = values[i] {
            sum += val;
            valid_count += 1;
        }
        // 窗口左端离开（窗口大小 = period）
        if i >= period {
            if let Some(val) = values[i - period] {
                sum -= val;
                valid_count -= 1;
            }
        }
        // 窗口填满后输出
        if i + 1 >= period {
            if valid_count == 0 {
                result.push(None);
            } else {
                result.push(Some(sum / valid_count as f64));
            }
        } else {
            result.push(None);
        }
    }

    result
}

/// 解析源列名；空串或纯空格回退默认 "close"
///
/// DataFrameArray 不一定含有标准的 open/high/low/close 列，
/// 通过该函数把空配置统一回退到 "close"，保持向后兼容。
fn resolve_source_column(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "close"
    } else {
        trimmed
    }
}

/// 从 DataFrame 中提取指定列的 f64 数据；列不存在或非 Float64 类型时返回 None
fn extract_column_values(df: &DataFrame, column_name: &str) -> Option<Vec<Option<f64>>> {
    let col = df.column(column_name)?;
    if !matches!(col.data_type, DataType::Float64) {
        return None;
    }
    Some(col.to_f64_vec())
}

/// 对单个 DataFrame **就地**追加 MA 列，避免全量 clone。
///
/// 优化点：
/// - **就地修改**：直接在输入 DataFrame 上 `add_column`，不再 `clone()` 整个表
/// - 源列不存在或类型不匹配时仅跳过并打印告警，不影响其他 DataFrame
/// - 参数为空时，原样返回（不追加列）
fn apply_ma_inplace(df: &mut DataFrame, params: &MaParams) {
    if params.ma_periods.is_empty() {
        println!("MA算子: 未配置任何 MA 周期，原样返回输入");
        return;
    }

    let source_col = resolve_source_column(&params.source_column);
    let source_values = match extract_column_values(df, source_col) {
        Some(v) => v,
        None => {
            let existing_cols: Vec<&str> = df.columns.iter().map(|c| c.name.as_str()).collect();
            eprintln!(
                "MA算子: 源列 '{}' 不存在或类型不匹配 (现有列: {:?})，跳过 MA",
                source_col, existing_cols
            );
            return;
        }
    };

    let periods = parse_periods(&params.ma_periods);
    if periods.is_empty() {
        eprintln!(
            "MA算子: MA 周期串 '{}' 未解析出有效周期，跳过 MA",
            params.ma_periods
        );
        return;
    }

    for &period in &periods {
        let ma_values = compute_sma(&source_values, period);
        let col_name = format!("ma_{}", period);
        df.add_column(DataFrame::new_float64_column(&col_name, ma_values));
    }
}

/// MA 算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立做 MA 计算，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 为兼容旧 DAG，单个 DataFrame 输入会被包装为单元素数组处理，
/// 输出仍为 DataFrameArray（单元素）。
///
/// 返回值:
/// - 0: 成功（包括参数为空、源列缺失时静默原样返回的情形）
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
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
        eprintln!("{}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };

    let params = parse_params(params_json_str);

    if input_count == 0 || inputs.is_null() {
        let err_msg = "缺少输入数据".to_string();
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -3;
    }

    // 从输入中提取 DataFrame 数组（兼容单个 DataFrame）
    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let input_dfs: Vec<DataFrame> = match input_pd {
        PortData::DataFrame(df) => vec![df],
        PortData::DataFrameArray(dfs) => dfs,
        _ => {
            let err_msg = "输入不是 DataFrame / DataFrameArray 类型".to_string();
            let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("{}", err_msg);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err_msg = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -5;
    }

    let ma_configured = !params.ma_periods.is_empty();
    println!(
        "MA算子: MA={}, 输入 DataFrame 数量={}, 首个行数={}",
        if ma_configured { "已配置" } else { "未配置" },
        input_dfs.len(),
        input_dfs[0].row_count
    );

    // 逐个 DataFrame 就地做 MA 计算（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("MA算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_ma_inplace(df, &params);
    }

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出统一为 DataFrameArray（与端口声明一致）
    let port_data = PortData::DataFrameArray(out_dfs);

    if !outputs.is_null() && output_cap > 0 {
        // 使用 owned 变体，避免每个 DataFrame 被 clone
        let c_pd = operator_runtime::c_abi::portdata_to_c_owned(port_data);
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

/// 获取 MA 算子版本
#[no_mangle]
pub extern "C" fn ma_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
