use mining_app::operator_executor::{compile_only, inject_params_into_code};

fn main() {
    // 模拟用户在编辑器中输入的代码（使用原生 Rust ABI）
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
    // 读取输入 DataFrame
    let input_df = if input_count > 0 {
        unsafe { &*(*inputs) }
    } else {
        // 无输入时返回默认数据
        let result = df!["value" => [1.0, 2.0, 3.0, 4.0, 5.0]].unwrap();
        if output_count > 0 {
            unsafe { *outputs = Box::into_raw(Box::new(result)); }
        }
        return 0;
    };
    
    // 用户自定义逻辑：将输入数据乘以 2
    let result = input_df.clone()
        .with_column(input_df.column("value").unwrap() * 2.0)
        .unwrap();
    
    // 写入输出
    if output_count > 0 {
        unsafe { *outputs = Box::into_raw(Box::new(result)); }
    }
    
    0
}
"#;
    
    // 注入参数（这里没有参数）
    let code_with_params = inject_params_into_code(user_code, &[]);
    
    println!("=== 注入后的代码 ===");
    println!("{}", code_with_params);
    println!("=== 开始编译 ===");
    
    match compile_only(&code_with_params, "test_operator") {
        Ok(path) => {
            println!("编译成功！DLL 路径: {}", path.display());
            if let Ok(metadata) = std::fs::metadata(&path) {
                println!("DLL 大小: {} KB", metadata.len() / 1024);
            }
        }
        Err(e) => {
            println!("编译失败:\n{}", e);
            std::process::exit(1);
        }
    }
}
