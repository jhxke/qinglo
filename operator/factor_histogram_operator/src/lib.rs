use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CStr, CString};

// =============================================================================
// 参数解析
// =============================================================================

/// 因子直方图算子参数结构体（全部 String，与前端字符串输入一致）
///
/// - `factor_column`: 因子列名（X 轴分箱依据），需 Float64 / Int64
/// - `return_column`: 收益率列名（Y 轴均值统计对象），需 Float64 / Int64
/// - `bins`: 分箱数量（字符串形式），空串回退默认 20；必须为正整数
/// - `min_val`: 可选因子最小边界（字符串形式），空串自动取数据最小值
/// - `max_val`: 可选因子最大边界（字符串形式），空串自动取数据最大值
/// - `result_column`: 收益率均值列名，空串回退默认 `mean_return`；
///   与保留列名(bin_index/bin_left/bin_right/bin_center/count/frequency)冲突时回退 `mean_return`
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct FactorHistogramParams {
    #[serde(default)]
    pub factor_column: String,
    #[serde(default)]
    pub return_column: String,
    #[serde(default)]
    pub bins: String,
    #[serde(default)]
    pub min_val: String,
    #[serde(default)]
    pub max_val: String,
    #[serde(default)]
    pub result_column: String,
}

fn parse_params(params_json: &str) -> FactorHistogramParams {
    if params_json.is_empty() {
        return FactorHistogramParams::default();
    }
    match serde_json::from_str::<FactorHistogramParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("因子直方图算子: 解析参数 JSON 失败: {}", e);
            FactorHistogramParams::default()
        }
    }
}

/// 解析 bins：空串回退默认 20；必须为正整数，否则返回 None（调用方报错 -6）
fn parse_bins(raw: &str) -> Option<usize> {
    let t = raw.trim();
    if t.is_empty() {
        return Some(20);
    }
    match t.parse::<usize>() {
        Ok(v) if v >= 1 => Some(v),
        _ => None,
    }
}

fn parse_f64_opt(raw: &str) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

/// 解析列名；空串或纯空格回退默认指定列
fn resolve_column(raw: &str, default: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        default.to_string()
    } else {
        t.to_string()
    }
}

// =============================================================================
// DataFrame 适配
// =============================================================================

/// 提取 DataFrame 指定列为 f64 序列；支持 Float64/Int64（Int64 提升为 f64）。
/// 列不存在或非数值类型 → None
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

// =============================================================================
// 样本收集与分箱
// =============================================================================

/// 一对有效样本：(因子值, 收益率值)
type Sample = (f64, f64);

/// 从所有 DataFrame 中收集 (因子值, 收益率值) 同行配对的有效样本
///
/// - 因子列或收益率列不存在 / 非数值类型 → 返回 Err（调用方报错 -8）
/// - 因子值或收益率为空 / Inf / NaN → 跳过该行（不参与分箱与均值统计）
fn collect_samples(
    input_dfs: &[DataFrame],
    factor_col: &str,
    return_col: &str,
) -> Result<Vec<Sample>, String> {
    let mut samples: Vec<Sample> = Vec::new();

    for (df_idx, df) in input_dfs.iter().enumerate() {
        if df.row_count == 0 {
            continue;
        }

        let factor_vals = match extract_f64_column(df, factor_col) {
            Some(v) => v,
            None => {
                return Err(format!(
                    "第 {} 个 DataFrame 中因子列 '{}' 不存在或类型不支持 (现有列: {:?})",
                    df_idx,
                    factor_col,
                    df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                ));
            }
        };

        let return_vals = match extract_f64_column(df, return_col) {
            Some(v) => v,
            None => {
                return Err(format!(
                    "第 {} 个 DataFrame 中收益率列 '{}' 不存在或类型不支持 (现有列: {:?})",
                    df_idx,
                    return_col,
                    df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                ));
            }
        };

        let row_count = df.row_count;
        for row in 0..row_count {
            // 因子值必须非空且有限
            let f = match factor_vals.get(row).copied().flatten() {
                Some(v) if v.is_finite() => v,
                _ => continue,
            };
            // 收益率值必须非空且有限
            let r = match return_vals.get(row).copied().flatten() {
                Some(v) if v.is_finite() => v,
                _ => continue,
            };
            samples.push((f, r));
        }
    }

    Ok(samples)
}

