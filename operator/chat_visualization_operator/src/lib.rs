//! DSL 流式对话展示算子
//!
//! 作为 ollama 流式对话节点的下游，接收逐 token 流式输入并拼接为
//! `chat "标题" { user "..."; assistant "..."; status streaming; token_count N }`
//! 格式的 DSL 文本输出。导出 5 个流式 C ABI 符号和批量 `execute_operator` 兜底。
//!
//! ## 设计要点
//!
//! - **双输入端口**：端口 `[0]` 为可选 `prompt`（用户问题，String）；端口 `[1]` 为
//!   流式 `tokens`（助手 token chunk，String）。`prompt` 优先用输入端口，其次参数。
//! - **增量 DSL 快照流式输出**：`stream_next` 在每次 `push` 后（`dirty=true`）产出一份
//!   完整 DSL 快照（`status=streaming` + 当前累积 assistant 文本），服务端通过
//!   `StreamChunk` 帧实时推送到前端，前端重读缓存即可看到「打字机」逐 token 效果。
//!   `push_end` 后产出最终 DSL（`status=done`）。服务端 `aggregate_chunks` 对 DSL 快照
//!   型 String chunk 采用「保留最后一份」策略（非拼接），保证最终缓存是单份合法 DSL。
//! - **状态机**：`status` ∈ { `streaming`（上游未 EOF）, `done`（上游 EOF）, `error` }
//! - **stderr 调试日志**：参数 `debug_log=true` 时，每个到达的 token 以
//!   `[chat_op] token(N)=xxx` 格式写入服务端 stderr，便于排查上游是否真的在推数据。

use std::ffi::{CStr, CString, c_char, c_void};

use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::PortData;
use operator_runtime::c_abi::{
    CPortData, CPortValue, c_set_last_error, portdata_from_c, portdata_to_c_owned, TYPE_NULL,
};
use serde::{Deserialize, Serialize};

// ===== 参数 =====

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ChatParams {
    /// 聊天窗口标题
    #[serde(default)]
    pub title: String,
    /// 用户问题（prompt 输入端口为空时兜底）
    #[serde(default)]
    pub prompt: String,
    /// user 气泡标签
    #[serde(default)]
    pub user_label: String,
    /// assistant 气泡标签
    #[serde(default)]
    pub assistant_label: String,
    /// 是否打印 token 到 stderr（调试用）
    #[serde(default)]
    pub debug_log: Option<bool>,
}

fn apply_defaults(mut p: ChatParams) -> ChatParams {
    if p.title.is_empty() {
        p.title = "对话".to_string();
    }
    if p.user_label.is_empty() {
        p.user_label = "用户".to_string();
    }
    if p.assistant_label.is_empty() {
        p.assistant_label = "助手".to_string();
    }
    if p.debug_log.is_none() {
        p.debug_log = Some(false);
    }
    p
}

fn parse_params(json: &str) -> ChatParams {
    if json.is_empty() {
        return apply_defaults(ChatParams::default());
    }
    match serde_json::from_str::<ChatParams>(json) {
        Ok(p) => apply_defaults(p),
        Err(e) => {
            eprintln!("[chat_op] 解析参数 JSON 失败: {}，使用默认值", e);
            apply_defaults(ChatParams::default())
        }
    }
}

// ===== 错误设置辅助 =====

fn set_err(msg: &str) {
    eprintln!("[chat_op] {}", msg);
    let c = CString::new(msg).unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

fn clear_err() {
    let c = CString::new("").unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

// ===== DSL 生成 =====

/// 字符串字面量转义（双引号包裹 + 反斜杠转义内部 `"`/`\`/换行/tab），保证前端
/// 词法器的 `lex_string` 能精确还原。
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// DSL 状态关键字（不允许字符串，直接作为 identifier 输出，解析器按 ident 识别更稳）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatStatus {
    Streaming,
    Done,
    Error,
}

impl ChatStatus {
    fn as_str(self) -> &'static str {
        match self {
            ChatStatus::Streaming => "streaming",
            ChatStatus::Done => "done",
            ChatStatus::Error => "error",
        }
    }
}

