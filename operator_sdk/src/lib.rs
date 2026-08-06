use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Once, RwLock};

use libloading::{Library, Symbol};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_to_c, portdata_from_c,
    c_pd_array_new, c_pd_array_push, c_pd_array_len, c_pd_free, TYPE_NULL,
};
use operator_runtime::PortData;
use serde::{Deserialize, Serialize};

// ===== operator_runtime.dll 错误信息获取 API =====

type GetLastErrorFn = unsafe extern "C" fn() -> *mut std::os::raw::c_char;
type FreeLastErrorFn = unsafe extern "C" fn(*mut std::os::raw::c_char);

struct RuntimeErrorApi {
    get_last_error: GetLastErrorFn,
    free_last_error: FreeLastErrorFn,
}

unsafe impl Send for RuntimeErrorApi {}
unsafe impl Sync for RuntimeErrorApi {}

static RUNTIME_ERROR_API: Once = Once::new();
static mut RUNTIME_ERROR_API_INSTANCE: Option<RuntimeErrorApi> = None;

fn load_runtime_error_api() -> Result<&'static RuntimeErrorApi, String> {
    unsafe {
        RUNTIME_ERROR_API.call_once(|| {
            let runtime_path = match find_runtime_dll() {
                Some(p) => p,
                None => {
                    RUNTIME_ERROR_API_INSTANCE = None;
                    return;
                }
            };

            match Library::new(&runtime_path) {
                Ok(lib) => {
                    let get_fn: GetLastErrorFn = match lib.get(b"c_get_last_error") {
                        Ok(f) => *f,
                        Err(e) => {
                            eprintln!("加载 c_get_last_error 符号失败: {}", e);
                            RUNTIME_ERROR_API_INSTANCE = None;
                            return;
                        }
                    };
                    let free_fn: FreeLastErrorFn = match lib.get(b"c_last_error_free") {
                        Ok(f) => *f,
                        Err(e) => {
                            eprintln!("加载 c_last_error_free 符号失败: {}", e);
                            RUNTIME_ERROR_API_INSTANCE = None;
                            return;
                        }
                    };
                    std::mem::forget(lib);
                    RUNTIME_ERROR_API_INSTANCE = Some(RuntimeErrorApi {
                        get_last_error: get_fn,
                        free_last_error: free_fn,
                    });
                }
                Err(e) => {
                    eprintln!("加载 operator_runtime DLL 失败（错误API）: {}", e);
                    RUNTIME_ERROR_API_INSTANCE = None;
                }
            }
        });

        let ptr = std::ptr::addr_of!(RUNTIME_ERROR_API_INSTANCE);
        match &*ptr {
            Some(api) => Ok(api),
            None => Err("无法加载 operator_runtime 错误信息 API".to_string()),
        }
    }
}

fn get_runtime_last_error() -> Option<String> {
    // 策略：优先从 cdylib 版本获取（算子通过 prefer-dynamic 链接的版本），
    // 再尝试 rlib 版本作为兜底。两者使用全局 Mutex 存储，确保同一实例内可传递。

    // 1. 优先尝试 cdylib 版本（算子实际使用的 operator_runtime.dll 实例）
    match load_runtime_error_api() {
        Ok(api) => {
            unsafe {
                let err_ptr = (api.get_last_error)();
                if !err_ptr.is_null() {
                    let msg = CStr::from_ptr(err_ptr).to_string_lossy().into_owned();
                    (api.free_last_error)(err_ptr);
                    eprintln!("[error_debug] cdylib c_get_last_error -> {:?}", msg);
                    if !msg.is_empty() {
                        return Some(msg);
                    }
                } else {
                    eprintln!("[error_debug] cdylib c_get_last_error returned null");
                }
            }
        }
        Err(e) => {
            eprintln!("[error_debug] load_runtime_error_api 失败: {}", e);
        }
    }

    // 2. 兜底：尝试 rlib 版本（operator_sdk 直接链接的 operator_runtime 实例）
    unsafe {
        let err_ptr = operator_runtime::c_abi::c_get_last_error();
        if !err_ptr.is_null() {
            let msg = CStr::from_ptr(err_ptr).to_string_lossy().into_owned();
            operator_runtime::c_abi::c_last_error_free(err_ptr);
            eprintln!("[error_debug] rlib c_get_last_error -> {:?}", msg);
            if !msg.is_empty() {
                return Some(msg);
            }
        } else {
            eprintln!("[error_debug] rlib c_get_last_error returned null");
        }
    }

    None
}

