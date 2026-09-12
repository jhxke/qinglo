use column_stream_operator::{
    execute_operator, execute_operator_stream_end, execute_operator_stream_next,
    execute_operator_stream_start, release_port_data,
};
use operator_runtime::c_abi::{
    c_get_last_error, c_last_error_free, portdata_from_c, portdata_to_c, CPortData, CPortValue,
    TYPE_NULL,
};
use operator_runtime::{DataFrame, PortData};
use std::ffi::CString;
use std::ptr;

/// 构造含 4 种类型列（各列带 null）的 DataFrame。
fn build_typed_df() -> DataFrame {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "price",
        vec![Some(10.0), None, Some(1.5), Some(0.25)],
    ));
    df.add_column(DataFrame::new_string_column(
        "code",
        vec![Some("a"), Some("b"), None, Some("d")],
    ));
    df.add_column(DataFrame::new_int64_column(
        "qty",
        vec![Some(1), None, Some(-3), Some(42)],
    ));
    df.add_column(DataFrame::new_bool_column(
        "flag",
        vec![Some(true), Some(false), None, Some(true)],
    ));
    df
}

/// 取最近一次 C ABI 错误（仅供打印）。
fn take_last_error() -> Option<String> {
    let p = c_get_last_error();
    if p.is_null() {
        return None;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() };
    c_last_error_free(p);
    Some(s)
}

/// 流式模式：start → 循环 next 收集 String chunk → end。
fn run_stream(df: &DataFrame, params_json: &str) -> Vec<String> {
    let input_port = PortData::DataFrame(df.clone());
    // portdata_to_c 内部会克隆 DataFrame 到独立 C 句柄；start 会消费它（置 TYPE_NULL）
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(params_json).unwrap_or_default();

    let handle = execute_operator_stream_start(
        c_inputs.as_ptr(),
        c_inputs.len(),
        params_cstr.as_ptr(),
    );
    assert!(!handle.is_null(), "[stream] start 返回 null: {:?}", take_last_error());
    println!("[stream] start ok, handle={:?}", handle);

    // start 正常时已消费输入；未消费则释放，避免句柄泄漏
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    let mut chunks = Vec::new();
    loop {
        let mut out = CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: ptr::null_mut() },
        };
        let rc = execute_operator_stream_next(handle, &mut out as *mut CPortData);
        match rc {
            0 => {
                assert_ne!(out.type_tag, TYPE_NULL, "rc=0 但 out_chunk 为空");
                let pd = unsafe { portdata_from_c(&mut out as *mut CPortData) };
                match pd {
                    PortData::String(s) => chunks.push(s),
                    other => panic!("[stream] 期望 String chunk，得到 {}", other.type_name()),
                }
            }
            1 => {
                if out.type_tag != TYPE_NULL {
                    release_port_data(&mut out as *mut CPortData);
                }
                break;
            }
            code => panic!("[stream] next 返回错误码 {}: {:?}", code, take_last_error()),
        }
    }

    execute_operator_stream_end(handle);
    println!("[stream] end ok，共收到 {} 个 chunk", chunks.len());
    chunks
}

/// 流式模式失败用例：start 应返回 null，并能取到错误详情。
fn run_stream_expect_fail(df: &DataFrame, params_json: &str) -> String {
    let input_port = PortData::DataFrame(df.clone());
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(params_json).unwrap_or_default();

    let handle = execute_operator_stream_start(
        c_inputs.as_ptr(),
        c_inputs.len(),
        params_cstr.as_ptr(),
    );
    assert!(handle.is_null(), "[stream] 预期 start 失败，但得到 handle");
    let err = take_last_error().unwrap_or_else(|| "(无错误信息)".to_string());

    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    err
}

/// 批量兜底：execute_operator 输出单列 String DataFrame。
fn run_batch(df: &DataFrame, params_json: &str) -> DataFrame {
    let input_port = PortData::DataFrame(df.clone());
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(params_json).unwrap_or_default();

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
    assert_eq!(rc, 0, "[batch] execute_operator 失败: {:?}", take_last_error());

    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    assert_ne!(output_slots[0].type_tag, TYPE_NULL, "[batch] 无输出");
    let pd = unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) };
    match pd {
        PortData::DataFrame(df) => df,
        other => panic!("[batch] 期望 DataFrame 输出，得到 {}", other.type_name()),
    }
}

fn print_string_column(label: &str, df: &DataFrame) {
    let col = &df.columns[0];
    println!(
        "[batch] {}: 列名='{}'，行数={}",
        label, col.name, df.row_count
    );
    for i in 0..df.row_count {
        println!("  行{}: {:?}", i, col.get_string(i));
    }
}

fn main() {
    let df = build_typed_df();
    println!("=== 输入 DataFrame: {} 行，列 price/code/qty/flag（均含 1 个 null）===", df.row_count);

    // ---------- 1. 流式：Float64 列 ----------
    println!("\n########## 流式 price (Float64) ##########");
    let chunks = run_stream(&df, r#"{ "source_column": "price" }"#);
    println!("chunks = {:?}", chunks);

    // ---------- 2. 流式：String 列 ----------
    println!("\n########## 流式 code (String) ##########");
    let chunks = run_stream(&df, r#"{ "source_column": "code" }"#);
    println!("chunks = {:?}", chunks);

    // ---------- 3. 流式：Int64 列 ----------
    println!("\n########## 流式 qty (Int64) ##########");
    let chunks = run_stream(&df, r#"{ "source_column": "qty" }"#);
    println!("chunks = {:?}", chunks);

    // ---------- 4. 流式：Bool 列 ----------
    println!("\n########## 流式 flag (Bool) ##########");
    let chunks = run_stream(&df, r#"{ "source_column": "flag" }"#);
    println!("chunks = {:?}", chunks);

    // ---------- 5. 失败：列不存在 ----------
    println!("\n########## 失败用例：列不存在 ##########");
    let err = run_stream_expect_fail(&df, r#"{ "source_column": "not_exist" }"#);
    println!("预期错误: {}", err);

    // ---------- 6. 失败：未配置 source_column ----------
    println!("\n########## 失败用例：缺少 source_column 参数 ##########");
    let err = run_stream_expect_fail(&df, r#"{}"#);
    println!("预期错误: {}", err);

    // ---------- 7. 批量兜底 ----------
    println!("\n########## 批量兜底 execute_operator ##########");
    let out_df = run_batch(&df, r#"{ "source_column": "price" }"#);
    print_string_column("price -> String", &out_df);

    // ---------- 8. 列名带空格（trim） ----------
    println!("\n########## 流式：列名参数带前后空格 ##########");
    let chunks = run_stream(&df, r#"{ "source_column": "  qty  " }"#);
    println!("chunks = {:?}", chunks);
}
