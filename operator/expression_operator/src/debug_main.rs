use expression_operator::{execute_operator, release_port_data};
use operator_runtime::c_abi::{portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, DataType, PortData};
use std::ffi::CString;
use std::ptr;

/// 构造一个含 ma5 / ma10 列的测试 DataFrame
/// ma5 与 ma10 大小交替，使得 ma5 > ma10 在部分行为真、部分行为假
fn build_signal_df(seed: f64, count: usize) -> DataFrame {
    let mut df = DataFrame::new();
    let ma5_values: Vec<Option<f64>> = (0..count).map(|i| Some(seed + (i as f64) * 1.5)).collect();
    let ma10_values: Vec<Option<f64>> = (0..count)
        .map(|i| Some(seed + 5.0 + if i % 2 == 0 { -3.0 } else { 3.0 }))
        .collect();
    // 前两行 ma5 设为空，演示空值传播
    let mut ma5_values = ma5_values;
    if count > 2 {
        ma5_values[0] = None;
        ma5_values[1] = None;
    }
    df.add_column(DataFrame::new_float64_column("ma5", ma5_values));
    df.add_column(DataFrame::new_float64_column("ma10", ma10_values));
    df.add_column(DataFrame::new_float64_column(
        "close",
        (0..count).map(|i| Some(seed + (i as f64))).collect(),
    ));
    df
}

/// 以 DataFrameArray 输入运行一次表达式算子，返回输出 PortData
fn run_expression(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
    let input_port = PortData::DataFrameArray(input_dfs);
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(params_json).unwrap_or_default();

    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue {
            str_ptr: ptr::null_mut(),
        },
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
                DataType::Int64 => format!("{:?}", col.get_i64(i)),
                DataType::Float64 => format!("{:?}", col.get_f64(i)),
                DataType::String => format!("{:?}", col.get_string(i)),
                DataType::Bool => format!("{:?}", col.get_bool(i)),
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
            println!(
                "=== {} 输出 DataFrameArray ({} 个 DataFrame) ===",
                label,
                dfs.len()
            );
            for (i, df) in dfs.iter().enumerate() {
                println!(
                    "  [DataFrame {}] 行数={}, 列数={}, 列名={:?}",
                    i,
                    df.row_count,
                    df.col_count(),
                    df.columns
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                );
                print_df(df, rows_per_df);
            }
        }
        PortData::DataFrame(df) => {
            println!("=== {} 输出 DataFrame (单) ===", label);
            println!(
                "  行数={}, 列数={}, 列名={:?}",
                df.row_count,
                df.col_count(),
                df.columns
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
            );
            print_df(df, rows_per_df);
        }
        other => println!("=== {} 输出类型: {} ===", label, other.type_name()),
    }
}

fn main() {
    let df_a = build_signal_df(10.0, 10);
    let df_b = build_signal_df(50.0, 10);
    println!(
        "=== 输入 DataFrameArray ===\n  A: 行数={}, 列名={:?}\n  B: 行数={}, 列名={:?}",
        df_a.row_count,
        df_a.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        df_b.row_count,
        df_b.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
    );

    // ---------- 1. 简单比较 ----------
    println!("\n########## 测试 1: ma5 > ma10 ##########");
    let json1 = r#"{ "column_name": "signal", "expression": "ma5 > ma10" }"#;
    if let Some(pd) = run_expression(vec![df_a.clone(), df_b.clone()], json1) {
        print_output("ma5>ma10", &pd, 6);
    }

    // ---------- 2. 复合表达式 ----------
    println!("\n########## 测试 2: ma5 > ma10 && close > 12 ##########");
    let json2 = r#"{ "column_name": "buy", "expression": "ma5 > ma10 && close > 12" }"#;
    if let Some(pd) = run_expression(vec![df_a.clone(), df_b.clone()], json2) {
        print_output("compound", &pd, 6);
    }

    // ---------- 3. 覆盖已有列 ----------
    println!("\n########## 测试 3: 覆盖已有列 ma5 ##########");
    let json3 = r#"{ "column_name": "ma5", "expression": "ma5 > ma10" }"#;
    if let Some(pd) = run_expression(vec![df_a.clone()], json3) {
        print_output("overwrite", &pd, 6);
    }

    // ---------- 4. 算术与括号 ----------
    println!("\n########## 测试 4: (ma5 - ma10) / ma10 > 0.05 ##########");
    let json4 = r#"{ "column_name": "deviation", "expression": "(ma5 - ma10) / ma10 > 0.05" }"#;
    if let Some(pd) = run_expression(vec![df_a.clone(), df_b.clone()], json4) {
        print_output("arithmetic", &pd, 6);
    }

    // ---------- 5. 错误：表达式为空 ----------
    println!("\n########## 测试 5: 表达式为空（应返回 -6）##########");
    let json5 = r#"{ "column_name": "x", "expression": "" }"#;
    run_expression(vec![df_a.clone()], json5);

    // ---------- 6. 错误：引用列不存在 ----------
    println!("\n########## 测试 6: 引用列不存在（应返回 -8）##########");
    let json6 = r#"{ "column_name": "x", "expression": "rsi > 30" }"#;
    run_expression(vec![df_a.clone()], json6);

    // ---------- 7. 错误：语法错误 ----------
    println!("\n########## 测试 7: 语法错误 a < b < c（应返回 -7）##########");
    let json7 = r#"{ "expression": "ma5 < ma10 < close" }"#;
    run_expression(vec![df_a, df_b], json7);
}