/// 在 deps 目录中查找指定 crate 的 dll 文件
pub fn find_crate_dll(deps_dir: &Path, crate_name: &str) -> Option<PathBuf> {
    let prefix = format!("{}", crate_name);
    let dll_ext = match std::env::consts::OS {
        "windows" => "dll",
        "linux" => "so",
        "macos" => "dylib",
        _ => "so",
    };
    if let Ok(entries) = fs::read_dir(deps_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.starts_with(&prefix) && file_name.ends_with(dll_ext) {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// 清理算法名称，将非法字符替换为下划线
pub fn sanitize_algorithm_name(name: &str) -> String {
    name.replace(|c: char| !c.is_alphanumeric() && c != '_' && c != '-', "_")
}

/// 参数类型定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParamType {
    Float,
    Int,
    Bool,
    String,
    DataFrame,
    DataFrameArray,
}

/// 参数定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperatorPortParamDef {
    pub name: String,
    pub param_type: ParamType,
    pub default_value: String,
}

/// 生成参数常量代码
pub fn generate_param_constants(params: &[OperatorPortParamDef]) -> String {
    let mut constants = String::new();
    for param in params {
        let const_name = format!(
            "PARAM_{}",
            param.name.to_uppercase().replace(|c: char| !c.is_alphanumeric(), "_")
        );

        match param.param_type {
            ParamType::Float => {
                if let Ok(f) = param.default_value.parse::<f64>() {
                    let value_str = if f.fract() == 0.0 {
                        format!("{}.0", f)
                    } else {
                        format!("{}", f)
                    };
                    constants.push_str(&format!("const {}: f64 = {};\n", const_name, value_str));
                }
            }
            ParamType::Int => {
                if let Ok(i) = param.default_value.parse::<i64>() {
                    constants.push_str(&format!("const {}: i64 = {};\n", const_name, i));
                }
            }
            ParamType::Bool => {
                if let Ok(b) = param.default_value.parse::<bool>() {
                    constants.push_str(&format!("const {}: bool = {};\n", const_name, b));
                }
            }
            ParamType::String => {
                let escaped_value = param.default_value.replace('\\', "\\\\").replace('"', "\\\"");
                constants.push_str(&format!(
                    "const {}: &str = \"{}\";\n",
                    const_name, escaped_value
                ));
            }
            ParamType::DataFrame => {}
            ParamType::DataFrameArray => {}
        }
    }

    if !constants.is_empty() {
        constants.push('\n');
    }

    constants
}

/// 根据算子定义和参数定义生成完整的代码（注入参数常量）
pub fn inject_params_into_code(code: &str, params: &[OperatorPortParamDef]) -> String {
    let constants = generate_param_constants(params);
    format!("{}{}", constants, code)
}

/// 查找 operator_runtime DLL 的路径
pub fn find_runtime_dll() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()));
    let cwd = std::env::current_dir().ok();

    let possible_dirs: Vec<Option<PathBuf>> = vec![
        exe_dir.clone(),
        exe_dir.as_ref().and_then(|p| p.parent().map(|p| p.to_path_buf())),
        exe_dir
            .as_ref()
            .and_then(|p| p.parent().and_then(|p| p.parent().map(|p| p.to_path_buf()))),
        cwd.as_ref().map(|c| c.join("target").join("debug")),
        cwd.as_ref().map(|c| c.join("target").join("debug").join("deps")),
        cwd.as_ref().map(|c| c.join("target").join("release")),
        cwd.as_ref().map(|c| c.join("target").join("release").join("deps")),
        cwd.as_ref().map(|c| c.join("operator_runtime")),
    ];

    for dir in possible_dirs.into_iter().flatten() {
        if dir.exists() {
            if let Some(path) = find_crate_dll(&dir, "operator_runtime") {
                return Some(path);
            }
        }
    }

    None
}

/// 确保 operator_runtime DLL 已加载（全局只加载一次）
pub fn ensure_runtime_loaded() -> Result<(), String> {
    static RUNTIME_LOADED: Once = Once::new();
    static mut LOAD_ERROR: Option<String> = None;

    RUNTIME_LOADED.call_once(|| {
        let runtime_path = match find_runtime_dll() {
            Some(p) => p,
            None => {
                unsafe {
                    LOAD_ERROR = Some("找不到 operator_runtime DLL".to_string());
                }
                return;
            }
        };

        unsafe {
            match Library::new(&runtime_path) {
                Ok(lib) => {
                    std::mem::forget(lib);
                }
                Err(e) => {
                    LOAD_ERROR = Some(format!("加载 operator_runtime DLL 失败: {}", e));
                }
            }
        }
    });

    unsafe {
        if let Some(ref err) = LOAD_ERROR {
            Err(err.clone())
        } else {
            Ok(())
        }
    }
}

