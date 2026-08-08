use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CStr, CString};

/// 未来收益算子参数结构体
///
/// - `n`：未来天数（字符串形式，与前端字符串输入一致）。空串回退默认 5；
///   必须能解析为正整数，否则报错（-6）。一般取 5 天以上。
/// - `result_column`：结果列名。为空时自动取 `future_return_{n}`；
///   与源列同名则就地覆盖源列，否则新增列（已存在则覆盖，源列保留）。
/// - `source_column`：计算所用的价格源列名，空串回退默认 `close`。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct FutureReturnParams {
    #[serde(default)]
    pub n: String,
    #[serde(default)]
    pub result_column: String,
    #[serde(default)]
    pub source_column: String,
}

/// 解析参数 JSON 为 FutureReturnParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> FutureReturnParams {
    if params_json.is_empty() {
        return FutureReturnParams::default();
    }
    match serde_json::from_str::<FutureReturnParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("未来收益算子: 解析参数 JSON 失败: {}", e);
            FutureReturnParams::default()
        }
    }
}

/// 解析 n（未来天数）：空串回退默认 5；必须为正整数，否则返回 None（调用方报错 -6）
fn parse_n(raw: &str) -> Option<usize> {
    let t = raw.trim();
    if t.is_empty() {
        return Some(5);
    }
    match t.parse::<usize>() {
        Ok(v) if v >= 1 => Some(v),
        _ => None,
    }
}

/// 解析源列名；空串或纯空格回退默认 "close"
fn resolve_source_column(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        "close".to_string()
    } else {
        t.to_string()
    }
}

/// 计算未来 n 天收益率：
///   `out[i] = (values[i + n] - values[i]) / values[i]`
///
/// - 末尾 n 行（`i + n >= len`）无未来价格 → `None`。
/// - 当前价或未来价为空 → `None`（空值传播）。
/// - 当前价为 0 → `None`（避免除零）。
/// - `n == 0` 时退化为恒等 0 收益（但参数校验已禁止 n=0，此处仅作防御性处理）。
fn compute_future_return(values: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    let len = values.len();
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        let j = i + n;
        if n == 0 {
            // n=0 防御：未来 0 天收益恒为 0（仅对有效价格）
            result.push(values[i].map(|_| 0.0));
            continue;
        }
        if j >= len {
            // 末尾 n 行：无未来价格
            result.push(None);
            continue;
        }
        match (values[i], values[j]) {
            (Some(cur), Some(fut)) => {
                if cur == 0.0 {
                    result.push(None); // 避免除零
                } else {
                    result.push(Some((fut - cur) / cur));
                }
            }
            _ => result.push(None), // 当前价或未来价为空
        }
    }
    result
}

