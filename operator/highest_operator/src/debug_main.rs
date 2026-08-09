use highest_operator::{execute_operator, release_port_data};
use operator_runtime::{DataFrame, PortData};
use operator_runtime::c_abi::{
    portdata_to_c, portdata_from_c, CPortData, CPortValue, TYPE_NULL,
};
use std::ffi::CString;
use std::ptr;

fn build_stock_df(n: usize) -> DataFrame {
    let mut df = DataFrame::new();
    // 构造 high 序列：基准 10 + 递增 + 末尾冲高（模拟突破）
    let high: Vec<Option<f64>> = (0..n)
        .map(|i| Some(10.0 + (i as f64) * 0.5 + (if i >= n - 1 { 5.0 } else { 0.0 })))
        .collect();
    df.add_column(DataFrame::new_float64_column("high", high));
    df
}

fn run_highest(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
    let input_port = PortData::DataFrameArray(input_dfs);
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

    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

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
    let df_b = build_stock_df(25);
    println!(
        "=== 输入 DataFrameArray (2 个 DataFrame) ===\n  A: 行数={}, 列名={:?}\n  B: 行数={}, 列名={:?}",
        df_a.row_count,
        df_a.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        df_b.row_count,
        df_b.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
    );

    // 1. 默认 N=20（df_a 只有 10 行，全空；df_b 前 19 行空，最后 6 行有效）
    println!("\n########## 测试默认 N=20 ##########");
    let default_json = r#"{}"#;
    if let Some(pd) = run_highest(vec![df_a.clone(), df_b.clone()], default_json) {
        print_output("默认", &pd, 5);
    }

    // 2. N=3（两个 DataFrame 都能产出有效值）
    println!("\n########## 测试 N=3 ##########");
    let n3_json = r#"{"n":"3"}"#;
    if let Some(pd) = run_highest(vec![df_a.clone(), df_b.clone()], n3_json) {
        print_output("N=3", &pd, 8);
    }

    // 3. 自定义列名
    println!("\n########## 测试自定义列名 ##########");
    let custom_json = r#"{"n":"3","column_name":"hhv3"}"#;
    if let Some(pd) = run_highest(vec![df_a.clone()], custom_json) {
        print_output("自定义", &pd, 5);
    }

    // 4. 自定义源列（用 close 计算 N 日收盘最高）
    println!("\n########## 测试自定义源列 source_column=close ##########");
    let mut df_close = DataFrame::new();
    df_close.add_column(DataFrame::new_float64_column(
        "close",
        vec![Some(5.0), Some(8.0), Some(6.0), Some(7.0), Some(9.0)],
    ));
    let close_json = r#"{"n":"2","source_column":"close","column_name":"close_max_2"}"#;
    if let Some(pd) = run_highest(vec![df_close], close_json) {
        print_output("close源", &pd, 10);
    }

    // 5. 兼容性：单个 DataFrame 输入
    println!("\n########## 兼容性：单个 DataFrame 输入 ##########");
    if let Some(pd) = run_highest(vec![df_b.clone()], n3_json) {
        print_output("单DF", &pd, 5);
    }
}