/// Cargo 编译结果
#[derive(Debug, Clone)]
pub struct CargoBuildResult {
    pub success: bool,
    pub lib_path: Option<PathBuf>,
    pub temp_dir: Option<PathBuf>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

/// 使用 cargo build 编译临时项目
pub fn cargo_project_build(
    code: &str,
    algorithm_name: &str,
    compile_base_dir: &Path,
    debug: bool,
    temp_dir_prefix: &str,
    runtime_path: &Path,
) -> CargoBuildResult {
    let runtime_path_str = match runtime_path.to_str() {
        Some(s) => s.replace("\\", "/"),
        None => {
            return CargoBuildResult {
                success: false,
                lib_path: None,
                temp_dir: None,
                stdout: String::new(),
                stderr: String::new(),
                error: Some("operator_runtime 路径无法转换为字符串".to_string()),
            };
        }
    };

    let sanitized_name = sanitize_algorithm_name(algorithm_name);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_dir_path = compile_base_dir
        .join(&sanitized_name)
        .join(format!("{}_{}", temp_dir_prefix, timestamp));

    if let Err(e) = fs::create_dir_all(&temp_dir_path) {
        return CargoBuildResult {
            success: false,
            lib_path: None,
            temp_dir: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(format!("创建编译目录失败: {}", e)),
        };
    }

    let src_dir = temp_dir_path.join("src");
    if let Err(e) = fs::create_dir_all(&src_dir) {
        return CargoBuildResult {
            success: false,
            lib_path: None,
            temp_dir: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(format!("创建 src 目录失败: {}", e)),
        };
    }

    let lib_rs_path = src_dir.join("lib.rs");
    if let Err(e) = fs::write(&lib_rs_path, code) {
        return CargoBuildResult {
            success: false,
            lib_path: None,
            temp_dir: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(format!("写入 lib.rs 失败: {}", e)),
        };
    }

    let cargo_toml = format!(
        r#"[workspace]

[package]
name = "{}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]
name = "{}"

[dependencies]
operator_runtime = {{ path = "{}" }}

[profile.release]
opt-level = 3
strip = "debuginfo"
codegen-units = 1
panic = "unwind"
prefer-dynamic = true

[profile.dev]
opt-level = 0
debug = true
prefer-dynamic = true
"#,
        sanitized_name, sanitized_name, runtime_path_str
    );

    let cargo_toml_path = temp_dir_path.join("Cargo.toml");
    if let Err(e) = fs::write(&cargo_toml_path, cargo_toml) {
        return CargoBuildResult {
            success: false,
            lib_path: None,
            temp_dir: None,
            stdout: String::new(),
            stderr: String::new(),
            error: Some(format!("写入 Cargo.toml 失败: {}", e)),
        };
    }

    let mut cargo = Command::new("cargo");
    let profile = if debug { "debug" } else { "release" };

    let project_target_dir = runtime_path
        .parent()
        .map(|p| p.join("target"))
        .unwrap_or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|c| c.join("target"))
                .unwrap_or_else(|| temp_dir_path.join("target"))
        });

    cargo
        .current_dir(&temp_dir_path)
        .env("CARGO_TARGET_DIR", &project_target_dir)
        .env("RUSTFLAGS", "-C prefer-dynamic")
        .arg("build")
        .arg(format!("--{}", profile));

    let dll_ext = match std::env::consts::OS {
        "windows" => "dll",
        "linux" => "so",
        "macos" => "dylib",
        _ => "so",
    };

    let output = match cargo.output() {
        Ok(o) => o,
        Err(e) => {
            return CargoBuildResult {
                success: false,
                lib_path: None,
                temp_dir: None,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(format!("执行 cargo 命令失败: {}", e)),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let all_output = format!("{}\n{}", stdout, stderr);
        let error_lines: Vec<&str> = all_output
            .lines()
            .filter(|line| {
                line.starts_with("error")
                    || line.starts_with("  -->")
                    || line.starts_with("   |")
            })
            .collect();

        let error_message = if !error_lines.is_empty() {
            error_lines.join("\n")
        } else if stderr.len() > 3000 {
            format!("...{}", &stderr[stderr.len() - 3000..])
        } else {
            stderr.clone()
        };

        return CargoBuildResult {
            success: false,
            lib_path: None,
            temp_dir: Some(temp_dir_path),
            stdout,
            stderr,
            error: Some(format!(
                "编译失败 (exit code: {})\n{}",
                output.status.code().unwrap_or(-1),
                error_message
            )),
        };
    }

    let output_path = project_target_dir
        .join(profile)
        .join(format!("{}.{}", sanitized_name, dll_ext));

    if output_path.exists() {
        return CargoBuildResult {
            success: true,
            lib_path: Some(output_path),
            temp_dir: Some(temp_dir_path),
            stdout,
            stderr,
            error: None,
        };
    }

    let deps_path = project_target_dir.join(profile).join("deps");
    if let Ok(entries) = fs::read_dir(&deps_path) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.starts_with(&sanitized_name) && file_name.ends_with(dll_ext) {
                    return CargoBuildResult {
                        success: true,
                        lib_path: Some(path),
                        temp_dir: Some(temp_dir_path),
                        stdout,
                        stderr,
                        error: None,
                    };
                }
            }
        }
    }

    CargoBuildResult {
        success: false,
        lib_path: None,
        temp_dir: Some(temp_dir_path),
        stdout,
        stderr,
        error: Some(format!(
            "编译成功但未找到输出文件: {}",
            output_path.display()
        )),
    }
}

