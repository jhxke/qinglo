use future_return_operator::{execute_operator, release_port_data};
use operator_runtime::c_abi::{portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, DataType, PortData};
use std::ffi::CString;
use std::ptr;

/// 构造测试用 DataFrame：含 close (Float64) 与 volume (Int64)，并在 close 中插入一个空值
fn build_stock_df(seed_close: f64, count: usize) -> DataFrame {
    let mut df = DataFrame::new();

    // 收盘价：确定性序列，第 2 行故意留空，验证空值传播语义
    let close_values: Vec<Option<f64>> = (0..count)
        .map(|i| {
            if i == 2 {
                None
            } else {
                Some(seed_close + (i as f64) * 1.5)
            }
        })
        .collect();
    let close_col = DataFrame::new_float64_column("close", close_values);

    // 成交量：Int64 序列，验证类型提升
    let volume_col = DataFrame::new_int64_column(
        "volume",
        (0..count).map(|i| Some(100 + (i as i64) * 10)).collect(),
    );

    // 代码列：字符串，验证非数值列被跳过
    let code_col = DataFrame::new_string_column(
        "code",
        (0..count).map(|_| Some("TEST")).collect(),
    );

    df.add_column(code_col);
    df.add_column(close_col);
    df.add_column(volume_col);
    df
}

/// 以 DataFrameArray 输入运行一次未来收益算子，返回输出 PortData
fn run_future_return(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
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

fn main() {
    let df_a = build_stock_df(10.0, 8);
    let df_b = build_stock_df(50.0, 8);
    println!(
        "=== 输入 DataFrameArray ===\n  A: 行数={}, 列名={:?}\n  B: 行数={}, 列名={:?}",
        df_a.row_count,
        df_a.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        df_b.row_count,
        df_b.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
    );

    // ---------- 1. 默认 n=5，结果列自动取 future_return_5 ----------
    println!("\n########## 测试 1：默认 n=5，结果列自动取 future_return_5 ##########");
    let json = r#"{ "source_column": "close" }"#;
    if let Some(pd) = run_future_return(vec![df_a.clone(), df_b.clone()], json) {
        match pd {
            PortData::DataFrameArray(dfs) => {
                println!("输出 DataFrameArray ({} 个)", dfs.len());
                for (i, df) in dfs.iter().enumerate() {
                    println!(
                        "  [DataFrame {}] 行数={}, 列名={:?}",
                        i, df.row_count,
                        df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                    );
                    print_df(df, 8);
                }
            }
            other => println!("意外输出类型: {}", other.type_name()),
        }
    }

    // ---------- 2. 自定义 n 与结果列名（源列保留） ----------
    println!("\n########## 测试 2：n=3, result_column=fwd_ret_3（源列保留）##########");
    let json = r#"{ "n": "3", "result_column": "fwd_ret_3", "source_column": "close" }"#;
    if let Some(pd) = run_future_return(vec![df_a.clone()], json) {
        match pd {
            PortData::DataFrameArray(dfs) => {
                println!(
                    "输出 DataFrameArray ({} 个，应为 1)，列名={:?}",
                    dfs.len(),
                    dfs[0].columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                );
                print_df(&dfs[0], 8);
            }
            other => println!("意外输出类型: {}", other.type_name()),
        }
    }

    // ---------- 3. Int64 源列（volume）→ 结果提升为 Float64 ----------
    println!("\n########## 测试 3：Int64 源列 volume，n=2，结果提升为 Float64 ##########");
    let json = r#"{ "n": "2", "source_column": "volume", "result_column": "vol_ret_2" }"#;
    if let Some(pd) = run_future_return(vec![df_a.clone()], json) {
        match pd {
            PortData::DataFrameArray(dfs) => {
                let col = dfs[0].column("vol_ret_2").unwrap();
                println!("vol_ret_2 类型: {:?}", col.data_type);
                print_df(&dfs[0], 8);
            }
            other => println!("意外输出类型: {}", other.type_name()),
        }
    }

    // ---------- 4. 单个 DataFrame 输入（兼容性） ----------
    println!("\n########## 兼容性：单个 DataFrame 输入 ##########");
    if let Some(pd) = run_future_return(vec![df_a.clone()], r#"{ "n": "1" }"#) {
        match pd {
            PortData::DataFrameArray(dfs) => {
                println!("输出 DataFrameArray ({} 个，应为 1)", dfs.len());
                print_df(&dfs[0], 8);
            }
            other => println!("意外输出类型: {}", other.type_name()),
        }
    }

    // ---------- 5. 含不存在列（应跳过，原样返回） ----------
    println!("\n########## 测试跳过：source_column=nope(不存在) ##########");
    if let Some(pd) = run_future_return(vec![df_a.clone()], r#"{ "n": "5", "source_column": "nope" }"#) {
        match pd {
            PortData::DataFrameArray(dfs) => {
                print_df(&dfs[0], 8);
            }
            other => println!("意外输出类型: {}", other.type_name()),
        }
    }

    // ---------- 6. 非法 n（应返回错误码 -6） ----------
    println!("\n########## 非法 n 测试（应返回 -6）##########");
    let _ = run_future_return(vec![df_a.clone()], r#"{ "n": "0" }"#);
    let _ = run_future_return(vec![df_a.clone()], r#"{ "n": "abc" }"#);

    // ---------- 7. n 大于等于行数（末尾全空） ----------
    println!("\n########## 测试 7：n=10 大于行数 8（结果全空）##########");
    if let Some(pd) = run_future_return(vec![df_b.clone()], r#"{ "n": "10", "result_column": "ret_10" }"#) {
        match pd {
            PortData::DataFrameArray(dfs) => {
                print_df(&dfs[0], 8);
            }
            other => println!("意外输出类型: {}", other.type_name()),
        }
    }

    // ---------- 8. 字符串源列（应跳过） ----------
    println!("\n########## 测试跳过：source_column=code(字符串) ##########");
    if let Some(pd) = run_future_return(vec![df_b], r#"{ "n": "2", "source_column": "code" }"#) {
        match pd {
            PortData::DataFrameArray(dfs) => {
                print_df(&dfs[0], 8);
            }
            other => println!("意外输出类型: {}", other.type_name()),
        }
    }
}
