use histogram_visualization_operator::{execute_operator, release_port_data};
use operator_runtime::c_abi::{portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, DataType, PortData};
use std::ffi::CString;
use std::ptr;

fn make_histogram_df(bins: usize) -> DataFrame {
    let mut df = DataFrame::new();
    let idx: Vec<Option<i64>> = (0..bins as i64).map(Some).collect();
    let width = 0.1;
    let start = - (bins as f64) * width / 2.0;
    let left: Vec<Option<f64>> = (0..bins).map(|i| Some(start + i as f64 * width)).collect();
    let right: Vec<Option<f64>> = (0..bins).map(|i| Some(start + (i as f64 + 1.0) * width)).collect();
    let center: Vec<Option<f64>> = left.iter().zip(right.iter())
        .map(|(l, r)| Some((l.unwrap() + r.unwrap()) / 2.0)).collect();
    // 模拟正态分布形直方图
    let mid = bins as f64 / 2.0;
    let count: Vec<Option<i64>> = (0..bins)
        .map(|i| {
            let d = i as f64 - mid + 0.5;
            let v = (-(d * d) / (2.0 * 2.0 * 2.0)).exp() * 100.0;
            Some(v.max(1.0) as i64)
        })
        .collect();
    let total: i64 = count.iter().map(|v| v.unwrap_or(0)).sum();
    let freq: Vec<Option<f64>> = count.iter()
        .map(|v| Some(v.unwrap_or(0) as f64 / total as f64)).collect();
    df.add_column(DataFrame::new_int64_column("bin_index", idx));
    df.add_column(DataFrame::new_float64_column("bin_left", left));
    df.add_column(DataFrame::new_float64_column("bin_right", right));
    df.add_column(DataFrame::new_float64_column("bin_center", center));
    df.add_column(DataFrame::new_int64_column("count", count));
    df.add_column(DataFrame::new_float64_column("frequency", freq));
    df
}

fn run_viz(input_pd: PortData, params_json: &str) -> Option<PortData> {
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
    println!("  行数={}, 列数={}, 列名={:?}", df.row_count, df.col_count(),
        df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>());
    for i in 0..df.row_count.min(10) {
        let mut vals: Vec<String> = Vec::new();
        for col in &df.columns {
            let v = match col.data_type {
                DataType::Float64 => format!("{:.4?}", col.get_f64(i)),
                DataType::Int64 => format!("{:?}", col.get_i64(i)),
                _ => "?".to_string(),
            };
            vals.push(v);
        }
        println!("  行{}: {}", i, vals.join(", "));
    }
}

fn main() {
    let df = make_histogram_df(15);
    println!("=== 输入直方图 DataFrame ===");
    print_df(&df);

    // ---------- 测试 1: DataFrame 输入，默认列配置 ----------
    println!("\n########## 测试 1: 默认配置 (x=bin_center, y=count) ##########");
    let json1 = "{}";
    let input1 = PortData::DataFrame(df.clone());
    if let Some(pd) = run_viz(input1, json1) {
        match pd {
            PortData::DataFrame(out) => {
                println!("  输出 DataFrame 透传成功:");
                print_df(&out);
            }
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // ---------- 测试 2: 自定义列名 + 标题 ----------
    println!("\n########## 测试 2: y_col=frequency, title=正态分布直方图 ##########");
    let json2 = r#"{ "y_col": "frequency", "title": "Normal Distribution" }"#;
    let input2 = PortData::DataFrame(df.clone());
    if let Some(pd) = run_viz(input2, json2) {
        match pd {
            PortData::DataFrame(_) => println!("  透传成功"),
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // ---------- 测试 3: DataFrameArray 输入（应取第一张）----------
    println!("\n########## 测试 3: DataFrameArray 输入（取第一张）##########");
    let df2 = make_histogram_df(8);
    let input3 = PortData::DataFrameArray(vec![df, df2]);
    let json3 = "{}";
    if let Some(pd) = run_viz(input3, json3) {
        match pd {
            PortData::DataFrame(out) => println!("  输出行数={} (应为第一张 15 行)", out.row_count),
            other => println!("  输出类型: {:?}", other.type_name()),
        }
    }

    // ---------- 测试 4: 错误：缺少输入 ----------
    println!("\n########## 测试 4: 缺少输入（应返回 -3）##########");
    let params_cstr = CString::new("{}").unwrap();
    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        std::ptr::null(), 0,
        out.as_mut_ptr(), out.len(),
        params_cstr.as_ptr(),
    );
    println!("  rc={} (期望 -3)", rc);
}
