use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_from_c,
    c_set_last_error, TYPE_NULL,
};
use std::ffi::{CStr, CString, c_char};
use serde::{Deserialize, Serialize};

/// 折线可视化算子参数（全部 String，与前端字符串输入一致）。
///
/// `indices` 为 0 基逗号分隔下标，空表示选取全部 DataFrame；
/// `date_col`/`close_col` 指定横纵轴列名，空字符串回退默认值；
/// `title_col` 可选，用作每个折线图标题的列名（取该列首行值）。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct LineChartParams {
    /// 选取 DataFrameArray 的 0 基下标，逗号分隔，如 "1,2"；空 = 全选
    #[serde(default)]
    pub indices: String,
    /// 日期列名（默认 "date"）
    #[serde(default)]
    pub date_col: String,
    /// 收盘价列名（默认 "close"）
    #[serde(default)]
    pub close_col: String,
    /// 折线图标题列名（可选，取该列首行值；空则由前端用"图表 N"）
    #[serde(default)]
    pub title_col: String,
}

impl LineChartParams {
    /// 用默认值回退空字段，得到完整的列名配置。
    fn with_defaults(&self) -> LineChartParams {
        LineChartParams {
            indices: self.indices.clone(),
            date_col: if self.date_col.is_empty() { "date".to_string() } else { self.date_col.clone() },
            close_col: if self.close_col.is_empty() { "close".to_string() } else { self.close_col.clone() },
            title_col: self.title_col.clone(),
        }
    }
}

/// 解析参数 JSON 为 LineChartParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> LineChartParams {
    if params_json.is_empty() {
        return LineChartParams::default();
    }
    match serde_json::from_str::<LineChartParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("折线算子: 解析参数 JSON 失败: {}", e);
            LineChartParams::default()
        }
    }
}

/// 解析逗号分隔的 0 基下标字符串，如 "1,2" -> [1, 2]；空串 -> 空 Vec（表示全选）。
/// 非法片段会被跳过并告警。
fn parse_indices(s: &str) -> Vec<usize> {
    s.split(',')
        .filter_map(|tok| {
            let t = tok.trim();
            if t.is_empty() {
                None
            } else {
                match t.parse::<usize>() {
                    Ok(i) => Some(i),
                    Err(_) => {
                        eprintln!("折线算子: 忽略非法下标 '{}'", t);
                        None
                    }
                }
            }
        })
        .collect()
}

