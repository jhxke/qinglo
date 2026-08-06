use kline_visualization_operator::{execute_operator, release_port_data};
use operator_runtime::{DataFrame, PortData};
use operator_runtime::c_abi::{
    portdata_to_c, portdata_from_c, CPortData, CPortValue, TYPE_NULL,
};
use std::ffi::CString;
use std::ptr;

/// 构造一个含 date/open/high/low/close/ma5/ma10 列的测试 DataFrame（模拟日线行情）。
fn build_kline_df(seed: f64, count: usize) -> DataFrame {
    let mut df = DataFrame::new();
    let date_strings: Vec<String> = (0..count).map(|i| format!("2024-01-{:02}", i + 1)).collect();
    let dates: Vec<Option<&str>> = date_strings.iter().map(|s| Some(s.as_str())).collect();
    df.add_column(DataFrame::new_string_column("date", dates));

    // 收盘价以 seed 为起点，逐日小幅波动
    let close: Vec<Option<f64>> = (0..count)
        .map(|i| {
            let v = seed + (i as f64) * 0.3 + if i % 3 == 0 { -0.4 } else { 0.25 };
            Some(v)
        })
        .collect();
    // open ≈ 上一日 close
    let open: Vec<Option<f64>> = (0..count)
        .map(|i| {
            if i == 0 {
                Some(seed - 0.1)
            } else {
                close[i - 1]
            }
        })
        .collect();
    let high: Vec<Option<f64>> = (0..count)
        .map(|i| Some(close[i].unwrap_or(0.0).max(open[i].unwrap_or(0.0)) + 0.5))
        .collect();
    let low: Vec<Option<f64>> = (0..count)
        .map(|i| Some(close[i].unwrap_or(0.0).min(open[i].unwrap_or(0.0)) - 0.5))
        .collect();

    // MA5 / MA10：前若干个为 None
    let ma5: Vec<Option<f64>> = (0..count)
        .map(|i| if i >= 4 { Some(seed + (i as f64) * 0.3) } else { None })
        .collect();
    let ma10: Vec<Option<f64>> = (0..count)
        .map(|i| if i >= 9 { Some(seed + (i as f64) * 0.28) } else { None })
        .collect();

    df.add_column(DataFrame::new_float64_column("open", open));
    df.add_column(DataFrame::new_float64_column("high", high));
    df.add_column(DataFrame::new_float64_column("low", low));
    df.add_column(DataFrame::new_float64_column("close", close));
    df.add_column(DataFrame::new_float64_column("ma5", ma5));
    df.add_column(DataFrame::new_float64_column("ma10", ma10));
    df
}

/// 以 DataFrameArray 输入运行 K线算子，返回输出 PortData。
fn run_kline(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
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

    // 若输入未被消费（提前返回错误），释放输入句柄
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    if output_slots[0].type_tag != TYPE_NULL {
        Some(unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) })
    } else {
        None
    }
}

fn main() {
    let df_a = build_kline_df(10.0, 12);
    let df_b = build_kline_df(50.0, 12);
    println!(
        "=== 输入 DataFrameArray ===\n  A: {} 行 × {} 列\n  B: {} 行 × {} 列",
        df_a.row_count, df_a.columns.len(),
        df_b.row_count, df_b.columns.len(),
    );

    // ---------- 1. 默认参数（全选） ----------
    println!("\n########## 测试 1: 默认参数（全选）##########");
    let json = r#"{}"#;
    if let Some(PortData::String(dsl)) = run_kline(vec![df_a.clone(), df_b.clone()], json) {
        println!("--- DSL 输出 ({} 字符) ---\n{}", dsl.chars().count(), dsl);
    } else {
        println!("未得到 String 输出");
    }

    // ---------- 2. indices="1" ----------
    println!("\n########## 测试 2: indices=\"1\"（只取第 2 个）##########");
    let json = r#"{ "indices": "1" }"#;
    if let Some(PortData::String(dsl)) = run_kline(vec![df_a.clone(), df_b.clone()], json) {
        println!("--- DSL 输出 ---\n{}", dsl);
    }

    // ---------- 3. 自定义列名 ----------
    println!("\n########## 测试 3: 自定义列名 ##########");
    let mut df_c = DataFrame::new();
    let dates: Vec<Option<&str>> = (0..5).map(|i| Some(["d1", "d2", "d3", "d4", "d5"][i])).collect();
    df_c.add_column(DataFrame::new_string_column("dt", dates));
    df_c.add_column(DataFrame::new_float64_column("o", vec![Some(1.0); 5]));
    df_c.add_column(DataFrame::new_float64_column("h", vec![Some(1.5); 5]));
    df_c.add_column(DataFrame::new_float64_column("l", vec![Some(0.5); 5]));
    df_c.add_column(DataFrame::new_float64_column("c", vec![Some(1.2); 5]));
    let json = r#"{ "open_col": "o", "high_col": "h", "low_col": "l", "close_col": "c", "date_col": "dt", "ma5_col": "", "ma10_col": "" }"#;
    if let Some(PortData::String(dsl)) = run_kline(vec![df_c], json) {
        println!("--- DSL 输出 ---\n{}", dsl);
    }

    // ---------- 4. 缺 MA10 列 ----------
    println!("\n########## 测试 4: 缺 MA10 列（应只输出 MA5 线）##########");
    let json = r#"{ "indices": "0" }"#;
    if let Some(PortData::String(dsl)) = run_kline(vec![df_a], json) {
        println!("--- DSL 输出 ---\n{}", dsl);
    }
}
