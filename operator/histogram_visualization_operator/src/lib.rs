use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, PortData};
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CStr, CString};

/// 直方图展示算子参数（全部 String，与前端字符串输入一致）。
///
/// 前端「直方图预览」按列名配置读取 DataFrame 中对应列绘制柱状图。
/// 算子本身只做输入透传，不校验列。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct HistogramVizParams {
    /// X 轴（分箱中心）列名，默认 `bin_center`
    #[serde(default)]
    pub x_col: String,
    /// Y 轴（计数）列名，默认 `count`；也可填 `frequency` 显示频率
    #[serde(default)]
    pub y_col: String,
    /// 可选，每箱左右边界列名（前端渲染 tooltip 用），默认 `bin_left`/`bin_right`
    #[serde(default)]
    pub left_col: String,
    #[serde(default)]
    pub right_col: String,
    /// 直方图标题（可选，空则由前端用默认标题）
    #[serde(default)]
    pub title: String,
}

impl HistogramVizParams {
    fn with_defaults(&self) -> HistogramVizParams {
        HistogramVizParams {
            x_col: if self.x_col.is_empty() { "bin_center".to_string() } else { self.x_col.clone() },
            y_col: if self.y_col.is_empty() { "count".to_string() } else { self.y_col.clone() },
            left_col: if self.left_col.is_empty() { "bin_left".to_string() } else { self.left_col.clone() },
            right_col: if self.right_col.is_empty() { "bin_right".to_string() } else { self.right_col.clone() },
            title: self.title.clone(),
        }
    }
}

fn parse_params(params_json: &str) -> HistogramVizParams {
    if params_json.is_empty() {
        return HistogramVizParams::default();
    }
    match serde_json::from_str::<HistogramVizParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("直方图展示算子: 解析参数 JSON 失败: {}", e);
            HistogramVizParams::default()
        }
    }
}