/// 折线可视化算子的执行函数（C ABI）。
///
/// 支持 DataFrameArray 输入：按 `indices` 选取子数组（空=全选），将选中的 DataFrame
/// **克隆**后以 `PortData::DataFrameArray` 输出（透传供下游使用）。可视化由前端
/// 「折线图预览」按 `date_col`/`close_col` 渲染，算子本身不生成 DSL、不校验列名。
///
/// 返回值:
/// - 0: 成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空 / indices 未选中任何 DataFrame
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

    let params = parse_params(params_json_str).with_defaults();

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

    // 按 indices 选取子集（空 = 全选）
    let indices = parse_indices(&params.indices);
    let selected: Vec<DataFrame> = if indices.is_empty() {
        // 全选：直接克隆所有 DataFrame
        input_dfs.iter().cloned().collect()
    } else {
        indices
            .iter()
            .filter_map(|&i| {
                if i < input_dfs.len() {
                    Some(input_dfs[i].clone())
                } else {
                    eprintln!("折线算子: 下标 {} 越界（共 {} 个 DataFrame），跳过", i, input_dfs.len());
                    None
                }
            })
            .collect()
    };

    if selected.is_empty() {
        let err_msg = format!("indices={} 未选中任何 DataFrame（共 {} 个）", params.indices, input_dfs.len());
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -5;
    }

    println!(
        "折线算子: indices={:?}, 选中 {} 个 DataFrame (共 {}), 列配置 date={}/close={}/title={}",
        if indices.is_empty() { "全部".to_string() } else { params.indices.clone() },
        selected.len(),
        input_dfs.len(),
        params.date_col, params.close_col,
        if params.title_col.is_empty() { "(默认图表N)".to_string() } else { params.title_col.clone() },
    );

    // 输出选中的 DataFrameArray（透传供下游使用）
    let port_data = PortData::DataFrameArray(selected);

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    if !outputs.is_null() && output_cap > 0 {
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

/// 获取折线可视化算子版本
#[no_mangle]
pub extern "C" fn line_chart_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_indices_handles_cases() {
        assert_eq!(parse_indices(""), Vec::<usize>::new());
        assert_eq!(parse_indices("1,2"), vec![1, 2]);
        assert_eq!(parse_indices(" 0 , 2 ,"), vec![0, 2]);
        assert_eq!(parse_indices("x,3"), vec![3]);
    }

    #[test]
    fn parse_params_empty_returns_default() {
        let p = parse_params("");
        assert!(p.indices.is_empty());
        assert!(p.date_col.is_empty());
        assert!(p.close_col.is_empty());
        assert!(p.title_col.is_empty());
    }

    #[test]
    fn parse_params_invalid_json_returns_default() {
        let p = parse_params("{not json");
        assert_eq!(p.indices, "");
    }

    #[test]
    fn with_defaults_fills_empty_fields() {
        let p = LineChartParams::default().with_defaults();
        assert_eq!(p.date_col, "date");
        assert_eq!(p.close_col, "close");
        assert!(p.title_col.is_empty()); // title_col 无默认值
        assert!(p.indices.is_empty());
    }

    #[test]
    fn with_defaults_keeps_custom_fields() {
        let p = LineChartParams {
            indices: "0,1".to_string(),
            date_col: "dt".to_string(),
            close_col: "price".to_string(),
            title_col: "sym".to_string(),
        };
        let d = p.with_defaults();
        assert_eq!(d.indices, "0,1");
        assert_eq!(d.date_col, "dt");
        assert_eq!(d.close_col, "price");
        assert_eq!(d.title_col, "sym");
    }

    /// 构造含 date + close 列的测试 DataFrame。
    fn make_df(n: usize, seed: f64) -> DataFrame {
        let mut df = DataFrame::new();
        let date_strings: Vec<String> = (0..n).map(|i| format!("2024-01-{:02}", i + 1)).collect();
        let dates: Vec<Option<&str>> = date_strings.iter().map(|s| Some(s.as_str())).collect();
        df.add_column(DataFrame::new_string_column("date", dates));
        let close: Vec<Option<f64>> = (0..n).map(|i| Some(seed + i as f64 * 0.5)).collect();
        df.add_column(DataFrame::new_float64_column("close", close));
        df
    }

    #[test]
    fn execute_operator_passes_through_all_when_indices_empty() {
        let dfs = vec![make_df(3, 10.0), make_df(3, 20.0)];
        let input = PortData::DataFrameArray(dfs);
        let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];
        let params_cstr = std::ffi::CString::new("{}").unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];

        let rc = execute_operator(
            c_inputs.as_ptr(),
            c_inputs.len(),
            out.as_mut_ptr(),
            out.len(),
            params_cstr.as_ptr(),
        );

        // 若输入未被消费（提前返回错误），释放输入句柄
        if c_inputs[0].type_tag != TYPE_NULL {
            release_port_data(&mut c_inputs[0] as *mut CPortData);
        }

        assert_eq!(rc, 0);
        assert_ne!(out[0].type_tag, TYPE_NULL);
        let pd = unsafe { portdata_from_c(&mut out[0] as *mut CPortData) };
        match pd {
            PortData::DataFrameArray(result) => {
                assert_eq!(result.len(), 2, "全选应透传 2 个 DataFrame");
                assert_eq!(result[0].row_count, 3);
                assert_eq!(result[1].row_count, 3);
            }
            other => panic!("期望 DataFrameArray，实际 {:?}", other.type_name()),
        }
    }

    #[test]
    fn execute_operator_selects_subset_by_indices() {
        let dfs = vec![make_df(2, 10.0), make_df(2, 20.0), make_df(2, 30.0)];
        let input = PortData::DataFrameArray(dfs);
        let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];
        let params_cstr = std::ffi::CString::new(r#"{"indices":"0,2"}"#).unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];

        let rc = execute_operator(
            c_inputs.as_ptr(),
            c_inputs.len(),
            out.as_mut_ptr(),
            out.len(),
            params_cstr.as_ptr(),
        );

        if c_inputs[0].type_tag != TYPE_NULL {
            release_port_data(&mut c_inputs[0] as *mut CPortData);
        }

        assert_eq!(rc, 0);
        let pd = unsafe { portdata_from_c(&mut out[0] as *mut CPortData) };
        match pd {
            PortData::DataFrameArray(result) => {
                assert_eq!(result.len(), 2, "indices=0,2 应选中 2 个");
                // 通过 close 首行值区分选中的是第 1、3 个
                let first_close = result[0].column("close").unwrap().get_f64(0).unwrap();
                let second_close = result[1].column("close").unwrap().get_f64(0).unwrap();
                assert!((first_close - 10.0).abs() < 1e-9, "第一个应为 seed=10");
                assert!((second_close - 30.0).abs() < 1e-9, "第二个应为 seed=30");
            }
            other => panic!("期望 DataFrameArray，实际 {:?}", other.type_name()),
        }
    }

    #[test]
    fn execute_operator_single_dataframe_wrapped() {
        let df = make_df(3, 10.0);
        let input = PortData::DataFrame(df);
        let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];
        let params_cstr = std::ffi::CString::new("{}").unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];

        let rc = execute_operator(
            c_inputs.as_ptr(),
            c_inputs.len(),
            out.as_mut_ptr(),
            out.len(),
            params_cstr.as_ptr(),
        );

        if c_inputs[0].type_tag != TYPE_NULL {
            release_port_data(&mut c_inputs[0] as *mut CPortData);
        }

        assert_eq!(rc, 0);
        let pd = unsafe { portdata_from_c(&mut out[0] as *mut CPortData) };
        match pd {
            PortData::DataFrameArray(result) => {
                assert_eq!(result.len(), 1, "单个 DataFrame 应包装为单元素数组");
            }
            other => panic!("期望 DataFrameArray，实际 {:?}", other.type_name()),
        }
    }

    #[test]
    fn execute_operator_missing_input_returns_error() {
        let params_cstr = std::ffi::CString::new("{}").unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }; 2];

        let rc = execute_operator(
            std::ptr::null(),
            0,
            out.as_mut_ptr(),
            out.len(),
            params_cstr.as_ptr(),
        );
        assert_eq!(rc, -3);
    }
}
