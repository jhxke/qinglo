use factor_histogram_operator::{execute_operator, release_port_data};
use operator_runtime::c_abi::{portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, DataType, PortData};
use std::ffi::CString;
use std::ptr;

/// 构造 50 行示例：factor 在 [0, 10) 均匀分布，return 随 factor 线性增大（加少量噪声）
fn build_sample_df() -> DataFrame {
    let mut df = DataFrame::new();
    let n = 50usize;
    let factor: Vec<Option<f64>> = (0..n).map(|i| Some(i as f64 * 0.2)).collect();
    let ret: Vec<Option<f64>> = (0..n)
        .map(|i| {
            let f = i as f64 * 0.2;
            // 因子越大收益越高（线性趋势），加一点波动模拟真实数据
            Some(f * 0.05 + (i as f64 % 7.0 - 3.0) * 0.002)
        })
        .collect();
    df.add_column(DataFrame::new_float64_column("factor", factor));
    df.add_column(DataFrame::new_float64_column("return", ret));
    df
}

fn run_factor_hist(input_pd: PortData, params_json: &str) -> Option<PortData> {
    let mut c_inputs = [portdata_to_c(&input_pd)];
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

fn print_df(df: &DataFrame) {
    println!(
        "  行数={}, 列数={}, 列名={:?}",
        df.row_count,
        df.col_count(),
        df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
    );
    for i in 0..df.row_count.min(12) {
        let mut vals: Vec<String> = Vec::new();
        for col in &df.columns {
            let v = match col.data_type {
                DataType::Float64 => format!("{:.4}", col.get_f64(i).unwrap_or(f64::NAN)),
                DataType::Int64 => format!("{}", col.get_i64(i).unwrap_or(0)),
                _ => "?".to_string(),
            };
            vals.push(format!("{}={}", col.name, v));
        }
        println!("  行{}: {}", i, vals.join(", "));
    }
    if df.row_count > 12 {
        println!("  ... 还有 {} 行", df.row_count - 12);
    }
}

fn main() {
    let df = build_sample_df();
    println!(
        "=== 输入 DataFrame ===\n  行数={}, 列名={:?}",
        df.row_count,
        df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
    );

    // 1. 默认配置 bins=20, result=mean_return
    println!("\n########## 测试 1: 默认 bins=20, result=mean_return ##########");
    let json1 = r#"{"factor_column":"factor","return_column":"return"}"#;
    if let Some(pd) = run_factor_hist(PortData::DataFrame(df.clone()), json1) {
        match pd {
            PortData::DataFrame(out) => print_df(&out),
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // 2. 自定义 bins + 边界
    println!("\n########## 测试 2: bins=5, min=0, max=10 ##########");
    let json2 = r#"{"factor_column":"factor","return_column":"return","bins":"5","min_val":"0","max_val":"10"}"#;
    if let Some(pd) = run_factor_hist(PortData::DataFrame(df.clone()), json2) {
        match pd {
            PortData::DataFrame(out) => print_df(&out),
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // 3. 自定义均值列名
    println!("\n########## 测试 3: result_column=avg_ret ##########");
    let json3 = r#"{"factor_column":"factor","return_column":"return","bins":"5","result_column":"avg_ret"}"#;
    if let Some(pd) = run_factor_hist(PortData::DataFrame(df.clone()), json3) {
        match pd {
            PortData::DataFrame(out) => {
                println!("  含 avg_ret 列: {}", out.column("avg_ret").is_some());
                print_df(&out);
            }
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // 4. DataFrameArray 输入（多表汇总）
    println!("\n########## 测试 4: DataFrameArray 输入（2 表汇总）##########");
    let df2 = build_sample_df();
    if let Some(pd) = run_factor_hist(
        PortData::DataFrameArray(vec![df, df2]),
        r#"{"factor_column":"factor","return_column":"return","bins":"5"}"#,
    ) {
        match pd {
            PortData::DataFrame(out) => {
                println!("  汇总后样本数翻倍:");
                print_df(&out);
            }
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // 5. 错误：缺少输入
    println!("\n########## 测试 5: 缺少输入（应返回 -3）##########");
    let params_cstr =
        CString::new(r#"{"factor_column":"factor","return_column":"return"}"#).unwrap();
    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        ptr::null(),
        0,
        out.as_mut_ptr(),
        out.len(),
        params_cstr.as_ptr(),
    );
    println!("  rc={} (期望 -3)", rc);
}