/// 对单个 DataFrame 就地写入未来收益列。
///
/// - 源列需为 Float64 或 Int64（Int64 提升为 f64 计算，收益率恒为 Float64）。
/// - 源列不存在或非数值类型 → 跳过并告警，DataFrame 原样保留。
/// - `result_col == source` → 就地覆盖源列；否则覆盖同名列或新增列（源列保留）。
fn apply_future_return(df: &mut DataFrame, source: &str, n: usize, result_col: &str) {
    // 先定位源列索引与类型，结束借用后再写入，避免借用冲突
    let (src_pos, data_type) = match df.columns.iter().position(|c| c.name == source) {
        Some(p) => (p, df.columns[p].data_type.clone()),
        None => {
            eprintln!(
                "未来收益算子: 源列 '{}' 不存在，跳过 (现有列: {:?})",
                source,
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            return;
        }
    };

    // 提取 f64 序列：Float64 直接取，Int64 提升为 f64（收益率必为小数）
    let values: Vec<Option<f64>> = match data_type {
        DataType::Float64 => df.columns[src_pos].to_f64_vec(),
        DataType::Int64 => df
            .columns[src_pos]
            .to_i64_vec()
            .into_iter()
            .map(|v| v.map(|x| x as f64))
            .collect(),
        other => {
            eprintln!(
                "未来收益算子: 源列 '{}' 类型 {:?} 不支持 (仅 Float64/Int64)，跳过",
                source, other
            );
            return;
        }
    };

    let out = compute_future_return(&values, n);
    let new_col = DataFrame::new_float64_column(result_col, out);

    // 写入结果列：同名就地覆盖；异名则覆盖已有列或新增列（保留源列）
    if result_col == source {
        df.columns[src_pos] = new_col;
    } else {
        match df.columns.iter().position(|c| c.name == result_col) {
            Some(p) => df.columns[p] = new_col,
            None => df.add_column(new_col),
        }
    }
}

/// 未来收益算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立计算未来 n 天收益率，
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
        eprintln!("未来收益算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    // n：空串回退 5；非正整数报错
    let n = match parse_n(&params.n) {
        Some(v) => v,
        None => {
            let err = format!(
                "参数 n='{}' 非法 (需为正整数，如 5)；空串将回退默认 5",
                params.n
            );
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("未来收益算子: {}", err);
            return -6;
        }
    };

    // 源列名：空串回退 "close"
    let source = resolve_source_column(&params.source_column);

    // 结果列名：空串自动取 future_return_{n}
    let result_col = {
        let t = params.result_column.trim();
        if t.is_empty() {
            format!("future_return_{}", n)
        } else {
            t.to_string()
        }
    };

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("未来收益算子: {}", err);
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
            eprintln!("未来收益算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("未来收益算子: {}", err);
        return -5;
    }

    println!(
        "未来收益算子: n={}, 源列='{}', 结果列='{}', 输入 DataFrame 数量={}, 首个行数={}",
        n, source, result_col, input_dfs.len(), input_dfs[0].row_count
    );

    // 逐个 DataFrame 计算未来收益（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("未来收益算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_future_return(df, &source, n, &result_col);
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

/// 获取未来收益算子版本
#[no_mangle]
pub extern "C" fn future_return_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造单列 Float64 DataFrame
    fn df_f64(name: &str, vals: Vec<Option<f64>>) -> DataFrame {
        let col = DataFrame::new_float64_column(name, vals);
        let mut df = DataFrame::new();
        df.add_column(col);
        df
    }

    /// 构造单列 Int64 DataFrame
    fn df_i64(name: &str, vals: Vec<Option<i64>>) -> DataFrame {
        let col = DataFrame::new_int64_column(name, vals);
        let mut df = DataFrame::new();
        df.add_column(col);
        df
    }

    fn approx(a: Option<f64>, b: Option<f64>) -> bool {
        match (a, b) {
            (Some(x), Some(y)) => (x - y).abs() < 1e-12,
            (None, None) => true,
            _ => false,
        }
    }

    fn assert_approx_vec(actual: &[Option<f64>], expected: &[Option<f64>]) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "长度不一致: actual={} expected={}",
            actual.len(),
            expected.len()
        );
        for (i, (a, b)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(approx(*a, *b), "第 {} 行不一致: actual={:?} expected={:?}", i, a, b);
        }
    }

    #[test]
    fn future_return_basic_float64() {
        // close = [10,11,12,13,14,15], n=2
        // i=0: (12-10)/10 = 0.2
        // i=1: (13-11)/11 = 0.181818...
        // i=2: (14-12)/12 = 0.166666...
        // i=3: (15-13)/13 = 0.153846...
        // i=4,5: 末尾 2 行无未来价 -> None
        let mut df = df_f64("close", vec![
            Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0), Some(15.0),
        ]);
        apply_future_return(&mut df, "close", 2, "ret_2");
        let col = df.column("ret_2").unwrap();
        assert_eq!(col.data_type, DataType::Float64);
        assert_approx_vec(
            &col.to_f64_vec(),
            &[
                Some(0.2),
                Some((13.0 - 11.0) / 11.0),
                Some((14.0 - 12.0) / 12.0),
                Some((15.0 - 13.0) / 13.0),
                None,
                None,
            ],
        );
        // 源列保持不变
        assert_eq!(
            df.column("close").unwrap().to_f64_vec(),
            vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0), Some(15.0)]
        );
        assert_eq!(df.col_count(), 2);
    }

    #[test]
    fn future_return_n_equals_len_all_null() {
        // n == len：每行 i+n 都越界 -> 全 None
        let mut df = df_f64("close", vec![Some(1.0), Some(2.0), Some(3.0)]);
        apply_future_return(&mut df, "close", 3, "ret_3");
        assert_approx_vec(
            &df.column("ret_3").unwrap().to_f64_vec(),
            &[None, None, None],
        );
    }

    #[test]
    fn future_return_n_greater_than_len_all_null() {
        let mut df = df_f64("close", vec![Some(1.0), Some(2.0)]);
        apply_future_return(&mut df, "close", 5, "ret_5");
        assert_approx_vec(
            &df.column("ret_5").unwrap().to_f64_vec(),
            &[None, None],
        );
    }

    #[test]
    fn future_return_n_one_last_row_null() {
        // n=1: 仅最后一行为空
        // [10,20,40] -> i=0:(20-10)/10=1.0, i=1:(40-20)/20=1.0, i=2:None
        let mut df = df_f64("close", vec![Some(10.0), Some(20.0), Some(40.0)]);
        apply_future_return(&mut df, "close", 1, "ret_1");
        assert_approx_vec(
            &df.column("ret_1").unwrap().to_f64_vec(),
            &[Some(1.0), Some(1.0), None],
        );
    }

    #[test]
    fn future_return_null_current_propagates() {
        // 当前价为空 -> 该行结果为空
        let mut df = df_f64("close", vec![None, Some(20.0), Some(40.0), Some(80.0)]);
        apply_future_return(&mut df, "close", 1, "ret_1");
        // i=0: cur=None -> None
        // i=1: (40-20)/20 = 1.0
        // i=2: (80-40)/40 = 1.0
        // i=3: 末尾 -> None
        assert_approx_vec(
            &df.column("ret_1").unwrap().to_f64_vec(),
            &[None, Some(1.0), Some(1.0), None],
        );
    }

    #[test]
    fn future_return_null_future_propagates() {
        // 未来价为空 -> 该行结果为空
        let mut df = df_f64("close", vec![Some(10.0), None, Some(40.0), Some(80.0)]);
        apply_future_return(&mut df, "close", 1, "ret_1");
        // i=0: fut=close[1]=None -> None
        // i=1: fut=close[2]=40, cur=None -> None
        // i=2: (80-40)/40 = 1.0
        // i=3: 末尾 -> None
        assert_approx_vec(
            &df.column("ret_1").unwrap().to_f64_vec(),
            &[None, None, Some(1.0), None],
        );
    }

    #[test]
    fn future_return_division_by_zero_is_null() {
        // 当前价为 0 -> 避免除零，结果为空
        let mut df = df_f64("close", vec![Some(0.0), Some(10.0), Some(20.0)]);
        apply_future_return(&mut df, "close", 1, "ret_1");
        // i=0: cur=0 -> None
        // i=1: (20-10)/10 = 1.0
        // i=2: 末尾 -> None
        assert_approx_vec(
            &df.column("ret_1").unwrap().to_f64_vec(),
            &[None, Some(1.0), None],
        );
    }

    #[test]
    fn future_return_int64_source_promotes_to_float64() {
        // Int64 源列 -> 结果提升为 Float64
        let mut df = df_i64("close", vec![Some(10), Some(20), Some(40)]);
        apply_future_return(&mut df, "close", 1, "ret_1");
        let col = df.column("ret_1").unwrap();
        assert_eq!(col.data_type, DataType::Float64);
        // i=0:(20-10)/10=1.0, i=1:(40-20)/20=1.0, i=2:None
        assert_approx_vec(
            &col.to_f64_vec(),
            &[Some(1.0), Some(1.0), None],
        );
        // 源列保持 Int64 原值
        assert_eq!(df.column("close").unwrap().data_type, DataType::Int64);
        assert_eq!(df.column("close").unwrap().to_i64_vec(), vec![Some(10), Some(20), Some(40)]);
    }

    #[test]
    fn future_return_overwrite_existing_result_column() {
        // result_col 指向已存在的另一列：覆盖该列，源列保留
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("close", vec![Some(10.0), Some(20.0), Some(40.0)]));
        df.add_column(DataFrame::new_float64_column("w", vec![Some(100.0), Some(100.0), Some(100.0)]));
        apply_future_return(&mut df, "close", 1, "w");
        assert_approx_vec(
            &df.column("close").unwrap().to_f64_vec(),
            &[Some(10.0), Some(20.0), Some(40.0)],
        );
        assert_approx_vec(
            &df.column("w").unwrap().to_f64_vec(),
            &[Some(1.0), Some(1.0), None],
        );
        assert_eq!(df.col_count(), 2);
    }

    #[test]
    fn future_return_result_equals_source_overwrites_source() {
        // result_col == source：就地覆盖源列
        let mut df = df_f64("close", vec![Some(10.0), Some(20.0), Some(40.0)]);
        apply_future_return(&mut df, "close", 1, "close");
        assert_approx_vec(
            &df.column("close").unwrap().to_f64_vec(),
            &[Some(1.0), Some(1.0), None],
        );
        assert_eq!(df.col_count(), 1);
    }

    #[test]
    fn future_return_skips_missing_source_column() {
        // 源列不存在：不 panic，不新增结果列
        let mut df = df_f64("close", vec![Some(1.0), Some(2.0)]);
        apply_future_return(&mut df, "missing", 1, "ret_1");
        assert_eq!(df.column("close").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0)]);
        assert!(df.column("ret_1").is_none());
        assert_eq!(df.col_count(), 1);
    }

    #[test]
    fn future_return_skips_non_numeric_source_column() {
        // 字符串列不支持：跳过，原值不变，不新增结果列
        let col = DataFrame::new_string_column("s", vec![Some("a"), Some("b"), Some("c")]);
        let mut df = DataFrame::new();
        df.add_column(col);
        apply_future_return(&mut df, "s", 1, "ret_1");
        let s = df.column("s").unwrap();
        assert_eq!(s.get_string(0), Some("a"));
        assert_eq!(s.get_string(1), Some("b"));
        assert!(df.column("ret_1").is_none());
        assert_eq!(df.col_count(), 1);
    }

    #[test]
    fn future_return_empty_df_noop() {
        // 空 DataFrame：函数不应 panic，直接返回
        let mut df = DataFrame::new();
        apply_future_return(&mut df, "close", 5, "ret_5");
        assert_eq!(df.row_count, 0);
    }

    #[test]
    fn parse_n_empty_defaults_to_five() {
        assert_eq!(parse_n(""), Some(5));
        assert_eq!(parse_n("   "), Some(5));
    }

    #[test]
    fn parse_n_valid_positive() {
        assert_eq!(parse_n("5"), Some(5));
        assert_eq!(parse_n("  10  "), Some(10));
        assert_eq!(parse_n("1"), Some(1));
    }

    #[test]
    fn parse_n_rejects_invalid() {
        assert_eq!(parse_n("0"), None);
        assert_eq!(parse_n("-1"), None);
        assert_eq!(parse_n("abc"), None);
        assert_eq!(parse_n("3.5"), None);
    }

    #[test]
    fn resolve_source_column_fallback() {
        assert_eq!(resolve_source_column(""), "close");
        assert_eq!(resolve_source_column("   "), "close");
        assert_eq!(resolve_source_column("  price  "), "price");
    }

    #[test]
    fn compute_future_return_core_logic() {
        // 直接测试纯函数：n=2, [10,11,12,13,14,15]
        let v = vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0), Some(15.0)];
        let out = compute_future_return(&v, 2);
        assert_approx_vec(
            &out,
            &[
                Some(0.2),
                Some((13.0 - 11.0) / 11.0),
                Some((14.0 - 12.0) / 12.0),
                Some((15.0 - 13.0) / 13.0),
                None,
                None,
            ],
        );
    }

    #[test]
    fn compute_future_return_n_zero_defensive() {
        // n=0 防御性：有效价 -> 0.0，空价 -> None
        let v = vec![Some(10.0), None, Some(30.0)];
        let out = compute_future_return(&v, 0);
        assert_approx_vec(&out, &[Some(0.0), None, Some(0.0)]);
    }
}
