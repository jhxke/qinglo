use mining_app::operator_executor::{compile_only, inject_params_into_code};
use std::fs;

fn main() {
    // 模拟用户在编辑器中输入的代码（使用原生 Rust ABI，支持多输入多输出）
    let user_code = r#"
use polars::prelude::*;
use std::slice;

#[export_name = "execute_operator"]
pub extern "C" fn execute_operator(
    inputs: *const *const DataFrame,
    input_count: usize,
    outputs: *mut *mut DataFrame,
    output_count: usize,
    _params_json: *const std::os::raw::c_char,
) -> i32 {
    // 将输入指针数组转为 Rust 切片（安全访问多个输入）
    let input_dfs = if input_count > 0 {
        unsafe { slice::from_raw_parts(inputs, input_count) }
    } else {
        // 无输入时返回默认数据
        let result = df!["value" => [1.0, 2.0, 3.0, 4.0, 5.0]].unwrap();
        if output_count > 0 {
            unsafe { *outputs = Box::into_raw(Box::new(result)); }
        }
        return 0;
    };
    
    // 用户自定义逻辑：处理多个输入
    // 示例：将所有输入的 value 列相加
    let mut result: Option<DataFrame> = None;
    for (idx, &input_df) in input_dfs.iter().enumerate() {
        let df = unsafe { &*input_df };
        if let Ok(col) = df.column("value") {
            if let Some(series) = col.as_series() {
                if result.is_none() {
                    result = Some(df!["value" => series.clone()].unwrap());
                } else {
                    result = result.map(|r| {
                        let r_col = r.column("value").unwrap();
                        let sum = series + r_col;
                        df!["value" => sum].unwrap()
                    });
                }
            }
        }
    }
    
    // 写入多个输出（示例：每个输入产生一个输出）
    let actual_output_count = output_count.min(input_count);
    for i in 0..actual_output_count {
        let df = unsafe { &*input_dfs[i] };
        let processed = df.clone()
            .with_column(df.column("value").unwrap() * (i + 1) as f64)
            .unwrap();
        unsafe { *outputs.add(i) = Box::into_raw(Box::new(processed)); }
    }
    
    0
}
"#;
    
    // 注入参数（这里没有参数）
    let code_with_params = inject_params_into_code(user_code, &[]);
    
    println!("=== 开始编译 ===");
    let start = std::time::Instant::now();
    
    match compile_only(&code_with_params, "test_operator_dynamic") {
        Ok(path) => {
            let duration = start.elapsed();
            println!("编译成功！耗时: {:?}", duration);
            println!("DLL 路径: {}", path.display());
            
            if let Ok(metadata) = fs::metadata(&path) {
                let size_kb = metadata.len() / 1024;
                println!("DLL 大小: {} KB", size_kb);
            }
            
            // 检查 operator_runtime.dll 大小
            if let Some(runtime_path) = path.parent().map(|p| p.join("operator_runtime.dll")) {
                if let Ok(metadata) = fs::metadata(&runtime_path) {
                    let size_kb = metadata.len() / 1024;
                    println!("operator_runtime.dll 大小: {} KB", size_kb);
                }
            }
        }
        Err(e) => {
            println!("编译失败:\n{}", e);
            std::process::exit(1);
        }
    }
}