/// 根据完整状态生成 `chat { ... }` DSL 文本。
///
/// 格式：
/// ```text
/// chat "标题" {
///   user "用户内容"      # 仅当 prompt 非空
///   assistant "助手内容"  # 可能是空串（还没开始推）
///   status streaming
///   token_count 123
/// }
/// ```
fn emit_dsl(
    title: &str,
    user: Option<&str>,
    assistant: &str,
    status: ChatStatus,
    token_count: u64,
) -> String {
    let mut out = String::new();
    out.push_str("chat ");
    out.push_str(&escape_str(title));
    out.push_str(" {\n");
    if let Some(u) = user.filter(|s| !s.is_empty()) {
        out.push_str("  user ");
        out.push_str(&escape_str(u));
        out.push('\n');
    }
    out.push_str("  assistant ");
    out.push_str(&escape_str(assistant));
    out.push('\n');
    out.push_str("  status ");
    out.push_str(status.as_str());
    out.push('\n');
    out.push_str("  token_count ");
    out.push_str(&token_count.to_string());
    out.push('\n');
    out.push_str("}\n");
    out
}

/// 从批量/启动时的输入端口解析 prompt：优先端口 [0]（String 非空），否则用参数。
fn resolve_prompt(
    inputs: *const CPortData,
    input_count: usize,
    param_prompt: &str,
) -> Option<String> {
    if input_count > 0 && !inputs.is_null() {
        // 端口 [0] 是 prompt
        let pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
        if let PortData::String(s) = &pd {
            if !s.is_empty() {
                return Some(s.clone());
            }
        }
        // 端口 [1] tokens 在 stream_start 不消费；留给 push 路径
    }
    if param_prompt.is_empty() {
        None
    } else {
        Some(param_prompt.to_string())
    }
}

// ===== 流式 handle =====

/// 流式执行的运行时状态（`stream_start` 返回的 `*mut c_void` 指向它）。
struct ChatStream {
    params: ChatParams,
    user: Option<String>,
    assistant: String,
    status: ChatStatus,
    token_count: u64,
    /// 上次 `stream_next` 产出后又有新 token 到达（`push` 设为 true，`next` 清除）。
    /// `next` 仅在 `dirty=true` 或 `status != Streaming` 且尚未 `emitted_done` 时产出 DSL。
    dirty: bool,
    /// 上游 EOF 已到达且我们已经把最终 `status=done`/`error` 的 DSL 产出过一次后，
    /// 再调 next 就返回 EOF=1。
    emitted_done: bool,
    debug_log: bool,
}

// ===== 流式 C ABI：5 个符号 =====

/// `stream_start`：解析参数与 prompt 端口，初始化流式状态。
#[no_mangle]
pub extern "C" fn execute_operator_stream_start(
    inputs: *const CPortData,
    input_count: usize,
    params_json: *const c_char,
) -> *mut c_void {
    if let Err(e) = ensure_runtime_loaded() {
        set_err(&format!("runtime 加载失败: {}", e));
        return std::ptr::null_mut();
    }

    let params_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_str);

    let user = resolve_prompt(inputs, input_count, &params.prompt);
    let debug_log = params.debug_log.unwrap_or(false);
    if debug_log {
        eprintln!(
            "[chat_op] stream_start: title={:?}, user_present={}",
            params.title,
            user.is_some()
        );
    }

    clear_err();
    let handle = Box::new(ChatStream {
        params,
        user,
        assistant: String::new(),
        status: ChatStatus::Streaming,
        token_count: 0,
        dirty: false,
        emitted_done: false,
        debug_log,
    });
    Box::into_raw(handle) as *mut c_void
}

