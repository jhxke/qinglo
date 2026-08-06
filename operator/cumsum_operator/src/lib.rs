use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CStr, CString};

/// 累加算子参数结构体
///
/// - `columns`：需要做行向累加（cumsum）的**源列名**，仅支持单列，
///   不支持逗号分隔多列。为空时报错（-6）；含逗号时报错（-7）。
/// - `result_column`：累加结果写入的列名。
///   - 为空或与 `columns` 相同：就地覆盖源列（保留原列名）。
///   - 与 `columns` 不同：写入名为 `result_column` 的列——若该列已存在则覆盖，
///     否则新增列；**源列保持不变**。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CumSumParams {
    #[serde(default)]
    pub columns: String,
    #[serde(default)]
    pub result_column: String,
}

/// 解析参数 JSON 为 CumSumParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> CumSumParams {
    if params_json.is_empty() {
        return CumSumParams::default();
    }
    match serde_json::from_str::<CumSumParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("累加算子: 解析参数 JSON 失败: {}", e);
            CumSumParams::default()
        }
    }
}

/// 对单个 DataFrame 做指定源列的行向累加（cumulative sum），把结果写入 `result_col`。
///
/// 对源列：第 i 行替换为第 0..=i 行之和，即
///   `out[0] = v[0]`，`out[1] = v[0] + v[1]`，`out[2] = v[0] + v[1] + v[2]`，...
///
/// - **Float64 / Int64 列**：按原类型累加。
///   Int64 使用 `saturating_add` 累加，避免超长序列在 debug 构建下溢出 panic。
/// - **空值处理**：与 pandas `cumsum(skipna=True)` 一致——空值位置输出空值，
///   累加器不重置（跳过空值继续累加）。
/// - **结果列写入**：
///   - `result_col == source`：就地覆盖源列。
///   - 否则：若已存在名为 `result_col` 的列则覆盖之；否则新增列（**源列保留**）。
/// - **源列不存在**：跳过并告警。
/// - **源列类型非数值**（String/Bool/Null）：跳过并告警。
fn apply_cumsum(df: &mut DataFrame, source: &str, result_col: &str) {
    // 先定位源列索引与类型，结束借用后再写入，避免借用冲突
    let (src_pos, data_type) = match df.columns.iter().position(|c| c.name == source) {
        Some(p) => (p, df.columns[p].data_type.clone()),
        None => {
            eprintln!(
                "累加算子: 源列 '{}' 不存在，跳过 (现有列: {:?})",
                source,
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            return;
        }
    };

    let new_col = match data_type {
        DataType::Float64 => {
            let vals = df.columns[src_pos].to_f64_vec();
            let mut sum = 0.0f64;
            let out: Vec<Option<f64>> = vals
                .into_iter()
                .map(|v| match v {
                    Some(x) => {
                        sum += x;
                        Some(sum)
                    }
                    None => None,
                })
                .collect();
            DataFrame::new_float64_column(result_col, out)
        }
        DataType::Int64 => {
            let vals = df.columns[src_pos].to_i64_vec();
            let mut sum = 0i64;
            let out: Vec<Option<i64>> = vals
                .into_iter()
                .map(|v| match v {
                    Some(x) => {
                        sum = sum.saturating_add(x);
                        Some(sum)
                    }
                    None => None,
                })
                .collect();
            DataFrame::new_int64_column(result_col, out)
        }
        other => {
            eprintln!(
                "累加算子: 源列 '{}' 类型 {:?} 不支持累加 (仅支持 Float64/Int64)，跳过",
                source, other
            );
            return;
        }
    };

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

/// 累加算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立做行向累加，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 单个 DataFrame 输入会被包装为单元素数组处理，输出仍为 DataFrameArray。
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
/// - -6: 未指定源列（参数 columns 为空）
/// - -7: columns 含逗号（不支持多列，仅支持单列累加）
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
        eprintln!("累加算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    // 源列：仅支持单列，去首尾空白
    let source = params.columns.trim().to_string();
    if source.is_empty() {
        let err = "未指定需要累加的源列 (参数 columns 为空)".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("累加算子: {}", err);
        return -6;
    }
    if source.contains(',') {
        let err = format!(
            "不支持多列累加 (参数 columns='{}' 含逗号)；仅支持单列，请去掉逗号",
            source
        );
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("累加算子: {}", err);
        return -7;
    }

    // 结果列名：为空或与源列同名 → 就地覆盖源列；否则写入指定列（保留源列）
    let result_col = {
        let t = params.result_column.trim();
        if t.is_empty() {
            source.clone()
        } else {
            t.to_string()
        }
    };

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("累加算子: {}", err);
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
            eprintln!("累加算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("累加算子: {}", err);
        return -5;
    }

    println!(
        "累加算子: 源列='{}', 结果列='{}', 输入 DataFrame 数量={}, 首个行数={}",
        source, result_col, input_dfs.len(), input_dfs[0].row_count
    );

    // 逐个 DataFrame 做行向累加（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("累加算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_cumsum(df, &source, &result_col);
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

/// 获取累加算子版本
#[no_mangle]
pub extern "C" fn cumsum_operator_version() -> *const c_char {
    b"0.2.0\0".as_ptr() as *const c_char
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

    #[test]
    fn cumsum_float64_basic_inplace() {
        // result_col == source：就地覆盖
        let mut df = df_f64("v", vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);
        apply_cumsum(&mut df, "v", "v");
        let col = df.column("v").unwrap();
        assert_eq!(col.to_f64_vec(), vec![Some(1.0), Some(3.0), Some(6.0), Some(10.0)]);
    }

    #[test]
    fn cumsum_float64_skips_null_like_pandas() {
        // [1, None, 2] -> [1, None, 3] (空值跳过，累加器不重置)
        let mut df = df_f64("v", vec![Some(1.0), None, Some(2.0)]);
        apply_cumsum(&mut df, "v", "v");
        let col = df.column("v").unwrap();
        assert_eq!(col.to_f64_vec(), vec![Some(1.0), None, Some(3.0)]);
    }

    #[test]
    fn cumsum_int64_preserves_type_inplace() {
        let mut df = df_i64("vol", vec![Some(10), Some(20), Some(30)]);
        apply_cumsum(&mut df, "vol", "vol");
        let col = df.column("vol").unwrap();
        assert_eq!(col.data_type, DataType::Int64);
        assert_eq!(col.to_i64_vec(), vec![Some(10), Some(30), Some(60)]);
    }

    #[test]
    fn cumsum_int64_with_null_inplace() {
        let mut df = df_i64("vol", vec![Some(5), None, Some(5)]);
        apply_cumsum(&mut df, "vol", "vol");
        let col = df.column("vol").unwrap();
        assert_eq!(col.to_i64_vec(), vec![Some(5), None, Some(10)]);
    }

    #[test]
    fn cumsum_creates_new_result_column_preserving_source() {
        // result_col != source：新建列，源列保持不变
        let mut df = df_f64("v", vec![Some(1.0), Some(2.0), Some(3.0)]);
        apply_cumsum(&mut df, "v", "cum_v");
        // 源列保持原值
        assert_eq!(df.column("v").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0), Some(3.0)]);
        // 新列持有累加值
        assert_eq!(df.column("cum_v").unwrap().to_f64_vec(), vec![Some(1.0), Some(3.0), Some(6.0)]);
        assert_eq!(df.col_count(), 2);
    }

    #[test]
    fn cumsum_overwrites_existing_result_column() {
        // result_col 指向已存在的另一列：覆盖该列，源列保留
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("v", vec![Some(1.0), Some(2.0), Some(3.0)]));
        df.add_column(DataFrame::new_float64_column("w", vec![Some(100.0), Some(100.0), Some(100.0)]));
        apply_cumsum(&mut df, "v", "w");
        assert_eq!(df.column("v").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0), Some(3.0)]);
        assert_eq!(df.column("w").unwrap().to_f64_vec(), vec![Some(1.0), Some(3.0), Some(6.0)]);
        assert_eq!(df.col_count(), 2);
    }

    #[test]
    fn cumsum_int64_new_result_column_preserves_type() {
        let mut df = df_i64("vol", vec![Some(10), Some(20), Some(30)]);
        apply_cumsum(&mut df, "vol", "cum_vol");
        let new = df.column("cum_vol").unwrap();
        assert_eq!(new.data_type, DataType::Int64);
        assert_eq!(new.to_i64_vec(), vec![Some(10), Some(30), Some(60)]);
        // 源列保持原值
        assert_eq!(df.column("vol").unwrap().to_i64_vec(), vec![Some(10), Some(20), Some(30)]);
    }

    #[test]
    fn cumsum_skips_missing_source_column() {
        let mut df = df_f64("v", vec![Some(1.0), Some(2.0)]);
        // 指定不存在的源列：不应 panic，原列不变，也不新增结果列
        apply_cumsum(&mut df, "missing", "cum_missing");
        assert_eq!(df.column("v").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0)]);
        assert!(df.column("cum_missing").is_none());
        assert_eq!(df.col_count(), 1);
    }

    #[test]
    fn cumsum_skips_non_numeric_source_column() {
        let col = DataFrame::new_string_column("s", vec![Some("a"), Some("b")]);
        let mut df = DataFrame::new();
        df.add_column(col);
        // 字符串列不支持累加：跳过，原值不变，不新增结果列
        apply_cumsum(&mut df, "s", "cum_s");
        let s = df.column("s").unwrap();
        assert_eq!(s.get_string(0), Some("a"));
        assert_eq!(s.get_string(1), Some("b"));
        assert!(df.column("cum_s").is_none());
    }
}