/// 只编译自定义算子代码
pub fn compile_only(
    code: &str,
    algorithm_name: &str,
    compile_base_dir: &Path,
    runtime_path: &Path,
) -> Result<PathBuf, String> {
    ensure_runtime_loaded()?;

    let result = cargo_project_build(code, algorithm_name, compile_base_dir, false, "build", runtime_path);

    if result.success {
        result
            .lib_path
            .ok_or_else(|| "编译成功但未找到输出文件".to_string())
    } else {
        Err(result.error.unwrap_or_else(|| "编译失败".to_string()))
    }
}

/// 算子执行函数的 C ABI 签名
///
/// 参数:
/// - inputs: 输入 CPortData 数组指针
/// - input_count: 输入元素数量
/// - outputs: 输出 CPortData 数组指针（预分配，由算子写入）
/// - output_cap: 输出数组容量
/// - params_json: 参数 JSON 字符串（C 字符串）
///
/// 返回值:
/// - 0: 成功
/// - 非零: 失败
pub type ExecuteOperatorNativeFn = unsafe extern "C" fn(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const std::os::raw::c_char,
) -> i32;

// ===== 算子 DLL 全局缓存 =====
//
// 设计目的：将「算子 DLL 加载」从每次执行（`execute_native_operator`）提前到
// 后台服务启动时（`operator_runtime_server::main`）。运行时直接复用已缓存的
// 执行函数指针，避免每次执行都 `Library::new` 带来的 IO/锁开销与日志刷屏。
//
// 生命周期：`Library` 实例在加载后通过 `std::mem::forget` 永久持有（进程退出时
// 由 OS 回收），确保函数指针在进程生命周期内始终有效。函数指针本身为 `Copy`，
// 可安全跨线程使用。

static OPERATOR_CACHE_INIT: Once = Once::new();
static mut OPERATOR_CACHE: Option<RwLock<HashMap<PathBuf, ExecuteOperatorNativeFn>>> = None;

fn operator_cache() -> &'static RwLock<HashMap<PathBuf, ExecuteOperatorNativeFn>> {
    OPERATOR_CACHE_INIT.call_once(|| {
        unsafe {
            OPERATOR_CACHE = Some(RwLock::new(HashMap::new()));
        }
    });
    // 经 addr_of! 取裸指针再解引用，避免直接对 static mut 创建共享引用
    // （Rust 2024 的 static_mut_refs 告警），与上方 RUNTIME_ERROR_API 同款写法。
    let ptr = std::ptr::addr_of!(OPERATOR_CACHE);
    unsafe {
        match &*ptr {
            Some(cache) => cache,
            None => unreachable!("OPERATOR_CACHE 在 call_once 后必然已初始化"),
        }
    }
}

/// 规范化 DLL 路径作为缓存键，避免相对/绝对路径不一致导致重复加载。
fn canonicalize_dll_path(dll_path: &Path) -> PathBuf {
    dll_path
        .canonicalize()
        .unwrap_or_else(|_| dll_path.to_path_buf())
}

/// 预加载算子 DLL 到全局缓存。
///
/// 在后台服务启动时遍历算子库目录批量调用；运行时 `execute_native_operator`
/// 直接复用已加载的函数指针。重复加载同一 DLL 会被跳过；加载后 `Library`
/// 永不释放，函数指针在进程生命周期内始终有效。
pub fn preload_operator(dll_path: &Path) -> Result<(), String> {
    ensure_runtime_loaded()?;

    let key = canonicalize_dll_path(dll_path);

    // 快速路径：读锁检查是否已加载
    if operator_cache().read().unwrap().contains_key(&key) {
        return Ok(());
    }

    // 慢路径：加写锁后再次检查，避免并发预加载同一 DLL 导致重复加载
    let mut cache = operator_cache().write().unwrap();
    if cache.contains_key(&key) {
        return Ok(());
    }

    eprintln!("[operator_preload] 加载算子 DLL: {}", key.display());

    let lib = unsafe { Library::new(&key).map_err(|e| format!("加载动态库失败: {}", e))? };

    let execute_fn: ExecuteOperatorNativeFn = unsafe {
        let symbol: Symbol<ExecuteOperatorNativeFn> = lib
            .get(b"execute_operator")
            .map_err(|e| format!("找不到 execute_operator 函数: {}", e))?;
        *symbol
    };

    // 永久持有 Library，绝不释放，确保函数指针始终有效
    std::mem::forget(lib);

    cache.insert(key, execute_fn);
    Ok(())
}

