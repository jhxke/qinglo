use daily_return_operator::{execute_operator, release_port_data};
use operator_runtime::c_abi::{
    portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, PortData};
use std::ffi::CString;
use std::ptr;

/// 构造一个含 close 列的股票 DataFrame（含上涨/下跌/持平/除零多种情形）
fn build_stock_df(n: usize) -> DataFrame {
    let mut df = DataFrame::new();
    // 价格序列：10, 11, 12, 11, 0, 12, 13, ... （注入除零与下跌）
    let close: Vec<Option<f64>> = (0..n)
        .map(|i| match i {
            0 => Some(10.0),
            1 => Some(11.0),
            2 => Some(12.0),
            3 => Some(11.0), // 下跌
            4 => Some(0.0),  // 跌到 0
            5 => Some(12.0), // 前值为 0 → 除零保护
            _ => Some(10.0 + (i as f64) * 0.5),
        })
        .collect();
    df.add_column(DataFrame::new_float64_column("close", close));
    df.add_column(DataFrame::new_int64_column("volume", (0..n).map(|i| Some(1000 + i as i64 * 10)).collect()));
    df
}

/// 以 DataFrameArray 输入运行一次当日收益率算子，返回输出 PortData
fn run_daily_return(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
    let input_port = PortData::DataFrameArray(input_dfs);
    // portdata_to_c 内部克隆 DataFrame 到独立 C 句柄，原 PortData 仍持有所有权
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(params_json).unwrap_or_default();

    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];

    let result = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );

    println!("execute_operator 返回: {}", result);

    // 若输入未被消费（提前返回错误），释放输入句柄
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    // 提取输出（portdata_from_c 会消费 output_slots[0] 的句柄）
    if output_slots[0].type_tag != TYPE_NULL {
        Some(unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) })
    } else {
        None
    }
}

fn print_df(df: &DataFrame, limit: usize) {
    let print_n = limit.min(df.row_count);
    for i in 0..print_n {
        let mut vals: Vec<String> = Vec::new();
        for col in &df.columns {
            let val = match col.data_type {
                operator_runtime::DataType::Int64 => format!("{:?}", col.get_i64(i)),
                operator_runtime::DataType::Float64 => format!("{:?}", col.get_f64(i)),
                operator_runtime::DataType::String => format!("{:?}", col.get_string(i)),
                operator_runtime::DataType::Bool => format!("{:?}", col.get_bool(i)),
                _ => "???".to_string(),
            };
            vals.push(val);
        }
        println!("  行{}: {:?}", i, vals);
    }
    if df.row_count > print_n {
        println!("  ... 还有 {} 行", df.row_count - print_n);
    }
}

fn print_output(label: &str, pd: &PortData, rows_per_df: usize) {
    match pd {
        PortData::DataFrameArray(dfs) => {
            println!("=== {} 输出 DataFrameArray ({} 个 DataFrame) ===", label, dfs.len());
            for (i, df) in dfs.iter().enumerate() {
                println!(
                    "  [DataFrame {}] 行数={}, 列数={}, 列名={:?}",
                    i,
                    df.row_count,
                    df.col_count(),
                    df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                );
                print_df(df, rows_per_df);
            }
        }
        _ => println!("=== {} 输出类型: {} ===", label, pd.type_name()),
    }
}

fn main() {
    let df_a = build_stock_df(10);
    let df_b = build_stock_df(6);
    println!(
        "=== 输入 DataFrameArray (2 个 DataFrame) ===\n  A: 行数={}, 列名={:?}\n  B: 行数={}, 列名={:?}",
        df_a.row_count,
        df_a.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        df_b.row_count,
        df_b.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
    );

    // 1. 默认参数：source=close, result=daily_return
    println!("\n########## 测试默认参数 ##########");
    let default_json = r#"{}"#;
    if let Some(pd) = run_daily_return(vec![df_a.clone(), df_b.clone()], default_json) {
        print_output("默认", &pd, 10);
    }

    // 2. 自定义列名
    println!("\n########## 测试自定义列名 ##########");
    let custom_json = r#"{"source_column":"close","result_column":"ret_1d"}"#;
    if let Some(pd) = run_daily_return(vec![df_a.clone()], custom_json) {
        print_output("自定义", &pd, 10);
    }

    // 3. 就地覆盖源列（result == source）
    println!("\n########## 测试就地覆盖源列 ##########");
    let overwrite_json = r#"{"source_column":"close","result_column":"close"}"#;
    if let Some(pd) = run_daily_return(vec![df_b.clone()], overwrite_json) {
        print_output("覆盖源列", &pd, 10);
    }

    // 4. 兼容性：单个 DataFrame 输入
    println!("\n########## 兼容性：单个 DataFrame 输入 ##########");
    let input_port = PortData::DataFrame(df_b.clone());
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(r#"{}"#).unwrap();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let result = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    println!("execute_operator 返回: {}", result);
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    if output_slots[0].type_tag != TYPE_NULL {
        let pd = unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) };
        print_output("单DF", &pd, 10);
    }

    // 5. 错误情形：空 DataFrameArray → -5
    println!("\n########## 错误情形：空 DataFrameArray ##########");
    let (code, _pd) = (|| {
        let empty_port = PortData::DataFrameArray(vec![]);
        let mut c_in = [portdata_to_c(&empty_port)];
        let p = CString::new(r#"{}"#).unwrap();
        let mut out: [CPortData; 2] = [CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: ptr::null_mut() },
        }; 2];
        let r = execute_operator(c_in.as_ptr(), c_in.len(), out.as_mut_ptr(), out.len(), p.as_ptr());
        if c_in[0].type_tag != TYPE_NULL {
            release_port_data(&mut c_in[0] as *mut CPortData);
        }
        (r, ())
    })();
    println!("空数组返回码: {} (期望 -5)", code);
}
