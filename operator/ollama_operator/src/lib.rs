//! ollama 流式对话算子
//!
//! 对接本地 ollama 服务，逐 token 流式输出 LLM 生成内容。导出 5 个流式 C ABI 符号
//! （`execute_operator_stream_start/push/push_end/next/end`），同时导出批量
//! `execute_operator` 作为 `stream=false` 时的兜底（收集全部 token 后一次性返回）。
//!
//! ## 设计要点
//!
//! - **HTTP/1.0 + 纯 `std::net::TcpStream`**：无外部 HTTP 依赖（reqwest/ureq），
//!   DLL 体积小。HTTP/1.0 不使用 chunked 编码，服务端流式写出后 `Connection: close`
//!   关闭，客户端逐行读取 NDJSON 直至 EOF。
//! - **prompt 来源**：优先取输入端口 `[0]`（`PortData::String`），为空则用参数 `prompt`。
//! - **源算子定位**：作为流式链头（head），不支持流式上游输入（`push` 返回错误）。
//! - **三态 `next`**：`0`=有 token chunk（写入 `*out_chunk`）；`1`=EOF（生成结束）；
//!   `<0`=错误。

use std::ffi::{CStr, CString, c_char, c_void};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::PortData;
use operator_runtime::c_abi::{
    CPortData, CPortValue, c_set_last_error, portdata_from_c, portdata_to_c_owned, TYPE_NULL,
};
use serde::{Deserialize, Serialize};

// ===== 参数 =====

/// ollama 算子参数（由 `params_json` 反序列化；字段名与 `operator.json` 的 Param 项一致）。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct OllamaParams {
    /// ollama 服务地址（IP:Port），默认 `127.0.0.1:11434`
    #[serde(default)]
    pub host: String,
    /// 模型名（必填，需已在 ollama 中 pull）
    #[serde(default)]
    pub model: String,
    /// 提示词（输入端口为空时使用）
    #[serde(default)]
    pub prompt: String,
    /// 系统提示词（可选）
    #[serde(default)]
    pub system: String,
    /// 采样温度（可选）
    #[serde(default)]
    pub temperature: Option<f64>,
    /// 连接/读取超时（秒），默认 300
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn apply_defaults(mut p: OllamaParams) -> OllamaParams {
    if p.host.is_empty() {
        p.host = "127.0.0.1:11434".to_string();
    }
    if p.timeout_secs.is_none() {
        p.timeout_secs = Some(300);
    }
    p
}

/// 解析参数 JSON；空串或非法 JSON 返回带默认值的参数。
fn parse_params(json: &str) -> OllamaParams {
    if json.is_empty() {
        return apply_defaults(OllamaParams::default());
    }
    match serde_json::from_str::<OllamaParams>(json) {
        Ok(p) => apply_defaults(p),
        Err(e) => {
            eprintln!("[ollama_operator] 解析参数 JSON 失败: {}，使用默认值", e);
            apply_defaults(OllamaParams::default())
        }
    }
}

// ===== 错误设置辅助 =====

/// 设置最近一次错误（同时打印到 stderr 便于诊断）。
fn set_err(msg: &str) {
    eprintln!("[ollama_operator] {}", msg);
    let c = CString::new(msg).unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

/// 清空最近一次错误（成功路径调用）。
fn clear_err() {
    let c = CString::new("").unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

// ===== HTTP 请求 =====

/// 构造 `/api/generate` 请求体（`stream: true`）。用 `serde_json::Value` 拼装以正确转义。
fn build_request_body(params: &OllamaParams, prompt: &str) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("model".into(), serde_json::Value::String(params.model.clone()));
    obj.insert("prompt".into(), serde_json::Value::String(prompt.to_string()));
    obj.insert("stream".into(), serde_json::Value::Bool(true));
    if !params.system.is_empty() {
        obj.insert("system".into(), serde_json::Value::String(params.system.clone()));
    }
    if let Some(t) = params.temperature {
        obj.insert("options".into(), serde_json::json!({ "temperature": t }));
    }
    serde_json::Value::Object(obj).to_string()
}

/// 建立 TCP 连接、发送 HTTP/1.0 POST、跳过响应头，返回 body 的缓冲读取器。
///
/// 失败返回错误字符串（调用方负责 `set_err`）。
fn send_request(params: &OllamaParams, prompt: &str) -> Result<BufReader<TcpStream>, String> {
    let timeout = Duration::from_secs(params.timeout_secs.unwrap_or(300));

    let addr = match params.host.to_socket_addrs() {
        Ok(mut a) => a
            .next()
            .ok_or_else(|| format!("无法解析地址: {}", params.host))?,
        Err(e) => return Err(format!("地址解析失败 {}: {}", params.host, e)),
    };

    let stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("连接 ollama {} 失败: {}", params.host, e))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("设置读超时失败: {}", e))?;
    stream.set_nodelay(true).ok();

    let body = build_request_body(params, prompt);
    let request = format!(
        "POST /api/generate HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        params.host,
        body.len(),
        body
    );

    let mut stream = stream;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("发送请求失败: {}", e))?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    skip_http_headers(&mut reader)?;
    Ok(reader)
}