/// 执行预编译的 Rust 原生算子（C ABI 接口）
///
/// 加载策略：
/// - **命中缓存**（服务启动时由 `preload_operator` 预加载的算子）：直接复用已缓存
///   的函数指针，无需 `Library::new`，消除每次执行的加载开销与日志刷屏。
/// - **未命中缓存**（运行时编译的自定义算子）：即时 `Library::new` 加载并执行，
///   `Library` 持有至函数末尾才 drop 释放文件锁，保持原有行为——便于 `enable_operator`
///   重新编译覆盖 DLL 文件（避免长期占用文件锁导致覆盖失败）。
pub fn execute_native_operator(
    dll_path: &Path,
    inputs: &[PortData],
    max_outputs: usize,
    params_json: &str,
) -> Result<Vec<PortData>, String> {
    ensure_runtime_loaded()?;

    let key = canonicalize_dll_path(dll_path);

    // 命中全局缓存则直接复用函数指针；未命中则即时加载 Library，并持有至函数末尾
    // 以保持 DLL 映射（执行期间不可卸载），函数返回时 drop → 释放文件锁。
    let cached_fn = operator_cache().read().unwrap().get(&key).copied();
    let (execute_fn, _lib_guard): (ExecuteOperatorNativeFn, Option<Library>) = match cached_fn {
        Some(f) => (f, None),
        None => {
            let lib = unsafe {
                Library::new(dll_path).map_err(|e| format!("加载动态库失败: {}", e))?
            };
            let f: ExecuteOperatorNativeFn = unsafe {
                let symbol: Symbol<ExecuteOperatorNativeFn> = lib
                    .get(b"execute_operator")
                    .map_err(|e| format!("找不到 execute_operator 函数: {}", e))?;
                *symbol
            };
            (f, Some(lib))
        }
    };

    eprintln!(
        "[operator_exec] 执行算子 DLL: {} ({})",
        dll_path.display(),
        if _lib_guard.is_some() { "即时加载" } else { "缓存" }
    );
    eprintln!("[operator_exec] 输入数量: {}, 最大输出: {}, 参数长度: {}", inputs.len(), max_outputs, params_json.len());

        // 将 Rust PortData 输入转换为 C ABI CPortData 数组
    let input_array = c_pd_array_new();
    for input in inputs {
        let c_pd = portdata_to_c(input);
        c_pd_array_push(input_array, c_pd);
    }
    let input_count = c_pd_array_len(input_array);
    let input_data = unsafe { (*input_array).data };

    // 分配输出缓冲区（C ABI PortData 数组）
    let output_cap = max_outputs.max(1);
    let mut output_pds: Vec<CPortData> = (0..output_cap)
        .map(|_| CPortData {
            type_tag: operator_runtime::c_abi::TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        })
        .collect();
    let output_data = output_pds.as_mut_ptr();

    let params_cstr =
        CString::new(params_json).map_err(|e| format!("参数 JSON 转换失败: {}", e))?;
    let params_ptr = params_cstr.as_ptr();

    let result = unsafe {
        eprintln!("[operator_exec] 调用 execute_operator, params_json: {:?}", params_json);
        execute_fn(input_data, input_count, output_data, output_cap, params_ptr)
    };
    // 注：_lib_guard 在函数末尾才 drop，确保执行与输出收集期间 DLL 保持映射

    eprintln!("[operator_exec] execute_operator 返回码: {}", result);

    // 释放输入数组的所有权（数据已被算子复制或使用完毕）
    unsafe {
        let _ = c_pd_array_len(input_array); // just to keep input_array alive
        let _ = Box::from_raw(input_array); // 释放 CPortDataArray 结构
    }

    if result != 0 {
        // 在清理输出之前，先尝试获取错误信息
        let real_error = get_runtime_last_error();

        // 清理可能被算子写入的输出
        for pd in &mut output_pds {
            if pd.type_tag != operator_runtime::c_abi::TYPE_NULL {
                operator_runtime::c_abi::c_pd_free(pd as *mut CPortData);
            }
        }

        let err_detail = match real_error {
            Some(msg) if !msg.is_empty() => {
                eprintln!("[operator_error] 返回码: {}, 错误信息: {}", result, msg);
                format!("执行失败: {} (返回码: {})", msg, result)
            }
            _ => {
                eprintln!("[operator_error] 返回码: {}, 无详细错误信息", result);
                // 为已知算子的常见返回码提供可读提示，帮助定位问题
                let hint = match result {
                    -1 => "（通常为 runtime 加载失败或参数解析失败）",
                    -2 => "（通常为数据源算子的数据库连接或 SQL 查询失败，请检查数据库配置）",
                    _ => "",
                };
                format!("执行失败，返回码: {}{}", result, hint)
            }
        };
        return Err(err_detail);
    }

    // 收集输出 - 转换回 Rust PortData
    let mut results = Vec::new();
    for pd in &mut output_pds {
        if pd.type_tag == operator_runtime::c_abi::TYPE_NULL {
            break;
        }
        results.push(unsafe { portdata_from_c(pd) });
    }

    Ok(results)
}

