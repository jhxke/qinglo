use chat_visualization_operator::{
    execute_operator,
    execute_operator_stream_start,
    execute_operator_stream_push,
    execute_operator_stream_push_end,
    execute_operator_stream_next,
    execute_operator_stream_end,
    release_port_data,
};
use operator_runtime::PortData;
use operator_runtime::c_abi::{
    portdata_to_c, portdata_from_c, CPortData, CPortValue, TYPE_NULL,
};
use std::ffi::CString;
use std::ptr;

/// 批量模式测试：一次性输入 prompt + 完整 tokens，输出 DSL。
fn run_batch(prompt: Option<&str>, tokens: Option<&str>, params_json: &str) -> Option<PortData> {
    // 构造输入端口数组：端口0=prompt(String), 端口1=tokens(String)
    let mut c_inputs: Vec<CPortData> = Vec::new();
    if let Some(p) = prompt {
        c_inputs.push(portdata_to_c(&PortData::String(p.to_string())));
    }
    if let Some(t) = tokens {
        c_inputs.push(portdata_to_c(&PortData::String(t.to_string())));
    }
    // 保证至少一个端口存在（即使 NULL）——execute_operator 会判空处理
    if c_inputs.is_empty() {
        c_inputs.push(CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: ptr::null_mut() } });
    }

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
    println!("[batch] execute_operator 返回: {}", result);

    // 释放未消费的输入
    for mut ci in c_inputs {
        if ci.type_tag != TYPE_NULL {
            release_port_data(&mut ci as *mut CPortData);
        }
    }

    if output_slots[0].type_tag != TYPE_NULL {
        Some(unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) })
    } else {
        None
    }
}

/// 流式模式测试：模拟 ollama 逐 token push，逐次 next 拉 DSL。
fn run_stream(prompt: Option<&str>, token_chunks: &[&str], params_json: &str) {
    // ---- stream_start ----
    let mut c_inputs: Vec<CPortData> = Vec::new();
    if let Some(p) = prompt {
        c_inputs.push(portdata_to_c(&PortData::String(p.to_string())));
    }
    let inputs_ptr = if c_inputs.is_empty() { ptr::null() } else { c_inputs.as_ptr() };
    let inputs_count = c_inputs.len();

    let params_cstr = CString::new(params_json).unwrap_or_default();
    let handle = execute_operator_stream_start(inputs_ptr, inputs_count, params_cstr.as_ptr());
    assert!(!handle.is_null(), "[stream] stream_start 返回 null");
    println!("[stream] stream_start ok, handle={:?}", handle);

    // 释放输入（如果没被 start 消费掉的话——start 已消费端口0，其他已为 null）
    for mut ci in c_inputs {
        if ci.type_tag != TYPE_NULL {
            release_port_data(&mut ci as *mut CPortData);
        }
    }

    // ---- 初始态 next（应产出 user + 空 assistant + streaming）----
    println!("\n-- next #0 (初始态) --");
    drain_next_once(handle);

    // ---- push token ----
    for (i, tok) in token_chunks.iter().enumerate() {
        let c_chunk = portdata_to_c(&PortData::String(tok.to_string()));
        let rc = execute_operator_stream_push(handle, &c_chunk as *const CPortData);
        assert!(rc == 0, "[stream] push #{} 失败: rc={}", i, rc);
        println!("[stream] push #{}: rc={}, token={:?}", i, rc, tok);

        // 每次 push 后 next 一次，打印增量 DSL
        println!("-- next #{} (after push) --", i + 1);
        drain_next_once(handle);
    }

    // ---- push_end ----
    let rc = execute_operator_stream_push_end(handle);
    println!("[stream] push_end: rc={}", rc);
    assert!(rc == 0);

    // ---- next 一次 (status=done) ----
    println!("-- next (after push_end, should be done) --");
    drain_next_once(handle);

    // ---- 再 next 一次应返回 EOF=1 ----
    println!("-- next (should be EOF=1) --");
    let mut out: CPortData = CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: ptr::null_mut() } };
    let rc = execute_operator_stream_next(handle, &mut out as *mut CPortData);
    println!("[stream] next rc={}", rc);
    if out.type_tag != TYPE_NULL {
        release_port_data(&mut out as *mut CPortData);
    }

    // ---- end ----
    execute_operator_stream_end(handle);
    println!("[stream] stream_end ok");
}

fn drain_next_once(handle: *mut std::ffi::c_void) {
    let mut out: CPortData = CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: ptr::null_mut() } };
    let rc = execute_operator_stream_next(handle, &mut out as *mut CPortData);
    println!("[stream] next rc={}", rc);
    if rc == 0 && out.type_tag != TYPE_NULL {
        let pd = unsafe { portdata_from_c(&mut out as *mut CPortData) };
        if let PortData::String(s) = pd {
            println!("--- DSL ({} chars) ---\n{}", s.chars().count(), s);
        } else {
            println!("(非 String 输出: {:?})", pd.type_name());
        }
    } else {
        // 释放
        if out.type_tag != TYPE_NULL {
            release_port_data(&mut out as *mut CPortData);
        }
    }
}

fn main() {
    // =================== 批量模式 ===================
    println!("################################################");
    println!("##########     批量模式测试             ##########");
    println!("################################################");

    println!("\n=== 批量 1: prompt 端口 + tokens 端口 + 默认参数 ===");
    if let Some(PortData::String(dsl)) = run_batch(
        Some("你好，帮我用 Rust 写 hello world"),
        Some("fn main() {\n    println!(\"Hello, world!\");\n}"),
        r#"{"title":"示例对话"}"#,
    ) {
        println!("--- DSL ---\n{}", dsl);
    }

    println!("\n=== 批量 2: 无 prompt，只用 tokens + debug_log 无效果（批量不触发日志）===");
    if let Some(PortData::String(dsl)) = run_batch(
        None,
        Some("直接的回答文本"),
        r#"{}"#,
    ) {
        println!("--- DSL ---\n{}", dsl);
    }

    // =================== 流式模式 ===================
    println!("\n\n################################################");
    println!("##########     流式模式测试             ##########");
    println!("################################################");

    println!("\n=== 流式 1: prompt + 分块 tokens + debug_log 打印到 stderr ===");
    run_stream(
        Some("介绍 Rust 的所有权"),
        &[
            "Rust 的所",
            "有权规则保证",
            "了内存安全，",
            "无需 GC。\n",
            "核心规则：每",
            "个值只有一",
            "个所有者，",
            "所有者离开",
            "作用域时值",
            "被释放。",
        ],
        r#"{"title":"Rust 所有权","debug_log":true}"#,
    );

    println!("\n=== 流式 2: 无 prompt（仅 assistant） ===");
    run_stream(
        None,
        &["Hel", "lo, ", "wor", "ld!"],
        r#"{"title":"无 user 对话"}"#,
    );
}