/// 读取并丢弃 HTTP 响应头（直到空行）。校验状态行必须为 200，否则报错。
fn skip_http_headers<R: BufRead>(reader: &mut R) -> Result<(), String> {
    let mut first = true;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("读取响应头失败: {}", e))?;
        if n == 0 {
            return Ok(()); // EOF（无 body）
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if first {
            first = false;
            // 状态行形如 "HTTP/1.0 200 OK"
            if !trimmed.starts_with("HTTP/") {
                return Err(format!("非法响应状态行: {}", trimmed));
            }
            let status = trimmed.split_whitespace().nth(1).unwrap_or("");
            if status != "200" {
                let mut body = String::new();
                let mut bl = String::new();
                loop {
                    bl.clear();
                    match reader.read_line(&mut bl) {
                        Ok(0) => break,
                        Ok(_) => body.push_str(&bl),
                        Err(_) => break,
                    }
                }
                return Err(format!(
                    "ollama 返回 HTTP {}: {}",
                    status,
                    body.trim()
                ));
            }
        }
        if trimmed.is_empty() {
            return Ok(()); // 空行 = 头结束
        }
    }
}

/// 从 NDJSON body 读取下一个 token。
///
/// - `Ok(Some(token))`：有 chunk
/// - `Ok(None)`：EOF 或 `done: true`（生成结束）
/// - `Err`：解析/IO 错误
fn read_next_token<R: BufRead>(reader: &mut R) -> Result<Option<String>, String> {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(None), // EOF
            Ok(_) => {
                let s = line.trim_end_matches(['\r', '\n']);
                if s.is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(s) {
                    Ok(v) => v,
                    Err(e) => return Err(format!("解析 ollama 响应行失败: {} | {}", e, s)),
                };
                let token = v.get("response").and_then(|x| x.as_str()).unwrap_or("");
                if token.is_empty() {
                    // 空 token：通常是终止行（done:true, response:""）
                    let done = v.get("done").and_then(|x| x.as_bool()).unwrap_or(false);
                    if done {
                        return Ok(None);
                    }
                    continue; // 跳过空 token（罕见）
                }
                return Ok(Some(token.to_string()));
            }
            Err(e) => return Err(format!("读取 ollama 响应失败: {}", e)),
        }
    }
}

/// 批量读取全部 token 并拼接为完整字符串（`execute_operator` 兜底使用）。
fn read_all_tokens<R: BufRead>(reader: &mut R) -> Result<String, String> {
    let mut full = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let s = line.trim_end_matches(['\r', '\n']);
                if s.is_empty() {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(s) {
                    Ok(v) => v,
                    Err(e) => return Err(format!("解析 ollama 响应行失败: {} | {}", e, s)),
                };
                if let Some(tok) = v.get("response").and_then(|x| x.as_str()) {
                    full.push_str(tok);
                }
            }
            Err(e) => return Err(format!("读取 ollama 响应失败: {}", e)),
        }
    }
    Ok(full)
}

/// 解析 prompt：优先输入端口 `[0]`（String 且非空），否则用参数 `prompt`。
fn resolve_prompt(
    inputs: *const CPortData,
    input_count: usize,
    params: &OllamaParams,
) -> Result<String, String> {
    if input_count > 0 && !inputs.is_null() {
        // portdata_from_c 消费（take）该 CPortData 的 owned 数据并置 NULL
        let pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
        if let PortData::String(s) = &pd {
            if !s.is_empty() {
                return Ok(s.clone());
            }
        }
        // 非 String 或空 → 回退到参数
    }
    if params.prompt.is_empty() {
        return Err("缺少 prompt（请用输入端口或参数 prompt 提供提示词）".to_string());
    }
    Ok(params.prompt.clone())
}