/// 查找 operator_runtime 的路径
pub fn find_runtime_path() -> Result<PathBuf, String> {
    let possible_paths = vec![
        std::env::current_dir()
            .ok()
            .map(|c| c.join("operator_runtime")),
        std::env::current_dir()
            .ok()
            .and_then(|c| c.parent().map(|p| p.join("operator_runtime"))),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.join("operator_runtime"))),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().and_then(|p| p.parent().map(|p| p.join("operator_runtime")))),
        std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent().map(|p| p.join("operator_runtime")))
            }),
    ];

    let found_path = possible_paths
        .iter()
        .flatten()
        .find(|p| p.exists())
        .cloned();

    match found_path {
        Some(p) => Ok(p),
        None => {
            let tried_paths: Vec<String> = possible_paths
                .into_iter()
                .flatten()
                .map(|p| p.to_string_lossy().to_string())
                .collect();

            Err(format!(
                "无法找到 operator_runtime 目录。尝试了以下路径:\n{}",
                tried_paths.join("\n")
            ))
        }
    }
}

/// 封装：便于 runtime 服务器直接调用
pub fn execute_operator(
    dll_path: &Path,
    inputs: &[operator_runtime::PortData],
    max_outputs: usize,
    params_json: &str,
) -> Result<Vec<operator_runtime::PortData>, String> {
    execute_native_operator(dll_path, inputs, max_outputs, params_json)
}

/// 编译并执行
pub fn compile_and_execute(
    code: &str,
    algorithm_name: &str,
    compile_base_dir: &Path,
    inputs: &[operator_runtime::PortData],
    output_count: usize,
    runtime_path: &Path,
) -> Result<Vec<operator_runtime::PortData>, String> {
    let build_result = cargo_project_build(
        code,
        algorithm_name,
        compile_base_dir,
        false,
        "build",
        runtime_path,
    );

    let output_path = if build_result.success {
        build_result
            .lib_path
            .ok_or_else(|| "编译成功但未找到输出文件".to_string())?
    } else {
        return Err(build_result.error.unwrap_or_else(|| "编译失败".to_string()));
    };

    execute_native_operator(&output_path, inputs, output_count, "")
}

// ===== 流式算子 C ABI 接口 =====
//
// 与 `execute_operator` 批量接口平行的可选能力：算子 DLL 可导出 5 个流式符号
// （`execute_operator_stream_start/push/push_end/next/end`），由本层加载并封装为
// `StreamHandle`，供服务端在 DAG 流式编排中按 chunk 拉取。
//
// 详细契约见 `operator_runtime/src/c_abi.rs` 顶部的流式文档注释。

/// 流式 start 函数指针
pub type StreamStartFn = unsafe extern "C" fn(
    inputs: *const CPortData,
    input_count: usize,
    params_json: *const std::os::raw::c_char,
) -> *mut std::ffi::c_void;
/// 流式 push 函数指针（借用语义）
pub type StreamPushFn = unsafe extern "C" fn(
    handle: *mut std::ffi::c_void,
    chunk: *const CPortData,
) -> i32;
/// 流式 push_end 函数指针
pub type StreamPushEndFn = unsafe extern "C" fn(handle: *mut std::ffi::c_void) -> i32;
/// 流式 next 函数指针（三态返回）
pub type StreamNextFn = unsafe extern "C" fn(
    handle: *mut std::ffi::c_void,
    out_chunk: *mut CPortData,
) -> i32;
/// 流式 end 函数指针
pub type StreamEndFn = unsafe extern "C" fn(handle: *mut std::ffi::c_void);

/// 算子导出的 5 个流式函数指针集合。
///
/// 函数指针本身为 `Copy`，可安全缓存与跨线程共享（指向永久持有的 DLL 内存）。
#[derive(Clone, Copy)]
pub struct StreamingOperatorFns {
    pub start: StreamStartFn,
    pub push: StreamPushFn,
    pub push_end: StreamPushEndFn,
    pub next: StreamNextFn,
    pub end: StreamEndFn,
}
// 函数指针指向进程生命周期内永久映射的 DLL，可跨线程共享
unsafe impl Send for StreamingOperatorFns {}
unsafe impl Sync for StreamingOperatorFns {}

/// 从已加载的 Library 中解析 5 个流式符号；缺任一返回 None（视为不支持流式）。
fn resolve_streaming_fns(lib: &Library) -> Option<StreamingOperatorFns> {
    let start = unsafe { get_fn::<StreamStartFn>(lib, b"execute_operator_stream_start")? };
    let push = unsafe { get_fn::<StreamPushFn>(lib, b"execute_operator_stream_push")? };
    let push_end = unsafe { get_fn::<StreamPushEndFn>(lib, b"execute_operator_stream_push_end")? };
    let next = unsafe { get_fn::<StreamNextFn>(lib, b"execute_operator_stream_next")? };
    let end = unsafe { get_fn::<StreamEndFn>(lib, b"execute_operator_stream_end")? };
    Some(StreamingOperatorFns { start, push, push_end, next, end })
}