/// `stream_push`：推入上游 chunk（来自 ollama 的 token，String）。
///
/// 累积写入到 `assistant` 缓冲。流式期间不产出 DSL（见 `stream_next`），仅更新内部
/// 累积状态；等 `push_end` 后由 `next` 一次性产出最终完整 DSL。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push(
    handle: *mut c_void,
    chunk: *const CPortData,
) -> i32 {
    if handle.is_null() {
        set_err("stream_push: handle 为 null");
        return -1;
    }
    if chunk.is_null() {
        // 空 chunk 视为 no-op，不报错
        return 0;
    }
    let hs: &mut ChatStream = unsafe { &mut *(handle as *mut ChatStream) };

    // 消费 chunk 里的 String
    let pd = unsafe { portdata_from_c(chunk as *mut CPortData) };
    let token = match pd {
        PortData::String(s) => s,
        other => {
            let msg = format!(
                "stream_push: 上游 chunk 类型错误，期望 String，实际 {}",
                other.type_name()
            );
            set_err(&msg);
            hs.status = ChatStatus::Error;
            return -2;
        }
    };

    if token.is_empty() {
        return 0; // 空串跳过
    }

    if hs.debug_log {
        eprintln!(
            "[chat_op] token({})={}",
            token.chars().count(),
            // 换行等不可见字符用 Debug 风格便于肉眼看
            format!("{:?}", token)
        );
    }

    hs.assistant.push_str(&token);
    hs.token_count = hs.token_count.saturating_add(token.chars().count() as u64);
    hs.dirty = true;
    0
}

/// `stream_push_end`：通知上游 EOF，切换到 `done` 状态。
///
/// 之后 `stream_next` 会一次性产出最终完整 DSL（`status=done`）。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push_end(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        set_err("stream_push_end: handle 为 null");
        return -1;
    }
    let hs: &mut ChatStream = unsafe { &mut *(handle as *mut ChatStream) };
    if hs.status == ChatStatus::Streaming {
        hs.status = ChatStatus::Done;
    }
    // 标记 dirty 确保即使上一个 token 已被 next 取走，push_end 后仍能产出最终 done DSL
    hs.dirty = true;
    if hs.debug_log {
        eprintln!(
            "[chat_op] push_end: 切换到 done，累计 token_count={}",
            hs.token_count
        );
    }
    0
}

/// `stream_next`：拉取最新 DSL 快照（三态返回，严格遵守 operator_sdk 约定）。
///
/// - `0`：有更新的 DSL 快照（写入 `*out_chunk`，owned String）
/// - `1`：当前暂无可读（非永久 EOF）
/// - `<0`：错误
///
/// **增量 DSL 快照流式输出**：每次 `push` 后 `dirty=true`，`next` 产出一份完整 DSL
/// 快照（`status=streaming` + 当前累积 assistant 文本）并清除 `dirty`。服务端通过
/// `StreamChunk` 帧实时推送到前端，前端重读缓存即可看到逐 token 的「打字机」效果。
///
/// `push_end` 后 `status=done` + `dirty=true`，`next` 产出最终完整 DSL（`status=done`）
/// 并设置 `emitted_done`，之后 `next` 返回 `1`（永久 EOF）。
///
/// 服务端 `aggregate_chunks` 对 DSL 快照型 String chunk 采用「保留最后一份」策略
/// （检测首 chunk 以 `chat ` 开头），保证最终缓存是单份合法 DSL 而非多份拼接。
#[no_mangle]
pub extern "C" fn execute_operator_stream_next(
    handle: *mut c_void,
    out_chunk: *mut CPortData,
) -> i32 {
    if handle.is_null() || out_chunk.is_null() {
        set_err("stream_next: handle 或 out_chunk 为 null");
        return -1;
    }
    let hs: &mut ChatStream = unsafe { &mut *(handle as *mut ChatStream) };

    // 已产出过最终 DSL：之后每次 next 都返回 EOF=1
    if hs.emitted_done {
        return 1;
    }

    // 无新数据且仍在流式中：暂无可读
    if !hs.dirty && hs.status == ChatStatus::Streaming {
        return 1;
    }

    // dirty=true 或 status=Done/Error：产出 DSL 快照
    let dsl = emit_dsl(
        &hs.params.title,
        hs.user.as_deref(),
        &hs.assistant,
        hs.status,
        hs.token_count,
    );
    let c_pd = portdata_to_c_owned(PortData::String(dsl));
    unsafe { *out_chunk = c_pd };
    hs.dirty = false;
    // status=Done/Error 时本次为最终 DSL，标记已产出
    if hs.status != ChatStatus::Streaming {
        hs.emitted_done = true;
    }
    0
}