// ===== 流式 handle =====

/// 流式执行的运行时状态（`stream_start` 返回的 `*mut c_void` 指向它）。
struct OllamaStream {
    /// ollama HTTP 响应 body 的缓冲读取器
    reader: BufReader<TcpStream>,
    /// 是否已到 EOF（避免 `next` 在结束后再次读已关闭的连接）
    eof: bool,
}

// ===== 流式 C ABI：5 个符号 =====

/// `stream_start`：解析参数与 prompt，建立到 ollama 的流式连接。
///
/// 返回不透明 handle（null = 失败，用 `c_get_last_error` 取详情）。
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

    if params.model.is_empty() {
        set_err("缺少必需参数 model（模型名）");
        return std::ptr::null_mut();
    }

    let prompt = match resolve_prompt(inputs, input_count, &params) {
        Ok(p) => p,
        Err(e) => {
            set_err(&e);
            return std::ptr::null_mut();
        }
    };

    let reader = match send_request(&params, &prompt) {
        Ok(r) => r,
        Err(e) => {
            set_err(&e);
            return std::ptr::null_mut();
        }
    };

    clear_err();
    let handle = Box::new(OllamaStream { reader, eof: false });
    Box::into_raw(handle) as *mut c_void
}

/// `stream_push`：推入上游 chunk。本算子为源（head），不支持流式上游输入。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push(
    _handle: *mut c_void,
    _chunk: *const CPortData,
) -> i32 {
    set_err("ollama 算子不支持流式上游输入（请用批量输入端口或参数 prompt 提供提示词）");
    -1
}

/// `stream_push_end`：通知上游 EOF。源算子无上游，no-op。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push_end(_handle: *mut c_void) -> i32 {
    0
}

/// `stream_next`：拉取下一个 token chunk（三态返回）。
///
/// - `0`：有 chunk（已写入 `*out_chunk`，owned String）
/// - `1`：EOF（生成结束，非永久暂无的语义不适用于源算子）
/// - `<0`：错误
#[no_mangle]
pub extern "C" fn execute_operator_stream_next(
    handle: *mut c_void,
    out_chunk: *mut CPortData,
) -> i32 {
    if handle.is_null() || out_chunk.is_null() {
        set_err("stream_next: handle 或 out_chunk 为 null");
        return -1;
    }
    let hs: &mut OllamaStream = unsafe { &mut *(handle as *mut OllamaStream) };
    if hs.eof {
        return 1;
    }

    match read_next_token(&mut hs.reader) {
        Ok(Some(token)) => {
            let c_pd = portdata_to_c_owned(PortData::String(token));
            unsafe { *out_chunk = c_pd };
            0
        }
        Ok(None) => {
            hs.eof = true;
            1
        }
        Err(e) => {
            set_err(&e);
            -1
        }
    }
}

/// `stream_end`：释放 handle 及关联资源（关闭 TCP 连接）。
#[no_mangle]
pub extern "C" fn execute_operator_stream_end(handle: *mut c_void) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle as *mut OllamaStream));
    }
}

// ===== 批量兜底：execute_operator =====

/// 批量执行（`stream=false` 时调用）：收集全部 token 后一次性返回完整字符串。
///
/// 返回：`0`=成功；`<0`=失败（用 `c_get_last_error` 取详情）。
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

    if params.model.is_empty() {
        set_err("缺少必需参数 model（模型名）");
        return -2;
    }

    let prompt = match resolve_prompt(inputs, input_count, &params) {
        Ok(p) => p,
        Err(e) => {
            set_err(&e);
            return -3;
        }
    };

    let mut reader = match send_request(&params, &prompt) {
        Ok(r) => r,
        Err(e) => {
            set_err(&e);
            return -4;
        }
    };

    let full = match read_all_tokens(&mut reader) {
        Ok(s) => s,
        Err(e) => {
            set_err(&e);
            return -5;
        }
    };

    clear_err();
    let port_data = PortData::String(full);

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

/// 获取 ollama 算子版本。
#[no_mangle]
pub extern "C" fn ollama_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}
