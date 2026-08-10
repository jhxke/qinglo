use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::{c_char, CStr, CString};

/// 量价能量因子算子参数结构体
///
/// - `n`：滚动窗口周期（字符串形式，与前端字符串输入一致）。空串回退默认 20；
///   必须能解析为正整数，否则报错（-6）。常用 N=20（20 日线，1 个月）。
/// - `price_column`：收盘价列名。空串回退默认 `close`。Float64 / Int64 支持。
/// - `volume_column`：成交量列名。空串回退默认 `volume`。Float64 / Int64 支持
///   （Int64 会提升为 f64）。
/// - `result_column`：结果因子列名。为空时自动取 `factor_vc_{n}`；
///   与 `price_column` 或 `volume_column` 同名则就地覆盖对应列，否则新增列。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct FactorVcParams {
    #[serde(default)]
    pub n: String,
    #[serde(default)]
    pub price_column: String,
    #[serde(default)]
    pub volume_column: String,
    #[serde(default)]
    pub result_column: String,
}

/// 解析参数 JSON 为 FactorVcParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> FactorVcParams {
    if params_json.is_empty() {
        return FactorVcParams::default();
    }
    match serde_json::from_str::<FactorVcParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("量价能量因子算子: 解析参数 JSON 失败: {}", e);
            FactorVcParams::default()
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

/// 计算当日收益率序列 `ret[i] = (price[i] - price[i-1]) / price[i-1]`
///
/// 语义对齐 `pandas.Series.pct_change()`（周期=1）：
/// - 首行（i=0，无前值）→ `None`
/// - 当日价或前一日价为空 → `None`（空值传播）
/// - 前一日价 == 0 → `None`（避免除零）
fn compute_daily_return(values: &[Option<f64>]) -> Vec<Option<f64>> {
    let len = values.len();
    if len == 0 {
        return vec![];
    }
    let mut result = Vec::with_capacity(len);
    result.push(None); // 首行无前值
    for i in 1..len {
        match (values[i], values[i - 1]) {
            (Some(cur), Some(prev)) => {
                if prev == 0.0 {
                    result.push(None);
                } else {
                    result.push(Some((cur - prev) / prev));
                }
            }
            _ => result.push(None),
        }
    }
    result
}

/// 计算过去 n 个值的滚动均值（窗口 `[i-n+1, i]`，含当前值）
///
/// 语义对齐 `pandas.Series.rolling(window=n).mean()`：
/// - 前 n-1 行（窗口不足 n 个值）→ `None`
/// - 窗口内任一值为空 → `None`（空值传播，等价 `min_periods=window`）
///
/// 使用滚动求和实现，时间复杂度 O(n)。
fn compute_rolling_mean(values: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    let len = values.len();
    if n == 0 || len == 0 {
        return vec![None; len];
    }

    let mut result = Vec::with_capacity(len);
    let mut sum = 0.0f64;
    let mut valid_count = 0usize;
    let mut none_count = 0usize;

    for i in 0..len {
        if let Some(val) = values[i] {
            sum += val;
            valid_count += 1;
        } else {
            none_count += 1;
        }
        if i >= n {
            if let Some(val) = values[i - n] {
                sum -= val;
                valid_count -= 1;
            } else {
                none_count -= 1;
            }
        }
        if i + 1 < n {
            result.push(None);
            continue;
        }
        if none_count > 0 || valid_count < n {
            result.push(None);
        } else {
            result.push(Some(sum / valid_count as f64));
        }
    }

    result
}