/// 取符号并解引用为函数指针（Copy）的辅助。
unsafe fn get_fn<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
    let sym: Symbol<T> = lib.get(name).ok()?;
    Some(*sym)
}

// ===== 流式算子全局缓存 =====
//
// 与 `OPERATOR_CACHE` 平行，缓存 `Option<StreamingOperatorFns>`：
// - `Some(fns)`：算子支持流式，fns 指向永久映射的 DLL
// - `None`：已探测，算子不支持流式
// - 键不存在：尚未探测
//
// `probe_streaming_operator` 与 `preload_streaming_operator` 负责建表；
// `StreamHandle::start` 优先查缓存，未命中时即时探测并写入。

static STREAMING_CACHE_INIT: Once = Once::new();
static mut STREAMING_CACHE: Option<RwLock<HashMap<PathBuf, Option<StreamingOperatorFns>>>> = None;

fn streaming_cache() -> &'static RwLock<HashMap<PathBuf, Option<StreamingOperatorFns>>> {
    STREAMING_CACHE_INIT.call_once(|| unsafe {
        STREAMING_CACHE = Some(RwLock::new(HashMap::new()));
    });
    let ptr = std::ptr::addr_of!(STREAMING_CACHE);
    unsafe {
        match &*ptr {
            Some(cache) => cache,
            None => unreachable!("STREAMING_CACHE 在 call_once 后必然已初始化"),
        }
    }
}

/// 探测算子是否支持流式执行，返回函数指针集合（不支持返回 `None`）。
///
/// 命中缓存直接返回；未命中则加载 DLL、解析 5 个符号、`mem::forget` 永久持有 DLL
/// 映射并写入缓存。**注意**：探测后 DLL 永久映射（文件锁不释放），自定义算子
/// 如需重新编译覆盖，建议通过预加载路径（`preload_streaming_operator`）在服务
/// 启动期加载，或重启服务。
pub fn probe_streaming_operator(dll_path: &Path) -> Result<Option<StreamingOperatorFns>, String> {
    ensure_runtime_loaded()?;
    let key = canonicalize_dll_path(dll_path);

    // 快速路径：读锁查缓存
    if let Some(cached) = streaming_cache().read().unwrap().get(&key).copied() {
        return Ok(cached);
    }

    // 慢路径：加载 DLL 探测符号
    let mut cache = streaming_cache().write().unwrap();
    if let Some(cached) = cache.get(&key).copied() {
        return Ok(cached); // double-check，避免并发重复加载
    }

    let lib = unsafe { Library::new(&key).map_err(|e| format!("加载动态库失败: {}", e))? };
    let fns = resolve_streaming_fns(&lib);
    // 永久持有 Library 映射，确保函数指针始终有效
    std::mem::forget(lib);
    cache.insert(key, fns);
    Ok(fns)
}

/// 预加载算子 DLL 的流式符号到全局缓存（服务启动期批量调用）。
///
/// 与 `preload_operator` 配合使用：前者缓存批量 `execute_operator` 函数指针，
/// 本函数缓存流式函数指针（`Some` 或 `None`）。重复加载同一 DLL 会被跳过。
pub fn preload_streaming_operator(dll_path: &Path) -> Result<(), String> {
    ensure_runtime_loaded()?;
    let key = canonicalize_dll_path(dll_path);

    if streaming_cache().read().unwrap().contains_key(&key) {
        return Ok(());
    }

    let mut cache = streaming_cache().write().unwrap();
    if cache.contains_key(&key) {
        return Ok(());
    }

    let lib = unsafe { Library::new(&key).map_err(|e| format!("加载动态库失败: {}", e))? };
    let fns = resolve_streaming_fns(&lib);
    std::mem::forget(lib);
    cache.insert(key, fns);
    Ok(())
}

/// 流式执行句柄，封装算子 DLL 返回的不透明 handle 与 5 个函数指针。
///
/// **生命周期**：`start` 创建，`end`（或 Drop）释放。handle 持有裸指针，**不可
/// 跨线程移动**（不实现 `Send`）——必须在构造它的 `spawn_blocking` 线程内使用并
/// 销毁，防止跨 `await` 边界。
///
/// **累积**：`accumulated` 缓存本节点产出的所有 chunk，供流式链尾聚合与非流式
/// 下游消费。`next` 不自动累积，由编排层显式调用 `accumulate`。
pub struct StreamHandle {
    handle: *mut std::ffi::c_void,
    fns: StreamingOperatorFns,
    accumulated: Vec<PortData>,
    ended: bool,
}