/// `stream_end`：释放 handle 及关联资源。
#[no_mangle]
pub extern "C" fn execute_operator_stream_end(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle as *mut ChatStream));
    }
}

// ===== 批量兜底：execute_operator =====

/// 批量执行（`stream=false` 时调用）：
/// - 端口 [0] prompt（String 可选）
/// - 端口 [1] tokens（String 或空；如果是流式链被强制批量，tokens 应该是上游收集完整后
///   转成批量的大 String——如果没有 tokens 端口，则用参数中兜底不出 assistant 也行）
///
/// 一次性生成完整 DSL（status=done）。
#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        set_err(&format!("runtime 加载失败: {}", e));
        return -1;
    }

    let params_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_str);

    // 解析 prompt (端口 [0]) + tokens (端口 [1])
    let mut user: Option<String> = None;
    let mut assistant: String = String::new();

    if !inputs.is_null() {
        // 端口 [0] prompt
        if input_count > 0 {
            let pd0 = unsafe { portdata_from_c(inputs as *mut CPortData) };
            if let PortData::String(s) = &pd0 {
                if !s.is_empty() {
                    user = Some(s.clone());
                }
            }
        }
        // 端口 [1] tokens（批量模式下可能是完整的拼接文本）
        if input_count > 1 {
            let pd1 = unsafe { portdata_from_c(inputs.add(1) as *mut CPortData) };
            if let PortData::String(s) = &pd1 {
                assistant = s.clone();
            }
        }
    }
    // 参数兜底 prompt
    if user.is_none() && !params.prompt.is_empty() {
        user = Some(params.prompt.clone());
    }

    let token_count = assistant.chars().count() as u64;
    let dsl = emit_dsl(
        &params.title,
        user.as_deref(),
        &assistant,
        ChatStatus::Done,
        token_count,
    );

    clear_err();
    let port_data = PortData::String(dsl);

    if !outputs.is_null() && output_cap > 0 {
        let c_pd = portdata_to_c_owned(port_data);
        unsafe {
            *outputs = c_pd;
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
                    value: CPortValue {
                        str_ptr: std::ptr::null_mut(),
                    },
                };
            }
        }
    }

    0
}

/// 释放 C ABI PortData 内存（由调用方调用）。
#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