/// 构建因子直方图 DataFrame（等宽分箱，每箱统计样本数 + 收益率均值）
///
/// 分箱策略：在 `[range_min, range_max]` 上等宽划分 `bins` 个箱，
/// 每箱左闭右开 `[left, right)`，最后一箱右闭 `[left, range_max]`（含最大值）。
///
/// 输出列（与「直方图展示算子」默认列名 100% 兼容，可直接接 viz 算子）：
/// - `bin_index`  (Int64)   —— 0 基分箱编号
/// - `bin_left`   (Float64) —— 箱左边界（含）
/// - `bin_right`  (Float64) —— 箱右边界（不含，最后一箱含最大值）
/// - `bin_center` (Float64) —— 箱中点 = (left+right)/2（默认 X 轴）
/// - `count`      (Int64)   —— 落入该箱的样本数量
/// - `frequency`  (Float64) —— 样本频率 = count / 总样本数
/// - `{result_col}`(Float64) —— 该箱收益率均值（默认列名 `mean_return`，即默认 Y 轴）
fn build_factor_histogram_dataframe(
    samples: &[Sample],
    bins: usize,
    min_opt: Option<f64>,
    max_opt: Option<f64>,
    result_col: &str,
) -> DataFrame {
    let mut df = DataFrame::new();

    // ---------- Step 1: 决定分箱范围 ----------
    // 数据范围自动取所有样本因子值的最小/最大值；用户显式边界覆盖
    let (data_min, data_max) = if samples.is_empty() {
        (0.0, 0.0)
    } else {
        let mn = samples.iter().map(|(f, _)| *f).fold(f64::INFINITY, f64::min);
        let mx = samples.iter().map(|(f, _)| *f).fold(f64::NEG_INFINITY, f64::max);
        (mn, mx)
    };

    let mut range_min = min_opt.unwrap_or(data_min);
    let mut range_max = max_opt.unwrap_or(data_max);
    // 用户边界颠倒自动对调
    if range_min > range_max {
        std::mem::swap(&mut range_min, &mut range_max);
    }
    // 避免零宽度分箱（min == max）向两侧各扩展 0.5
    if (range_max - range_min).abs() < 1e-12 {
        range_min -= 0.5;
        range_max += 0.5;
    }

    let bin_width = (range_max - range_min) / bins as f64;
    let bin_width = if bin_width <= 0.0 { 1e-9 } else { bin_width };

    let total_count = samples.len() as f64;

    // ---------- Step 2: 分箱累加（sum + count） ----------
    let mut sums: Vec<f64> = vec![0.0; bins];
    let mut counts: Vec<i64> = vec![0; bins];

    if !samples.is_empty() && bin_width > 0.0 {
        for &(f, r) in samples {
            // 范围外的样本不统计（用户显式收窄边界时生效）
            if f < range_min || f > range_max {
                continue;
            }
            let mut idx = ((f - range_min) / bin_width).floor() as i64;
            if idx < 0 {
                idx = 0;
            }
            if idx >= bins as i64 {
                idx = bins as i64 - 1; // 最后一箱右闭，含 range_max
            }
            let u = idx as usize;
            sums[u] += r;
            counts[u] += 1;
        }
    }

    // ---------- Step 3: 构造各列 ----------
    let mut bin_index_col: Vec<Option<i64>> = Vec::with_capacity(bins);
    let mut bin_left_col: Vec<Option<f64>> = Vec::with_capacity(bins);
    let mut bin_right_col: Vec<Option<f64>> = Vec::with_capacity(bins);
    let mut bin_center_col: Vec<Option<f64>> = Vec::with_capacity(bins);
    let mut count_col: Vec<Option<i64>> = Vec::with_capacity(bins);
    let mut freq_col: Vec<Option<f64>> = Vec::with_capacity(bins);
    let mut mean_col: Vec<Option<f64>> = Vec::with_capacity(bins);

    for i in 0..bins {
        let left = range_min + (i as f64) * bin_width;
        let right = left + bin_width;
        let center = (left + right) / 2.0;
        let cnt = counts[i];
        let freq = if total_count > 0.0 { cnt as f64 / total_count } else { 0.0 };
        // 空箱均值置 None（前端渲染时柱高为 0）
        let mean = if cnt > 0 { Some(sums[i] / cnt as f64) } else { None };

        bin_index_col.push(Some(i as i64));
        bin_left_col.push(Some(left));
        bin_right_col.push(Some(right));
        bin_center_col.push(Some(center));
        count_col.push(Some(cnt));
        freq_col.push(Some(freq));
        mean_col.push(mean);
    }

    df.add_column(DataFrame::new_int64_column("bin_index", bin_index_col));
    df.add_column(DataFrame::new_float64_column("bin_left", bin_left_col));
    df.add_column(DataFrame::new_float64_column("bin_right", bin_right_col));
    df.add_column(DataFrame::new_float64_column("bin_center", bin_center_col));
    df.add_column(DataFrame::new_int64_column("count", count_col));
    df.add_column(DataFrame::new_float64_column("frequency", freq_col));
    df.add_column(DataFrame::new_float64_column(result_col, mean_col));

    println!(
        "因子直方图算子: 分箱统计 bins={}, bin_width={}, 范围=[{}, {}], 样本数={}, 均值列='{}'",
        bins, bin_width, range_min, range_max, samples.len(), result_col
    );

    df
}