/// 计算过去 n 个值的滚动最大值（窗口 `[i-n+1, i]`，含当前值）
///
/// 语义对齐 `pandas.Series.rolling(window=n).max()`：
/// - 前 n-1 行 → `None`；窗口内任一值为空 → `None`（空值传播）
///
/// 采用单调递减双端队列实现，时间复杂度 O(n)。
fn compute_rolling_max(values: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    let len = values.len();
    if n == 0 || len == 0 {
        return vec![None; len];
    }

    let mut result = Vec::with_capacity(len);
    let mut deque: VecDeque<usize> = VecDeque::with_capacity(n);
    let mut none_count = 0usize;

    for i in 0..len {
        match values[i] {
            Some(v) => {
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
        if i >= n {
            let out = i - n;
            match values[out] {
                Some(_) => {
                    if deque.front() == Some(&out) {
                        deque.pop_front();
                    }
                }
                None => {
                    none_count -= 1;
                }
            }
        }
        if i + 1 < n {
            result.push(None);
            continue;
        }
        if none_count > 0 {
            result.push(None);
        } else {
            let max_idx = *deque.front().unwrap();
            result.push(Some(values[max_idx].unwrap()));
        }
    }

    result
}

/// 计算过去 n 个值的滚动最小值（窗口 `[i-n+1, i]`，含当前值）
///
/// 语义对齐 `pandas.Series.rolling(window=n).min()`：
/// - 前 n-1 行 → `None`；窗口内任一值为空 → `None`（空值传播）
///
/// 采用单调递增双端队列实现，时间复杂度 O(n)。
fn compute_rolling_min(values: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    let len = values.len();
    if n == 0 || len == 0 {
        return vec![None; len];
    }

    let mut result = Vec::with_capacity(len);
    let mut deque: VecDeque<usize> = VecDeque::with_capacity(n);
    let mut none_count = 0usize;

    for i in 0..len {
        match values[i] {
            Some(v) => {
                while let Some(&back) = deque.back() {
                    if values[back].unwrap() >= v {
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
        if i >= n {
            let out = i - n;
            match values[out] {
                Some(_) => {
                    if deque.front() == Some(&out) {
                        deque.pop_front();
                    }
                }
                None => {
                    none_count -= 1;
                }
            }
        }
        if i + 1 < n {
            result.push(None);
            continue;
        }
        if none_count > 0 {
            result.push(None);
        } else {
            let min_idx = *deque.front().unwrap();
            result.push(Some(values[min_idx].unwrap()));
        }
    }

    result
}

/// 计算量价能量因子 `factor_vc = M + W × S`
///
/// 各分量（窗口 `[i-n+1, i]`，`min_periods=N`）：
/// - `M = (Pt - Pavg) / Pavg`：价格趋势项
/// - `S = (1/N) × Σ sign(ret[j]) × ln(1 + Vt[j]/Vavg)`：加权量能项
/// - `pos = (Pt - Ln) / (Hn - Ln)`；`W = 1 - pos^1.5`：位置惩罚项
///
/// 空值传播规则：
/// - 前 n-1 行 → `None`
/// - 窗口内任一 close/volume 为空 → `None`
/// - `ret[0]`（首行）为空 → S 窗口含空 → `None`（故因子有效起点为 i=N）
/// - `Pavg == 0` / `Vavg == 0` → `None`
/// - `Hn == Ln`（窗口价格恒定）→ `None`
fn compute_factor_vc(
    price: &[Option<f64>],
    volume: &[Option<f64>],
    n: usize,
) -> Vec<Option<f64>> {
    let len = price.len();
    if n == 0 || len == 0 {
        return vec![None; len];
    }

    let ret = compute_daily_return(price);
    let cma = compute_rolling_mean(price, n);
    let vma = compute_rolling_mean(volume, n);
    let hn = compute_rolling_max(price, n);
    let ln = compute_rolling_min(price, n);

    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        // 前 n-1 行窗口不足 → None
        if i + 1 < n {
            result.push(None);
            continue;
        }

        // ===== S: 加权量能 =====
        // vma[i] 非空 ⟺ 窗口 [i-n+1, i] 内 volume 全有效（空值传播）
        let vma_v = match vma[i] {
            Some(v) if v != 0.0 => v,
            _ => {
                result.push(None);
                continue;
            }
        };
        // 遍历窗口计算 S；同时校验 ret 窗口全有效（ret[0]=None 是主要触发点）
        let mut s = 0.0f64;
        let mut s_valid = true;
        for j in (i + 1 - n)..=i {
            match ret[j] {
                Some(r) => {
                    // vma[i].is_some() 已保证 volume 窗口全有效，此处 unwrap 安全
                    let v = volume[j].unwrap();
                    let lv = (1.0 + v / vma_v).ln(); // log1p(v / vma_)
                    if r > 0.0 {
                        s += lv;
                    } else if r < 0.0 {
                        s -= lv;
                    }
                    // r == 0：不加不减
                }
                None => {
                    s_valid = false;
                    break;
                }
            }
        }
        if !s_valid {
            result.push(None);
            continue;
        }
        let s_val = s / n as f64;

        // ===== M: 价格趋势项 =====
        let (pt, cma_v) = match (price[i], cma[i]) {
            (Some(p), Some(c)) if c != 0.0 => (p, c),
            _ => {
                result.push(None);
                continue;
            }
        };
        let m = (pt - cma_v) / cma_v;

        // ===== W: 位置惩罚 =====
        let (hn_v, ln_v) = match (hn[i], ln[i]) {
            (Some(h), Some(l)) => (h, l),
            _ => {
                result.push(None);
                continue;
            }
        };
        if hn_v == ln_v {
            // 窗口内价格恒定，pos 分母为 0 → None
            result.push(None);
            continue;
        }
        let pos = (pt - ln_v) / (hn_v - ln_v);
        let w = 1.0 - pos.powf(1.5);

        // ===== factor_vc = M + W × S =====
        result.push(Some(m + w * s_val));
    }

    result
}

/// 对单个 DataFrame 就地写入量价能量因子列。
///
/// - price_column / volume_column 需为 Float64 或 Int64（Int64 提升为 f64）。
/// - 任一源列不存在或非数值类型 → 跳过并告警，DataFrame 原样保留。
/// - `result_col == price_column` 或 `result_col == volume_column` → 就地覆盖对应列；
///   否则覆盖同名列或新增列（源列保留）。
fn apply_factor_vc(
    df: &mut DataFrame,
    price_column: &str,
    volume_column: &str,
    n: usize,
    result_col: &str,
) {
    let price_values = match extract_f64_column(df, price_column) {
        Some(v) => v,
        None => {
            eprintln!(
                "量价能量因子算子: 价格列 '{}' 不存在或类型不支持 (现有列: {:?})，跳过",
                price_column,
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            return;
        }
    };

    let volume_values = match extract_f64_column(df, volume_column) {
        Some(v) => v,
        None => {
            eprintln!(
                "量价能量因子算子: 成交量列 '{}' 不存在或类型不支持 (现有列: {:?})，跳过",
                volume_column,
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            return;
        }
    };

    let out = compute_factor_vc(&price_values, &volume_values, n);
    let new_col = DataFrame::new_float64_column(result_col, out);

    // 写入结果列：同名就地覆盖 price_column / volume_column；否则覆盖已有列或新增列
    if result_col == price_column || result_col == volume_column {
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

/// 量价能量因子算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立计算 factor_vc，
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
        eprintln!("量价能量因子算子: {}", err_msg);
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
            eprintln!("量价能量因子算子: {}", err);
            return -6;
        }
    };

    let price_column = resolve_column(&params.price_column, "close");
    let volume_column = resolve_column(&params.volume_column, "volume");

    // 结果列名：空串自动取 factor_vc_{n}
    let result_col = {
        let t = params.result_column.trim();
        if t.is_empty() {
            format!("factor_vc_{}", n)
        } else {
            t.to_string()
        }
    };

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("量价能量因子算子: {}", err);
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
            eprintln!("量价能量因子算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("量价能量因子算子: {}", err);
        return -5;
    }

    println!(
        "量价能量因子算子: n={}, price='{}', volume='{}', result='{}', 输入 DataFrame 数量={}, 首个行数={}",
        n, price_column, volume_column, result_col, input_dfs.len(), input_dfs[0].row_count
    );

    // 逐个 DataFrame 计算因子（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("量价能量因子算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_factor_vc(df, &price_column, &volume_column, n, &result_col);
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

/// 获取量价能量因子算子版本
#[no_mangle]
pub extern "C" fn factor_vc_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