/// 获取本算子版本。
#[no_mangle]
pub extern "C" fn chat_visualization_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_str_handles_special_chars() {
        assert_eq!(escape_str("hi"), "\"hi\"");
        assert_eq!(escape_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(escape_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(escape_str("a\nb"), "\"a\\nb\"");
        assert_eq!(escape_str("a\rb"), "\"a\\rb\"");
        assert_eq!(escape_str("a\tb"), "\"a\\tb\"");
    }

    #[test]
    fn emit_dsl_full() {
        let dsl = emit_dsl(
            "标题",
            Some("你好，用 Rust 写一个 hello world"),
            "fn main() {\n    println!(\"hi\");\n}",
            ChatStatus::Streaming,
            42,
        );
        assert!(dsl.starts_with("chat \"标题\" {"));
        assert!(dsl.contains("user \"你好，用 Rust 写一个 hello world\""));
        assert!(dsl.contains(
            "assistant \"fn main() {\\n    println!(\\\"hi\\\");\\n}\""
        ));
        assert!(dsl.contains("status streaming"));
        assert!(dsl.contains("token_count 42"));
        assert!(dsl.trim_end().ends_with('}'));
    }

    #[test]
    fn emit_dsl_no_user() {
        let dsl = emit_dsl("对话", None, "hi", ChatStatus::Done, 2);
        assert!(!dsl.contains("user "));
        assert!(dsl.contains("assistant \"hi\""));
        assert!(dsl.contains("status done"));
    }

    #[test]
    fn parse_params_defaults() {
        let p = parse_params("");
        assert_eq!(p.title, "对话");
        assert_eq!(p.user_label, "用户");
        assert_eq!(p.assistant_label, "助手");
        assert_eq!(p.debug_log, Some(false));
    }

    #[test]
    fn parse_params_partial_json() {
        let p = parse_params(r#"{"title":"我的聊天","debug_log":true}"#);
        assert_eq!(p.title, "我的聊天");
        assert_eq!(p.debug_log, Some(true));
        assert_eq!(p.user_label, "用户"); // 默认值回退
    }

    #[test]
    fn parse_params_invalid_json_uses_default() {
        let p = parse_params("{not json");
        assert_eq!(p.title, "对话");
    }

    // ---- 流式流程测试（不调用 C ABI，直接操作 ChatStream 结构）----

    fn fresh_stream() -> ChatStream {
        ChatStream {
            params: apply_defaults(ChatParams {
                title: "T".into(),
                prompt: "Q".into(),
                user_label: "U".into(),
                assistant_label: "A".into(),
                debug_log: Some(false),
            }),
            user: Some("Q".into()),
            assistant: String::new(),
            status: ChatStatus::Streaming,
            token_count: 0,
            dirty: false,
            emitted_done: false,
            debug_log: false,
        }
    }

    #[test]
    fn stream_flow_tokens_then_done() {
        let mut s = fresh_stream();
        // 初始态（streaming）的 DSL：user + 空 assistant
        let dsl0 = emit_dsl(
            &s.params.title,
            s.user.as_deref(),
            &s.assistant,
            s.status,
            s.token_count,
        );
        assert!(dsl0.contains("user \"Q\""));
        assert!(dsl0.contains("assistant \"\""));
        assert!(dsl0.contains("status streaming"));

        // push 第一个 token → dirty=true，next 产出含该 token 的 streaming DSL 快照
        s.assistant.push_str("Hello");
        s.token_count += 5;
        s.dirty = true;
        let dsl1 = emit_dsl(
            &s.params.title,
            s.user.as_deref(),
            &s.assistant,
            s.status,
            s.token_count,
        );
        assert!(dsl1.contains("assistant \"Hello\""));
        assert!(dsl1.contains("token_count 5"));
        assert!(dsl1.contains("status streaming"));

        // push_end → done + dirty=true，next 产出最终 done DSL
        s.status = ChatStatus::Done;
        s.dirty = true;
        let dsl2 = emit_dsl(
            &s.params.title,
            s.user.as_deref(),
            &s.assistant,
            s.status,
            s.token_count,
        );
        assert!(dsl2.contains("status done"));
        assert!(dsl2.contains("assistant \"Hello\""));
    }

    #[test]
    fn status_ident_strings() {
        assert_eq!(ChatStatus::Streaming.as_str(), "streaming");
        assert_eq!(ChatStatus::Done.as_str(), "done");
        assert_eq!(ChatStatus::Error.as_str(), "error");
    }

    // ---- C ABI 回归测试：stream_next 逐 token 产出 DSL 快照，push_end 后产出最终 done DSL ----
    //
    // 验证流式输出行为：
    // 1. push 后 next 产出 streaming DSL 快照（含已累积 token）
    // 2. 再次 next（无新 push）返回 1（暂无可读）
    // 3. push_end 后 next 产出最终 done DSL
    // 4. 之后 next 返回 1（EOF）
    //
    // 这里直接构造 ChatStream 并 Box::into_raw 作为 handle，绕过 stream_start
    // （后者会 ensure_runtime_loaded，在纯 `cargo test` 环境下可能找不到 DLL）。
    // push / next / push_end / end 本身不依赖运行时 DLL，portdata_to_c/from_c 已
    // 作为 rlib 静态链接进测试二进制。

    fn null_slot() -> CPortData {
        CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        }
    }

    /// 由 fresh_stream 派生一个裸 handle，可选覆盖 user。
    fn make_handle(user: Option<&str>) -> *mut c_void {
        let mut s = fresh_stream();
        s.user = user.map(|u| u.to_string());
        Box::into_raw(Box::new(s)) as *mut c_void
    }

    #[test]
    fn stream_next_emits_snapshot_per_token_then_final_done() {
        use operator_runtime::c_abi::portdata_to_c;

        let handle = make_handle(Some("Q"));

        // push "Hel" → next 应产出 streaming DSL 快照（assistant="Hel"）
        let tok_pd = PortData::String("Hel".to_string());
        let c = portdata_to_c(&tok_pd);
        assert_eq!(execute_operator_stream_push(handle, &c as *const CPortData), 0);
        let mut c_mut = c;
        release_port_data(&mut c_mut as *mut CPortData);

        let mut out = null_slot();
        assert_eq!(execute_operator_stream_next(handle, &mut out as *mut CPortData), 0,
            "push 后 next 必须返回 0（有 chunk）");
        let pd = unsafe { portdata_from_c(&mut out as *mut CPortData) };
        if let PortData::String(dsl) = &pd {
            assert!(dsl.contains("assistant \"Hel\""), "首 token DSL: {}", dsl);
            assert!(dsl.contains("status streaming"), "应为 streaming: {}", dsl);
            assert!(dsl.contains("token_count 3"), "{}", dsl);
        } else {
            panic!("期望 String 输出");
        }

        // 再次 next（无新 push）→ 返回 1（暂无可读）
        let mut out2 = null_slot();
        assert_eq!(execute_operator_stream_next(handle, &mut out2 as *mut CPortData), 1,
            "无新 push 时 next 应返回 1");

        // push "lo" → next 产出更新后的 streaming DSL（assistant="Hello"）
        let tok_pd = PortData::String("lo".to_string());
        let c = portdata_to_c(&tok_pd);
        assert_eq!(execute_operator_stream_push(handle, &c as *const CPortData), 0);
        let mut c_mut = c;
        release_port_data(&mut c_mut as *mut CPortData);

        let mut out3 = null_slot();
        assert_eq!(execute_operator_stream_next(handle, &mut out3 as *mut CPortData), 0);
        let pd3 = unsafe { portdata_from_c(&mut out3 as *mut CPortData) };
        if let PortData::String(dsl) = &pd3 {
            assert!(dsl.contains("assistant \"Hello\""), "第二 token DSL: {}", dsl);
            assert!(dsl.contains("status streaming"), "{}", dsl);
            assert!(dsl.contains("token_count 5"), "{}", dsl);
        } else {
            panic!("期望 String 输出");
        }

        // push_end → next 产出最终 done DSL
        assert_eq!(execute_operator_stream_push_end(handle), 0);
        let mut out4 = null_slot();
        assert_eq!(execute_operator_stream_next(handle, &mut out4 as *mut CPortData), 0);
        let pd4 = unsafe { portdata_from_c(&mut out4 as *mut CPortData) };
        match pd4 {
            PortData::String(dsl) => {
                assert!(dsl.contains("user \"Q\""), "DSL 缺 user: {}", dsl);
                assert!(dsl.contains("assistant \"Hello\""), "DSL 缺完整 assistant: {}", dsl);
                assert!(dsl.contains("status done"), "DSL 状态非 done: {}", dsl);
                assert!(dsl.contains("token_count 5"), "DSL token_count 错误: {}", dsl);
            }
            other => panic!("期望 String 输出，实际 {}", other.type_name()),
        }

        // 再 next：应返回 1（EOF，最终 DSL 只产出一次）
        let mut out5 = null_slot();
        assert_eq!(execute_operator_stream_next(handle, &mut out5 as *mut CPortData), 1);

        execute_operator_stream_end(handle);
    }

    #[test]
    fn stream_next_emits_empty_done_when_no_tokens() {
        // 上游立即 EOF（无 token）：push_end 后 next 仍应产出一份合法 DSL（空 assistant + done）
        let handle = make_handle(None);

        assert_eq!(execute_operator_stream_push_end(handle), 0);

        let mut out = null_slot();
        assert_eq!(execute_operator_stream_next(handle, &mut out as *mut CPortData), 0);
        let pd = unsafe { portdata_from_c(&mut out as *mut CPortData) };
        if let PortData::String(dsl) = pd {
            assert!(!dsl.contains("user "), "无 user 时不应输出 user 行: {}", dsl);
            assert!(dsl.contains("assistant \"\""), "{}", dsl);
            assert!(dsl.contains("status done"), "{}", dsl);
            assert!(dsl.contains("token_count 0"), "{}", dsl);
        } else {
            panic!("期望 String 输出");
        }

        let mut out2 = null_slot();
        assert_eq!(execute_operator_stream_next(handle, &mut out2 as *mut CPortData), 1);
        execute_operator_stream_end(handle);
    }
}
