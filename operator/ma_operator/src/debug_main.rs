use ma_operator::{execute_operator, release_port_data};
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    portdata_to_c, portdata_from_c, CPortData, CPortValue, TYPE_NULL,
};
use std::ffi::CString;
use std::ptr;

/// 构造一个测试用 DataFrame：模拟某只股票的日线行情（id / close / volume）
fn build_stock_df(seed_close: f64, count: usize) -> DataFrame {
    let mut df = DataFrame::new();
    let id_col = DataFrame::new_int64_column(
        "id",
        (1..=count).map(|i| Some(i as i64)).collect(),
    );
    // 收盘价：以 seed_close 为起点，叠加一个有涨有跌的确定性序列
    let close_values: Vec<Option<f64>> = (0..count)
        .map(|i| {
            let v = seed_close
                + (i as f64) * 0.5
                + if i % 5 == 0 { -1.5 } else { 0.8 };
            Some(v)
        })
        .collect();
    let close_col = DataFrame::new_float64_column("close", close_values);
    let volume_col = DataFrame::new_int64_column(
        "volume",
        (0..count)
            .map(|i| Some((1000 + (i as i64) * 50) % 2000))
            .collect(),
    );

    df.add_column(id_col);
    df.add_column(close_col);
    df.add_column(volume_col);
    df
}

/// 以 DataFrameArray 输入运行一次 MA 算子，返回输出 PortData
///
/// 注意：execute_operator 内部会通过 portdata_from_c 消费输入 CPortData
/// （将其 type_tag 置为 TYPE_NULL）。因此这里只保留一份 CPortData（数组元素本身），
/// 调用结束后根据 type_tag 判断是否需要释放，避免对已消费的句柄重复释放。
fn run_ma(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
    let input_port = PortData::DataFrameArray(input_dfs);
    // portdata_to_c 内部会克隆 DataFrame 到独立的 C 句柄，原 PortData 仍持有所有权
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

    // 若输入未被消费（execute_operator 提前返回错误），则释放输入句柄
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    // 提取输出（portdata_from_c 会消费 output_slots[0] 的句柄）
    let output_pd = if output_slots[0].type_tag != TYPE_NULL {
        Some(unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) })
    } else {
        None
    };

    output_pd
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

/// 打印 DataFrameArray 输出
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
        PortData::DataFrame(df) => {
            println!("=== {} 输出 DataFrame (单) ===", label);
            println!(
                "  行数={}, 列数={}, 列名={:?}",
                df.row_count,
                df.col_count(),
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            print_df(df, rows_per_df);
        }
        other => println!("=== {} 输出类型: {} ===", label, other.type_name()),
    }
}

fn main() {
    // 构造 DataFrameArray：2 个 DataFrame（模拟 2 只股票），各 30 行
    let df_a = build_stock_df(10.0, 30);
    let df_b = build_stock_df(50.0, 30);
    println!(
        "=== 输入 DataFrameArray ===\n  A: 行数={}, 列名={:?}\n  B: 行数={}, 列名={:?}",
        df_a.row_count,
        df_a.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        df_b.row_count,
        df_b.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
    );

    // ---------- 1. MA (5,10,20) ----------
    println!("\n########## 测试 MA (5,10,20) ##########");
    let ma_json = r#"{ "ma_periods": "5,10,20" }"#;
    if let Some(pd) = run_ma(vec![df_a.clone(), df_b.clone()], ma_json) {
        print_output("MA", &pd, 8);
    }

    // ---------- 2. 兼容性：单个 DataFrame 输入 ----------
    println!("\n########## 兼容性测试：单个 DataFrame 输入（应输出单元素 DataFrameArray）##########");
    if let Some(pd) = run_ma(vec![df_a.clone()], ma_json) {
        print_output("Single-DF MA", &pd, 5);
    }

    // ---------- 3. 自定义源列名（source_column=price，非标准 close） ----------
    println!("\n########## 测试自定义源列 (source_column=price) ##########");
    let mut df_price = DataFrame::new();
    let price_col = DataFrame::new_float64_column(
        "price",
        (0..30)
            .map(|i| {
                Some(10.0 + (i as f64) * 0.5 + if i % 5 == 0 { -1.5 } else { 0.8 })
            })
            .collect(),
    );
    df_price.add_column(price_col);
    let sc_json = r#"{ "ma_periods": "5,10", "source_column": "price" }"#;
    if let Some(pd) = run_ma(vec![df_price], sc_json) {
        print_output("MA-SOURCE-COL", &pd, 5);
    }

    // ---------- 4. 空参数：原样返回输入 ----------
    println!("\n########## 空参数测试：原样返回输入 ##########");
    let empty_json = r#"{}"#;
    if let Some(pd) = run_ma(vec![df_a, df_b], empty_json) {
        print_output("EMPTY", &pd, 5);
    }
}
