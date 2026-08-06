use libloading::{Library, Symbol};

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use operator_executor_client::{PortData, DataFrame};

use crate::config::get_compile_directory;
use crate::operator_executor::cargo_project_build;
use operator_executor_client::ensure_runtime_loaded;

/// Rust 原生算子执行函数签名，直接传递 PortData 指针
/// 
/// 约定:
/// - inputs: 输入 PortData 指针数组，以 NULL 指针结尾
/// - outputs: 输出 PortData 指针数组（调用方预分配并以 NULL 初始化，算子按序写入，NULL 结束标记）
/// - params_json: 参数 JSON 字符串（C 字符串）
type ExecuteOperatorFn = unsafe extern "C" fn(
    inputs: *const *const PortData,
    outputs: *mut *mut PortData,
    params_json: *const std::os::raw::c_char,
) -> i32;

/// Debug 模式下的详细诊断信息。
#[derive(Clone, Debug)]
pub struct DebugDiagnostics {
    /// 整体是否成功 (编译并执行均通过)
    pub success: bool,
    /// rustc 编译耗时 (毫秒)
    pub compile_duration_ms: u128,
    /// `execute_operator` 调用耗时 (毫秒)
    pub execute_duration_ms: u128,
    /// rustc 的标准输出
    pub rustc_stdout: String,
    /// rustc 的标准错误 (含警告/错误)
    pub rustc_stderr: String,
    /// 生成的动态库路径
    pub lib_path: Option<PathBuf>,
    /// 动态库文件大小 (字节)
    pub lib_size_bytes: Option<u64>,
    /// 保留的临时目录路径 (含 lib.rs 与动态库)
    pub temp_dir: Option<PathBuf>,
    /// 实际传入的输入数据
    pub inputs: Vec<PortData>,
    /// 执行结果输出 (成功时)
    pub outputs: Option<PortData>,
    /// 失败时的错误描述
    pub error: Option<String>,
}

/// 以 Debug 模式编译并执行自定义算子代码，返回详细诊断信息。
pub fn compile_and_execute_debug(code: &str, inputs: Vec<PortData>, algorithm_name: &str) -> DebugDiagnostics {
    let mut diag = DebugDiagnostics {
        success: false,
        compile_duration_ms: 0,
        execute_duration_ms: 0,
        rustc_stdout: String::new(),
        rustc_stderr: String::new(),
        lib_path: None,
        lib_size_bytes: None,
        temp_dir: None,
        inputs,
        outputs: None,
        error: None,
    };

    // 确保运行时已加载
    if let Err(e) = ensure_runtime_loaded() {
        diag.error = Some(e);
        return diag;
    }

    let compile_start = Instant::now();
    
    // 使用 cargo build 编译临时项目，实现真正的动态链接
    // debug 模式编译，便于调试
    let compile_base_dir = get_compile_directory();
    let build_result = cargo_project_build(code, algorithm_name, &compile_base_dir, true, "build_debug");
    
    diag.compile_duration_ms = compile_start.elapsed().as_millis();
    diag.rustc_stdout = build_result.stdout;
    diag.rustc_stderr = build_result.stderr;
    diag.temp_dir = build_result.temp_dir;

    if !build_result.success {
        diag.error = build_result.error;
        return diag;
    }

    let output_path = match build_result.lib_path {
        Some(p) => p,
        None => {
            diag.error = Some("编译成功但未找到输出文件".to_string());
            return diag;
        }
    };

    // 记录动态库产物信息
    diag.lib_path = Some(output_path.clone());
    diag.lib_size_bytes = fs::metadata(&output_path).ok().map(|m| m.len());

    // 加载动态库
    let lib = match unsafe { Library::new(&output_path) } {
        Ok(l) => l,
        Err(e) => {
            diag.error = Some(format!("加载动态库失败: {}", e));
            return diag;
        }
    };

    let exec_start = Instant::now();
    
    // 获取 execute_operator 函数
    let execute_fn: Symbol<ExecuteOperatorFn> = match unsafe { lib.get(b"execute_operator") } {
        Ok(f) => f,
        Err(e) => {
            diag.error = Some(format!("找不到 execute_operator 函数: {}", e));
            diag.execute_duration_ms = exec_start.elapsed().as_millis();
            return diag;
        }
    };

    // 空输入时给一个默认占位输入
    let prepared_inputs: Vec<PortData> = if diag.inputs.is_empty() {
        vec![PortData::DataFrame(DataFrame::from_f64_vec("value", vec![1.0, 2.0, 3.0, 4.0, 5.0]))]
    } else {
        diag.inputs.clone()
    };

    // 将输入 PortData 转换为指针数组（以 NULL 结尾）
    let mut input_ptrs: Vec<*const PortData> = prepared_inputs
        .iter()
        .map(|d| d as *const PortData)
        .collect();
    input_ptrs.push(std::ptr::null());

    // 准备输出缓冲区（预分配，以 NULL 初始化，+1 给算子写 NULL 结束标记）
    let capacity = 2;
    let mut output_ptrs: Vec<*mut PortData> = vec![std::ptr::null_mut(); capacity];

    // 参数通过代码注入，这里传递空 JSON
    let params_cstr = match std::ffi::CString::new("") {
        Ok(c) => c,
        Err(e) => {
            diag.error = Some(format!("参数 JSON 转换失败: {}", e));
            diag.execute_duration_ms = exec_start.elapsed().as_millis();
            return diag;
        }
    };
    let params_ptr = params_cstr.as_ptr();

    // 调用函数
    let result = unsafe {
        execute_fn(
            input_ptrs.as_ptr(),
            output_ptrs.as_mut_ptr(),
            params_ptr,
        )
    };
    
    diag.execute_duration_ms = exec_start.elapsed().as_millis();

    if result != 0 {
        // 释放已分配的输出 PortData
        for ptr in output_ptrs {
            if !ptr.is_null() {
                unsafe {
                    let _ = Box::from_raw(ptr);
                }
            }
        }
        diag.error = Some(format!("执行失败，返回码: {}", result));
    } else {
        // 将输出指针转换为 PortData
        if !output_ptrs[0].is_null() {
            let output_data = unsafe { *Box::from_raw(output_ptrs[0]) };
            diag.outputs = Some(output_data);
        }
        diag.success = true;
    }

    // 同步实际使用的输入 (空输入被替换的情况)
    if diag.inputs.is_empty() {
        diag.inputs = vec![PortData::DataFrame(DataFrame::from_f64_vec("value", vec![1.0, 2.0, 3.0, 4.0, 5.0]))];
    }

    diag
}