/// 直方图展示算子的执行函数（C ABI）。
///
/// 输入可以是 DataFrame 或 DataFrameArray：
///   - DataFrame → 原样透传输出
///   - DataFrameArray → 取第 0 个 DataFrame（直方图通常为单表），或为空时全部透传
///
/// 输出为 DataFrame（单表），前端「直方图预览」右键菜单按 x_col/y_col/left_col/right_col
/// 渲染为柱状图，含 tooltip、坐标轴、缩放。
///
/// 返回值:
/// - 0: 成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入空（DataFrame 行数 0 或 DataFrameArray 长度 0）
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
        eprintln!("直方图展示算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str).with_defaults();

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("直方图展示算子: {}", err);
        return -3;
    }

    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let output_df: DataFrame = match input_pd {
        PortData::DataFrame(df) => {
            if df.row_count == 0 {
                let err = "输入 DataFrame 为空 (0 行)".to_string();
                let c_msg = CString::new(err.clone()).unwrap_or_default();
                c_set_last_error(c_msg.as_ptr());
                eprintln!("直方图展示算子: {}", err);
                return -5;
            }
            df
        }
        PortData::DataFrameArray(dfs) => {
            if dfs.is_empty() {
                let err = "输入 DataFrameArray 为空".to_string();
                let c_msg = CString::new(err.clone()).unwrap_or_default();
                c_set_last_error(c_msg.as_ptr());
                eprintln!("直方图展示算子: {}", err);
                return -5;
            }
            // DataFrameArray 输入：取第 0 个（收益率直方图算子输出为单 DataFrame，
            // 但兼容起见支持数组，取第一张表用于展示）
            dfs.into_iter().next().unwrap()
        }
        _ => {
            let err = "输入不是 DataFrame / DataFrameArray 类型".to_string();
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("直方图展示算子: {}", err);
            return -4;
        }
    };

    println!(
        "直方图展示算子: x_col='{}', y_col='{}', left_col='{}', right_col='{}', title='{}', DataFrame 行数={}, 列={:?}",
        params.x_col, params.y_col, params.left_col, params.right_col,
        if params.title.is_empty() { "(默认)".to_string() } else { params.title.clone() },
        output_df.row_count,
        output_df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
    );

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出 DataFrame（透传，前端按列名渲染）
    let port_data = PortData::DataFrame(output_df);
    if !outputs.is_null() && output_cap > 0 {
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

#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

#[no_mangle]
pub extern "C" fn histogram_visualization_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_runtime::DataFrame;

    fn make_histogram_df(n: usize) -> DataFrame {
        let mut df = DataFrame::new();
        let idx: Vec<Option<i64>> = (0..n as i64).map(Some).collect();
        let left: Vec<Option<f64>> = (0..n).map(|i| Some(i as f64 * 0.1 - 0.3)).collect();
        let right: Vec<Option<f64>> = (0..n).map(|i| Some((i as f64 + 1.0) * 0.1 - 0.3)).collect();
        let center: Vec<Option<f64>> = left.iter().zip(right.iter()).map(|(l, r)| Some((l.unwrap() + r.unwrap()) / 2.0)).collect();
        let count: Vec<Option<i64>> = (0..n).map(|i| Some((i * 3 + 1) as i64)).collect();
        let freq: Vec<Option<f64>> = {
            let total: i64 = count.iter().map(|v| v.unwrap_or(0)).sum();
            count.iter().map(|v| Some(v.unwrap_or(0) as f64 / total as f64)).collect()
        };
        df.add_column(DataFrame::new_int64_column("bin_index", idx));
        df.add_column(DataFrame::new_float64_column("bin_left", left));
        df.add_column(DataFrame::new_float64_column("bin_right", right));
        df.add_column(DataFrame::new_float64_column("bin_center", center));
        df.add_column(DataFrame::new_int64_column("count", count));
        df.add_column(DataFrame::new_float64_column("frequency", freq));
        df
    }

    #[test]
    fn with_defaults_fills_empty_fields() {
        let p = HistogramVizParams::default().with_defaults();
        assert_eq!(p.x_col, "bin_center");
        assert_eq!(p.y_col, "count");
        assert_eq!(p.left_col, "bin_left");
        assert_eq!(p.right_col, "bin_right");
        assert!(p.title.is_empty());
    }

    #[test]
    fn with_defaults_keeps_custom_fields() {
        let p = HistogramVizParams {
            x_col: "center".to_string(),
            y_col: "frequency".to_string(),
            left_col: "l".to_string(),
            right_col: "r".to_string(),
            title: "收益率分布".to_string(),
        };
        let d = p.with_defaults();
        assert_eq!(d.x_col, "center");
        assert_eq!(d.y_col, "frequency");
        assert_eq!(d.title, "收益率分布");
    }

    #[test]
    fn execute_passes_through_dataframe() {
        let df = make_histogram_df(5);
        let input = PortData::DataFrame(df);
        let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];
        let params_cstr = CString::new("{}").unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];

        let rc = execute_operator(
            c_inputs.as_ptr(), c_inputs.len(),
            out.as_mut_ptr(), out.len(),
            params_cstr.as_ptr(),
        );
        if c_inputs[0].type_tag != TYPE_NULL {
            release_port_data(&mut c_inputs[0] as *mut CPortData);
        }
        assert_eq!(rc, 0);
        let pd = unsafe { portdata_from_c(&mut out[0] as *mut CPortData) };
        match pd {
            PortData::DataFrame(result) => {
                assert_eq!(result.row_count, 5);
                assert_eq!(result.col_count(), 6);
                assert!(result.column("count").is_some());
            }
            other => panic!("期望 DataFrame，实际 {:?}", other.type_name()),
        }
    }

    #[test]
    fn execute_dataframe_array_takes_first() {
        let df1 = make_histogram_df(3);
        let df2 = make_histogram_df(4);
        let input = PortData::DataFrameArray(vec![df1, df2]);
        let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];
        let params_cstr = CString::new("{}").unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];

        let rc = execute_operator(
            c_inputs.as_ptr(), c_inputs.len(),
            out.as_mut_ptr(), out.len(),
            params_cstr.as_ptr(),
        );
        if c_inputs[0].type_tag != TYPE_NULL {
            release_port_data(&mut c_inputs[0] as *mut CPortData);
        }
        assert_eq!(rc, 0);
        let pd = unsafe { portdata_from_c(&mut out[0] as *mut CPortData) };
        match pd {
            PortData::DataFrame(result) => {
                // 取第一张，行数=3
                assert_eq!(result.row_count, 3);
            }
            other => panic!("期望 DataFrame，实际 {:?}", other.type_name()),
        }
    }

    #[test]
    fn execute_missing_input_error() {
        let params_cstr = CString::new("{}").unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];
        let rc = execute_operator(
            std::ptr::null(), 0,
            out.as_mut_ptr(), out.len(),
            params_cstr.as_ptr(),
        );
        assert_eq!(rc, -3);
    }

    #[test]
    fn execute_empty_df_error() {
        let df = DataFrame::new();
        let input = PortData::DataFrame(df);
        let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];
        let params_cstr = CString::new("{}").unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];
        let rc = execute_operator(
            c_inputs.as_ptr(), c_inputs.len(),
            out.as_mut_ptr(), out.len(),
            params_cstr.as_ptr(),
        );
        if c_inputs[0].type_tag != TYPE_NULL {
            release_port_data(&mut c_inputs[0] as *mut CPortData);
        }
        assert_eq!(rc, -5);
    }
}