impl StreamHandle {
    /// 启动流式执行：解析 DLL（优先缓存）、调用 `stream_start`。
    ///
    /// `inputs` 为物化的非流式输入（流式上游端口由服务端以占位填充，算子不应读取）。
    /// 算子不支持流式（未导出符号）时返回 `Err`。
    pub fn start(dll_path: &Path, inputs: &[PortData], params_json: &str) -> Result<Self, String> {
        let fns = match probe_streaming_operator(dll_path)? {
            Some(f) => f,
            None => return Err(format!("算子不支持流式执行: {}", dll_path.display())),
        };

        // 构造输入 CPortData 数组（与 execute_native_operator 同款写法）
        let input_array = c_pd_array_new();
        for input in inputs {
            let c_pd = portdata_to_c(input);
            c_pd_array_push(input_array, c_pd);
        }
        let input_count = c_pd_array_len(input_array);
        let input_data = unsafe { (*input_array).data };

        let params_cstr =
            CString::new(params_json).map_err(|e| format!("参数 JSON 转换失败: {}", e))?;

        let handle = unsafe { (fns.start)(input_data, input_count, params_cstr.as_ptr()) };

        // 释放输入数组外壳（数据已被算子复制或使用，与 execute_native_operator 一致）
        unsafe {
            let _ = c_pd_array_len(input_array); // keep alive
            let _ = Box::from_raw(input_array);
        }

        if handle.is_null() {
            let err = get_runtime_last_error()
                .unwrap_or_else(|| "stream_start 返回 null".to_string());
            return Err(err);
        }

        Ok(StreamHandle {
            handle,
            fns,
            accumulated: Vec::new(),
            ended: false,
        })
    }

    /// 推入一个上游 chunk（**借用语义**，支持扇出）。
    ///
    /// 服务端把 `chunk` 转为 owned `CPortData` 传引用给算子，push 返回后由服务端
    /// `c_pd_free` 释放。算子需保留时必须自行深拷贝。
    pub fn push(&mut self, chunk: &PortData) -> Result<(), String> {
        let c_pd = portdata_to_c(chunk);
        let rc = unsafe { (self.fns.push)(self.handle, &c_pd as *const CPortData) };
        // 借用结束：释放 owned chunk（算子应深拷贝而非 take 所有权）
        c_pd_free(&c_pd as *const CPortData as *mut CPortData);
        if rc < 0 {
            let err = get_runtime_last_error()
                .unwrap_or_else(|| format!("stream_push 返回 {}", rc));
            return Err(err);
        }
        Ok(())
    }

    /// 通知上游已 EOF，transformer 可 flush 残留缓冲。
    pub fn push_end(&mut self) -> Result<(), String> {
        let rc = unsafe { (self.fns.push_end)(self.handle) };
        if rc < 0 {
            let err = get_runtime_last_error()
                .unwrap_or_else(|| format!("stream_push_end 返回 {}", rc));
            return Err(err);
        }
        Ok(())
    }

    /// 拉取下一个输出 chunk。
    ///
    /// 返回 `Ok(Some(pd))` = 有 chunk（owned）；`Ok(None)` = 当前暂无更多（非永久，
    /// 调用方应继续 push 或在 `push_end` 后再循环 `next` 直至 `None` 才算永久结束）；
    /// `Err` = 错误。
    pub fn next(&mut self) -> Result<Option<PortData>, String> {
        let mut slot = CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: std::ptr::null_mut() },
        };
        let rc = unsafe { (self.fns.next)(self.handle, &mut slot as *mut CPortData) };
        match rc {
            0 => {
                // 取出 owned 数据（portdata_from_c 消费 slot 并置 TYPE_NULL）
                let pd = unsafe { portdata_from_c(&mut slot as *mut CPortData) };
                c_pd_free(&mut slot as *mut CPortData); // 已置 NULL，no-op
                Ok(Some(pd))
            }
            1 => {
                c_pd_free(&mut slot as *mut CPortData); // 未触碰，no-op
                Ok(None)
            }
            _ => {
                c_pd_free(&mut slot as *mut CPortData);
                let err = get_runtime_last_error()
                    .unwrap_or_else(|| format!("stream_next 返回 {}", rc));
                Err(err)
            }
        }
    }

    /// 累积一个 chunk 到本节点输出缓存（供链尾聚合 / DagNodeResult 预览）。
    pub fn accumulate(&mut self, chunk: PortData) {
        self.accumulated.push(chunk);
    }

    /// 已累积的 chunk 列表。
    pub fn accumulated(&self) -> &[PortData] {
        &self.accumulated
    }

    /// 消费并返回全部累积 chunk，同时释放流式 handle。
    pub fn into_accumulated(mut self) -> Vec<PortData> {
        if !self.ended {
            unsafe { (self.fns.end)(self.handle) };
            self.ended = true;
        }
        std::mem::take(&mut self.accumulated)
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if !self.ended {
            unsafe { (self.fns.end)(self.handle) };
            self.ended = true;
        }
    }
}