/// 把用户在 Debug 输入框里填写的文本解析为多路输入数据。
///
/// 格式：
/// - 多个输入流之间用 `;` 分隔；
/// - 每个输入流内部用 `,` 分隔浮点数；
/// - 例如 `1,2,3,4,5` 表示单路 5 个点；`1,2,3;4,5,6` 表示两路输入。
pub fn parse_debug_inputs(text: &str) -> Result<Vec<PortData>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("输入为空".to_string());
    }

    text.split(';')
        .map(|stream| {
            let stream = stream.trim();
            if stream.is_empty() {
                return Err("存在空的输入流 (连续的 ';')".to_string());
            }
            let nums: Result<Vec<f64>, String> = stream
                .split(',')
                .map(|s| {
                    s.trim().parse::<f64>().map_err(|e| {
                        format!("无法解析 '{}': {}", s.trim(), e)
                    })
                })
                .collect();
            nums.map(|v| PortData::DataFrame(DataFrame::from_f64_vec("value", v)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_executor_client::{PortData, DataFrame};

    #[test]
    fn parse_debug_inputs_single_stream() {
        let parsed = parse_debug_inputs("1, 2, 3, 4, 5").unwrap();
        let expected = PortData::DataFrame(DataFrame::from_f64_vec("value", vec![1.0, 2.0, 3.0, 4.0, 5.0]));
        assert_eq!(parsed, vec![expected]);
    }

    #[test]
    fn parse_debug_inputs_multi_stream() {
        let parsed = parse_debug_inputs("1,2,3;4,5,6").unwrap();
        let expected1 = PortData::DataFrame(DataFrame::from_f64_vec("value", vec![1.0, 2.0, 3.0]));
        let expected2 = PortData::DataFrame(DataFrame::from_f64_vec("value", vec![4.0, 5.0, 6.0]));
        assert_eq!(parsed, vec![expected1, expected2]);
    }

    #[test]
    fn parse_debug_inputs_rejects_empty() {
        assert!(parse_debug_inputs("").is_err());
        assert!(parse_debug_inputs("   ").is_err());
    }

    #[test]
    fn parse_debug_inputs_rejects_invalid_number() {
        assert!(parse_debug_inputs("1, abc, 3").is_err());
    }

    #[test]
    fn parse_debug_inputs_rejects_empty_stream() {
        assert!(parse_debug_inputs("1,2;;3,4").is_err());
    }
}
