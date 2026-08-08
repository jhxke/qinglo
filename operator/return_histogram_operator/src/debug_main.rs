use return_histogram_operator::{execute_operator, release_port_data};
use operator_runtime::c_abi::{portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, PortData};
use std::ffi::CString;
use std::ptr;

fn build_test_df(seed: f64, count: usize) -> DataFrame {
    let mut df = DataFrame::new();
    let ma5: Vec<Option<f64>> = (0..count)
        .map(|i| Some(seed + (i as f64) * 1.5 + if i % 2 == 0 { -5.0 } else { 5.0 }))
        .collect();
    let ma10: Vec<Option<f64>> = (0..count).map(|_| Some(seed + 5.0)).collect();
    // 未来收益率列: 模拟 [i%=0:0.1, 1:-0.05, 2:0.2, 3:-0.15...]
    let ret_vals = [0.1f64, -0.05, 0.2, -0.15, 0.03, -0.08, 0.12, -0.02];
    let future_ret: Vec<Option<f64>> = (0..count)
        .map(|i| Some(ret_vals[i % ret_vals.len()]))
        .collect();
    df.add_column(DataFrame::new_float64_column("ma5", ma5));
    df.add_column(DataFrame::new_float64_column("ma10", ma10));
    df.add_column(DataFrame::new_float64_column("future_return_5", future_ret));
    df
}

fn run_histogram(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
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

fn print_histogram_df(df: &DataFrame) {
    println!(
        "  直方图 DataFrame: 行数={}, 列={:?}",
        df.row_count,
        df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
    );
    for i in 0..df.row_count {
        let idx = df.column("bin_index").and_then(|c| c.get_i64(i));
        let left = df.column("bin_left").and_then(|c| c.get_f64(i));
        let right = df.column("bin_right").and_then(|c| c.get_f64(i));
        let cnt = df.column("count").and_then(|c| c.get_i64(i));
        let freq = df.column("frequency").and_then(|c| c.get_f64(i));
        // 简易文本柱状图
        let bar_len = cnt.unwrap_or(0) as usize;
        let bar: String = std::iter::repeat('#').take(bar_len.min(40)).collect();
        println!(
            "  箱{:>2} [{:>8.4}, {:>8.4}) count={:>4} freq={:.4}  {}",
            idx.unwrap_or(-1),
            left.unwrap_or(0.0),
            right.unwrap_or(0.0),
            cnt.unwrap_or(0),
            freq.unwrap_or(0.0),
            bar
        );
    }
}

fn main() {
    let df_a = build_test_df(10.0, 20);
    let df_b = build_test_df(50.0, 20);
    println!(
        "=== 输入 DataFrameArray ===\n  A: 行数={}, B: 行数={}",
        df_a.row_count, df_b.row_count
    );

    // ---------- 测试 1: 基础直方图 ----------
    println!("\n########## 测试 1: ma5 > ma10 条件，bins=10 ##########");
    let json1 = r#"{ "expression": "ma5 > ma10", "value_column": "future_return_5", "bins": "10" }"#;
    if let Some(pd) = run_histogram(vec![df_a.clone(), df_b.clone()], json1) {
        match pd {
            PortData::DataFrame(df) => print_histogram_df(&df),
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // ---------- 测试 2: 自定义范围 ----------
    println!("\n########## 测试 2: 范围 [-0.2, 0.3], bins=5 ##########");
    let json2 = r#"{ "expression": "ma5 > ma10", "value_column": "future_return_5", "bins": "5", "min_val": "-0.2", "max_val": "0.3" }"#;
    if let Some(pd) = run_histogram(vec![df_a.clone(), df_b.clone()], json2) {
        match pd {
            PortData::DataFrame(df) => print_histogram_df(&df),
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // ---------- 测试 3: 复合表达式 ----------
    println!("\n########## 测试 3: ma5 > ma10 && future_return_5 > 0（正收益），bins=5 ##########");
    let json3 = r#"{ "expression": "ma5 > ma10 && future_return_5 > 0", "value_column": "future_return_5", "bins": "5" }"#;
    if let Some(pd) = run_histogram(vec![df_a.clone()], json3) {
        match pd {
            PortData::DataFrame(df) => print_histogram_df(&df),
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // ---------- 测试 4: 错误：expression 空 ----------
    println!("\n########## 测试 4: expression 空（应返回 -6）##########");
    run_histogram(vec![df_a.clone()], r#"{ "value_column": "x" }"#);

    // ---------- 测试 5: 错误：value_column 空 ----------
    println!("\n########## 测试 5: value_column 空（应返回 -6）##########");
    run_histogram(vec![df_a], r#"{ "expression": "a > b" }"#);
}