// =============================================================================
// C ABI 入口
// =============================================================================

/// 因子直方图算子的执行函数（C ABI）
///
/// 输入: DataFrameArray（兼容单个 DataFrame，会被包装为单元素数组）
/// 流程: 汇总所有 DataFrame 中 (因子列, 收益率列) 同行配对的有效样本，
///       按因子值等宽分箱，每箱统计样本数 `count` 与收益率均值 `mean_return`。
/// 输出: 单个 DataFrame（直方图），包含列: bin_index, bin_left, bin_right,
///       bin_center, count, frequency, {result_column}(默认 mean_return)。
///       列结构与「直方图展示算子」默认配置完全兼容，可直接连接做可视化。
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
/// - -6: 参数非法（factor_column / return_column 空，或 bins 非正整数）
/// - -8: 列不存在或类型不支持（因子列 / 收益率列）
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
        eprintln!("因子直方图算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    // 因子列名：必填，空串报错
    let factor_col = params.factor_column.trim().to_string();
    if factor_col.is_empty() {
        let err = "factor_column 参数为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("因子直方图算子: {}", err);
        return -6;
    }

    // 收益率列名：必填，空串报错
    let return_col = params.return_column.trim().to_string();
    if return_col.is_empty() {
        let err = "return_column 参数为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("因子直方图算子: {}", err);
        return -6;
    }

    // bins：空串回退 20；非正整数报错
    let bins = match parse_bins(&params.bins) {
        Some(v) => v,
        None => {
            let err = format!("bins='{}' 非法 (需为正整数，空串默认 20)", params.bins);
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("因子直方图算子: {}", err);
            return -6;
        }
    };

    let min_opt = parse_f64_opt(&params.min_val);
    let max_opt = parse_f64_opt(&params.max_val);

    // 均值列名：空串回退 mean_return；与保留列名冲突时回退 mean_return
    let result_col = {
        let name = resolve_column(&params.result_column, "mean_return");
        const RESERVED: [&str; 6] = [
            "bin_index", "bin_left", "bin_right", "bin_center", "count", "frequency",
        ];
        if RESERVED.contains(&name.as_str()) {
            eprintln!(
                "因子直方图算子: result_column='{}' 与保留列名冲突，回退为 'mean_return'",
                name
            );
            "mean_return".to_string()
        } else {
            name
        }
    };

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("因子直方图算子: {}", err);
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
            eprintln!("因子直方图算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("因子直方图算子: {}", err);
        return -5;
    }

    println!(
        "因子直方图算子: factor_column='{}', return_column='{}', bins={}, min={:?}, max={:?}, result='{}', 输入 DataFrame 数量={}",
        factor_col, return_col, bins, min_opt, max_opt, result_col, input_dfs.len()
    );

    // 收集所有有效配对样本
    let samples = match collect_samples(&input_dfs, &factor_col, &return_col) {
        Ok(s) => s,
        Err(e) => {
            let err = format!("收集样本失败: {}", e);
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("因子直方图算子: {}", err);
            return -8;
        }
    };

    println!(
        "因子直方图算子: 收集到 {} 个有效配对样本 (因子范围 {:?})",
        samples.len(),
        if samples.is_empty() {
            None
        } else {
            Some((
                samples.iter().map(|(f, _)| *f).fold(f64::INFINITY, f64::min),
                samples.iter().map(|(f, _)| *f).fold(f64::NEG_INFINITY, f64::max),
            ))
        }
    );

    // 构建因子直方图 DataFrame
    let histogram_df =
        build_factor_histogram_dataframe(&samples, bins, min_opt, max_opt, &result_col);

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出为单个 DataFrame（与端口声明一致）
    let port_data = PortData::DataFrame(histogram_df);
    if !outputs.is_null() && output_cap > 0 {
        // 使用 owned 变体，避免 DataFrame 被 clone
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

/// 获取因子直方图算子版本
#[no_mangle]
pub extern "C" fn factor_histogram_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
