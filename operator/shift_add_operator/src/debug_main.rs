use operator_runtime::c_abi::{
    portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use shift_add_operator::{execute_operator, release_port_data};
use std::ffi::CString;
use std::ptr;

/// 构造测试 DataFrame：含 s (Float64) 与 t (Int64)，并在 s 中插入一个空值
fn build_df() -> DataFrame {
    let mut df = DataFrame::new();

    // 源列 s：第 2 行故意留空，验证空值传播
    let s_values: Vec<Option<f64>> = vec![Some(1.0), Some(2.0), None, Some(4.0), Some(5.0)];
    df.add_column(DataFrame::new_float64_column("s", s_values));

    // 目标列 t：Int64，验证类型保留
    df.add_column(DataFrame::new_int64_column(
        "t",
        vec![Some(10), Some(20), Some(30), Some(40), Some(50)],
    ));

    // 目标列 u：Float64，验证多目标
    df.add_column(DataFrame::new_float64_column(
        "u",
        vec![Some(100.0), Some(200.0), Some(300.0), Some(400.0), Some(500.0)],
    ));

    // 字符串列：验证非数值列被跳过
    df.add_column(DataFrame::new_string_column(
        "code",
        vec![Some("A"), Some("A"), Some("A"), Some("A"), Some("A")],
    ));

    df
}

/// 以 DataFrameArray 输入运行一次前移加算子，返回输出 PortData
fn run_shift_add(input_dfs: Vec<DataFrame>, params_json: &str) -> Option<PortData> {
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
    println!("########## 前移加算子调试 ##########\n");

    // ---------- 1. 前移 2 行，加到 t,u 两列 ----------
    println!("########## 测试1: source=s, shift_n=2, targets=t,u ##########");
    let df = build_df();
    println!("输入:");
    print_df(&df, 5);
    let json = r#"{ "source_column": "s", "shift_n": "2", "target_columns": "t,u" }"#;
    if let Some(pd) = run_shift_add(vec![df], json) {
        if let PortData::DataFrameArray(dfs) = pd {
            println!("输出:");
            print_df(&dfs[0], 5);
            // t 为 Int64，s 为 Float64 -> t 提升为 Float64
            println!(
                "  t 列类型: {:?} (源 Float64 + 目标 Int64 应提升为 Float64)",
                dfs[0].column("t").unwrap().data_type
            );
        }
    }

    // ---------- 2. 前移 1 行，只加到 t ----------
    println!("\n########## 测试2: source=s, shift_n=1, targets=t ##########");
    let df = build_df();
    let json = r#"{ "source_column": "s", "shift_n": "1", "target_columns": "t" }"#;
    if let Some(pd) = run_shift_add(vec![df], json) {
        if let PortData::DataFrameArray(dfs) = pd {
            print_df(&dfs[0], 5);
        }
    }

    // ---------- 3. shift_n=0（直接相加） ----------
    println!("\n########## 测试3: source=s, shift_n=0, targets=u ##########");
    let df = build_df();
    let json = r#"{ "source_column": "s", "shift_n": "0", "target_columns": "u" }"#;
    if let Some(pd) = run_shift_add(vec![df], json) {
        if let PortData::DataFrameArray(dfs) = pd {
            print_df(&dfs[0], 5);
        }
    }

    // ---------- 4. 含不存在源列（应返回 -6） ----------
    println!("\n########## 测试4: source 为空（应返回 -6）##########");
    let df = build_df();
    let json = r#"{ "source_column": "", "shift_n": "1", "target_columns": "t" }"#;
    let _ = run_shift_add(vec![df], json);

    // ---------- 5. 目标列含字符串列（应跳过 code） ----------
    println!("\n########## 测试5: targets=t,code（code 为字符串应跳过）##########");
    let df = build_df();
    let json = r#"{ "source_column": "s", "shift_n": "1", "target_columns": "t,code" }"#;
    if let Some(pd) = run_shift_add(vec![df], json) {
        if let PortData::DataFrameArray(dfs) = pd {
            print_df(&dfs[0], 5);
        }
    }
}
