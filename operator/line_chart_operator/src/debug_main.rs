use line_chart_operator::{execute_operator, release_port_data};
use operator_runtime::{DataFrame, PortData};
use operator_runtime::c_abi::{
    portdata_to_c, portdata_from_c, CPortData, CPortValue, TYPE_NULL,
};
use std::ffi::CString;
use std::ptr;

/// 构造一个含 date + close 列的测试 DataFrame（模拟日线收盘价序列）。
fn build_line_df(seed: f64, count: usize, sym: &str) -> DataFrame {
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
    df.add_column(DataFrame::new_float64_column("close", close));

    // 标题列（symbol）——首行值用作折线图标题
    let sym_strings: Vec<String> = (0..count).map(|_| sym.to_string()).collect();
    let syms: Vec<Option<&str>> = sym_strings.iter().map(|s| Some(s.as_str())).collect();
    df.add_column(DataFrame::new_string_column("symbol", syms));

    df
}

/// 以 DataFrameArray 输入运行折线算子，返回输出 PortData。
fn run_line_chart(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
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
    let df_a = build_line_df(10.0, 12, "AAPL");
    let df_b = build_line_df(50.0, 12, "MSFT");
    let df_c = build_line_df(100.0, 8, "GOOG");
    println!(
        "=== 输入 DataFrameArray ===\n  A: {} 行 × {} 列\n  B: {} 行 × {} 列\n  C: {} 行 × {} 列",
        df_a.row_count, df_a.columns.len(),
        df_b.row_count, df_b.columns.len(),
        df_c.row_count, df_c.columns.len(),
    );

    // ---------- 1. 默认参数（全选透传） ----------
    println!("\n########## 测试 1: 默认参数（全选透传）##########");
    let json = r#"{}"#;
    match run_line_chart(vec![df_a.clone(), df_b.clone()], json) {
        Some(PortData::DataFrameArray(dfs)) => {
            println!("--- 输出 DataFrameArray ({} 个) ---", dfs.len());
            for (i, df) in dfs.iter().enumerate() {
                println!("  [{}] {} 行 × {} 列", i, df.row_count, df.columns.len());
            }
        }
        other => println!(
            "未得到 DataFrameArray 输出: {:?}",
            other.as_ref().map(|p| p.type_name()).unwrap_or("None")
        ),
    }

    // ---------- 2. indices="1"（只取第 2 个） ----------
    println!("\n########## 测试 2: indices=\"1\"（只取第 2 个）##########");
    let json = r#"{ "indices": "1" }"#;
    match run_line_chart(vec![df_a.clone(), df_b.clone(), df_c.clone()], json) {
        Some(PortData::DataFrameArray(dfs)) => {
            println!("--- 输出 DataFrameArray ({} 个) ---", dfs.len());
            for (i, df) in dfs.iter().enumerate() {
                let close0 = df.column("close").and_then(|c| c.get_f64(0)).unwrap_or(f64::NAN);
                println!("  [{}] {} 行，首日 close={}", i, df.row_count, close0);
            }
            assert_eq!(dfs.len(), 1, "indices=1 应只选中 1 个");
            let close0 = dfs[0].column("close").unwrap().get_f64(0).unwrap();
            assert!((close0 - 50.0).abs() < 1.0, "应选中 seed=50 的 MSFT");
        }
        other => println!(
            "未得到 DataFrameArray 输出: {:?}",
            other.as_ref().map(|p| p.type_name()).unwrap_or("None")
        ),
    }

    // ---------- 3. 自定义列名 + title_col ----------
    println!("\n########## 测试 3: 自定义列名 + title_col ##########");
    let json = r#"{ "indices": "0", "date_col": "date", "close_col": "close", "title_col": "symbol" }"#;
    match run_line_chart(vec![df_a.clone()], json) {
        Some(PortData::DataFrameArray(dfs)) => {
            println!("--- 输出 DataFrameArray ({} 个)，透传保留全部列 ---", dfs.len());
            let df = &dfs[0];
            println!("  列名: {:?}", df.columns.iter().map(|c| &c.name).collect::<Vec<_>>());
            let sym0 = df.column("symbol").and_then(|c| c.get_string(0)).unwrap_or("");
            println!("  title_col=symbol 首行值: {}", sym0);
        }
        other => println!(
            "未得到 DataFrameArray 输出: {:?}",
            other.as_ref().map(|p| p.type_name()).unwrap_or("None")
        ),
    }

    // ---------- 4. indices 越界 + 全选兜底 ----------
    println!("\n########## 测试 4: indices=\"0,5\"（5 越界，应只选中 0）##########");
    let json = r#"{ "indices": "0,5" }"#;
    match run_line_chart(vec![df_a.clone(), df_b], json) {
        Some(PortData::DataFrameArray(dfs)) => {
            println!("--- 输出 DataFrameArray ({} 个，越界下标被跳过) ---", dfs.len());
            assert_eq!(dfs.len(), 1, "应只选中 1 个（下标 5 越界跳过）");
        }
        other => println!(
            "未得到 DataFrameArray 输出: {:?}",
            other.as_ref().map(|p| p.type_name()).unwrap_or("None")
        ),
    }

    // ---------- 5. 单个 DataFrame 输入（包装为单元素数组） ----------
    println!("\n########## 测试 5: 单个 DataFrame 输入 ##########");
    let input_port = PortData::DataFrame(df_a);
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new("{}").unwrap_or_default();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    println!("execute_operator 返回: {}", rc);
    if output_slots[0].type_tag != TYPE_NULL {
        let pd = unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) };
        match pd {
            PortData::DataFrameArray(dfs) => {
                println!("--- 单 DataFrame 被包装为 {} 元素数组 ---", dfs.len());
            }
            other => println!("期望 DataFrameArray，实际 {:?}", other.type_name()),
        }
    }

    println!("\n=== 全部测试通过 ===");
}
