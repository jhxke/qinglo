use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::PortData;
use std::ffi::{c_char, CString};

/// 合并算子的执行函数（C ABI）
///
/// **设计理念**：不"合并"列，而是依赖 runtime 层的**对象共享**——扇出时多个
/// 分支共享同一个 `DataFrame`（通过 `Rc<RefCell<PortData>>`），各分支算子就地
/// 追加的列会通过执行后回写累积到同一对象上。因此合并算子只需**输出第一个
/// 非空输入**即可拿到包含所有分支新增列的完整 DataFrame。
///
/// 行为：
/// 1. 逐个输入端口查找第一个非 `TYPE_NULL` 且非空的 `DataFrame` / `DataFrameArray`
/// 2. 原样输出该端口数据（不做任何列操作）
/// 3. 所有端口均未连接或为空时返回 `-5`
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 输入端口数组为空 (input_count == 0)
/// - -5: 所有输入端口均未提供有效数据（全空数组/全为 TYPE_NULL）
#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    _params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        let err_msg = format!("{}", e);
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("合并算子: {}", err_msg);
        return -1;
    }

    if input_count == 0 {
        let err = "缺少输入数据 (input_count == 0)".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("合并算子: {}", err);
        return -3;
    }

    // 找到第一个非空且有效的输入端口，原样输出。
    // 依赖 runtime 层 Rc<RefCell<PortData>> 共享 + 回写机制：
    // 各分支算子（MA/RSI/MACD...）就地追加的列已累积到同一对象上，
    // 任意一个输入端口都能拿到包含全部新增列的完整 DataFrame。
    for i in 0..input_count {
        let input_ptr = unsafe { inputs.add(i) };
        if input_ptr.is_null() {
            continue;
        }
        let type_tag = unsafe { (*input_ptr).type_tag };
        if type_tag == TYPE_NULL {
            continue;
        }

        let port_data = unsafe { portdata_from_c(input_ptr as *mut CPortData) };

        // 只接受 DataFrame / DataFrameArray；其他类型跳过
        let is_valid = match &port_data {
            PortData::DataFrame(df) => !df.columns.is_empty(),
            PortData::DataFrameArray(dfs) => !dfs.is_empty(),
            _ => false,
        };
        if !is_valid {
            eprintln!("合并算子: 输入端口 {} 为空或类型不支持，跳过", i);
            continue;
        }

        let col_info = match &port_data {
            PortData::DataFrame(df) => format!("{} 行 × {} 列", df.row_count, df.columns.len()),
            PortData::DataFrameArray(dfs) => {
                let total_cols: usize = dfs.iter().map(|d| d.columns.len()).sum();
                format!("{} 个 DataFrame, 共 {} 列", dfs.len(), total_cols)
            }
            _ => String::new(),
        };
        println!("合并算子: 使用输入端口 {} 的数据 ({})，原样输出", i, col_info);

        // 清空错误信息（成功执行）
        let c_msg = CString::new("").unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());

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

        return 0;
    }

    let err = "所有输入端口均未连接或为空，请至少连接 1 个有效上游输出".to_string();
    let c_msg = CString::new(err.clone()).unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());
    eprintln!("合并算子: {}", err);
    -5
}

/// 释放 C ABI PortData 内存（由调用方调用）
#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

/// 获取合并算子版本
#[no_mangle]
pub extern "C" fn merge_operator_version() -> *const c_char {
    b"0.3.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_runtime::c_abi::{portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL};
    use operator_runtime::DataFrame;
    use std::ffi::CString;

    /// 构造单列 Float64 DataFrame
    fn df_f64(name: &str, vals: Vec<Option<f64>>) -> DataFrame {
        let col = DataFrame::new_float64_column(name, vals);
        let mut df = DataFrame::new();
        df.add_column(col);
        df
    }

    /// 调用 execute_operator 并返回 (返回码, 输出 PortData)
    fn run_merge(c_inputs: Vec<CPortData>) -> (i32, Option<PortData>) {
        let input_count = c_inputs.len();
        let inputs_ptr = c_inputs.as_ptr();
        let mut c_outputs: Vec<CPortData> = vec![
            CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } },
            CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } },
        ];
        let params_json = CString::new("").unwrap();
        let rc = execute_operator(
            inputs_ptr,
            input_count,
            c_outputs.as_mut_ptr(),
            c_outputs.len(),
            params_json.as_ptr(),
        );
        let out = if rc == 0 {
            Some(unsafe { portdata_from_c(&mut c_outputs[0]) })
        } else {
            None
        };
        (rc, out)
    }

    /// 基本场景：第一个非空输入应被原样输出。
    #[test]
    fn outputs_first_non_empty_input() {
        let df = df_f64("close", vec![Some(1.0), Some(2.0), Some(3.0)]);
        let mut c_inputs: Vec<CPortData> = Vec::new();
        c_inputs.push(portdata_to_c_owned(PortData::DataFrameArray(vec![df])));

        let (rc, out) = run_merge(c_inputs);
        assert_eq!(rc, 0);
        match out.unwrap() {
            PortData::DataFrameArray(dfs) => {
                assert_eq!(dfs.len(), 1);
                assert_eq!(dfs[0].columns.len(), 1);
                assert_eq!(dfs[0].columns[0].name, "close");
            }
            other => panic!("期望 DataFrameArray，得到 {:?}", other),
        }
    }

    /// 多个端口时，跳过 TYPE_NULL，输出第一个有效端口。
    #[test]
    fn skips_null_ports_and_outputs_first_valid() {
        let df = df_f64("close", vec![Some(1.0), Some(2.0)]);
        let mut c_inputs: Vec<CPortData> = Vec::new();
        // port0: TYPE_NULL
        c_inputs.push(CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } });
        // port1: 有效数据
        c_inputs.push(portdata_to_c_owned(PortData::DataFrameArray(vec![df])));
        // port2: TYPE_NULL
        c_inputs.push(CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } });

        let (rc, out) = run_merge(c_inputs);
        assert_eq!(rc, 0);
        assert!(matches!(out.unwrap(), PortData::DataFrameArray(_)));
    }

    /// 全部端口未连接时应返回 -5。
    #[test]
    fn returns_error_when_all_ports_null() {
        let c_inputs: Vec<CPortData> = vec![
            CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } },
            CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } },
        ];
        let (rc, _) = run_merge(c_inputs);
        assert_eq!(rc, -5);
    }

    /// 多个有效端口时，输出第一个（不合并）。
    #[test]
    fn outputs_first_when_multiple_valid() {
        let df0 = df_f64("close", vec![Some(1.0)]);
        let df1 = df_f64("ma_5", vec![Some(2.0)]);
        let mut c_inputs: Vec<CPortData> = Vec::new();
        c_inputs.push(portdata_to_c_owned(PortData::DataFrameArray(vec![df0])));
        c_inputs.push(portdata_to_c_owned(PortData::DataFrameArray(vec![df1])));

        let (rc, out) = run_merge(c_inputs);
        assert_eq!(rc, 0);
        match out.unwrap() {
            PortData::DataFrameArray(dfs) => {
                assert_eq!(dfs[0].columns[0].name, "close", "应输出第一个端口的数据");
            }
            other => panic!("期望 DataFrameArray，得到 {:?}", other),
        }
    }
}
