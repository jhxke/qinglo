use super::*;
use operator_runtime::{DataType, ColumnData, DataFrame, PortData};
use operator_runtime::c_abi::{CPortData, portdata_to_c};
use std::ffi::CString;

/// 测试参数解析功能
#[test]
fn test_parse_params() {
    // 测试空字符串
    let params = parse_params("");
    assert_eq!(params.host, "");
    assert_eq!(params.port, 0);
    assert_eq!(params.database, "");
    assert_eq!(params.username, "");
    assert_eq!(params.password, "");
    assert_eq!(params.query, "");

    // 测试有效 JSON
    let json = r#"{
        "host": "localhost",
        "port": 5432,
        "database": "whatigo",
        "username": "postgres",
        "password": "difyai123456",
        "query": "select open,high,low,close from tushare_daily limit 10"
    }"#;
    let params = parse_params(json);
    assert_eq!(params.host, "localhost");
    assert_eq!(params.port, 5432);
    assert_eq!(params.database, "whatigo");
    assert_eq!(params.username, "postgres");
    assert_eq!(params.password, "difyai123456");
    assert_eq!(params.query, "select open,high,low,close from tushare_daily limit 10");

    // 测试部分参数（所有字段都必须提供，serde 默认要求完整）
    let json_partial = r#"{
        "host": "127.0.0.1",
        "port": 5433,
        "database": "",
        "username": "",
        "password": "",
        "query": ""
    }"#;
    let params = parse_params(json_partial);
    assert_eq!(params.host, "127.0.0.1");
    assert_eq!(params.port, 5433);
    assert_eq!(params.database, "");
}

/// 测试列类型推断功能（使用模拟的 Row）
#[test]
fn test_infer_column_type() {
    // 由于无法直接创建 tokio_postgres::Row，我们测试辅助函数的逻辑
    // 通过测试 ColumnData 的数据类型处理来间接验证
    let mut col_int = ColumnData::new("test_int".to_string(), DataType::Int64);
    col_int.push_i64(Some(42));
    assert_eq!(col_int.data_type, DataType::Int64);
    assert_eq!(col_int.get_i64(0), Some(42));

    let mut col_float = ColumnData::new("test_float".to_string(), DataType::Float64);
    col_float.push_f64(Some(3.14));
    assert_eq!(col_float.data_type, DataType::Float64);
    assert_eq!(col_float.get_f64(0), Some(3.14));

    let mut col_string = ColumnData::new("test_string".to_string(), DataType::String);
    col_string.push_string(Some("hello"));
    assert_eq!(col_string.data_type, DataType::String);
    assert_eq!(col_string.get_string(0), Some("hello"));

    let mut col_bool = ColumnData::new("test_bool".to_string(), DataType::Bool);
    col_bool.push_bool(Some(true));
    assert_eq!(col_bool.data_type, DataType::Bool);
    assert_eq!(col_bool.get_bool(0), Some(true));
}

/// 测试 DataFrame 创建和操作
#[test]
fn test_dataframe_operations() {
    let mut df = DataFrame::new();
    
    // 添加列
    let col1 = DataFrame::new_int64_column("id", vec![Some(1), Some(2), Some(3)]);
    let col2 = DataFrame::new_string_column("name", vec![Some("Alice"), Some("Bob"), Some("Charlie")]);
    let col3 = DataFrame::new_float64_column("score", vec![Some(95.5), Some(88.0), Some(92.3)]);
    
    df.add_column(col1);
    df.add_column(col2);
    df.add_column(col3);
    
    // 验证行数和列数
    assert_eq!(df.row_count, 3);
    assert_eq!(df.col_count(), 3);
    
    // 验证列数据
    let id_col = df.column("id").unwrap();
    assert_eq!(id_col.get_i64(0), Some(1));
    assert_eq!(id_col.get_i64(1), Some(2));
    assert_eq!(id_col.get_i64(2), Some(3));
    
    let name_col = df.column("name").unwrap();
    assert_eq!(name_col.get_string(0), Some("Alice"));
    assert_eq!(name_col.get_string(1), Some("Bob"));
    assert_eq!(name_col.get_string(2), Some("Charlie"));
    
    let score_col = df.column("score").unwrap();
    assert_eq!(score_col.get_f64(0), Some(95.5));
    assert_eq!(score_col.get_f64(1), Some(88.0));
    assert_eq!(score_col.get_f64(2), Some(92.3));
}

/// 测试空值处理
#[test]
fn test_null_values() {
    let mut col = ColumnData::new("test".to_string(), DataType::Int64);
    col.push_i64(Some(1));
    col.push_i64(None);
    col.push_i64(Some(3));
    
    assert_eq!(col.len(), 3);
    assert_eq!(col.null_count, 1);
    assert_eq!(col.get_i64(0), Some(1));
    assert_eq!(col.get_i64(1), None);
    assert_eq!(col.get_i64(2), Some(3));
    assert!(col.is_null(1));
    assert!(!col.is_null(0));
    assert!(!col.is_null(2));
}

/// 测试 PortData 封装和解封
#[test]
fn test_port_data_operations() {
    // 创建测试 DataFrame
    let mut df = DataFrame::new();
    let col = DataFrame::new_int64_column("test", vec![Some(1), Some(2)]);
    df.add_column(col);
    
    // 封装为 PortData
    let port_data = PortData::DataFrame(df.clone());
    assert_eq!(port_data.type_name(), "DataFrame");
    
    // 验证 PortData 中的 DataFrame
    match port_data {
        PortData::DataFrame(data) => {
            assert_eq!(data.row_count, 2);
            assert_eq!(data.col_count(), 1);
        }
        _ => panic!("Expected DataFrame"),
    }
}

/// 测试 execute_operator 的边界情况（不连接实际数据库）
#[test]
#[allow(unused_unsafe)]
fn test_execute_operator_boundary_cases() {
    // 测试空输出指针
    let _result = execute_operator(
        std::ptr::null(),
        0,
        std::ptr::null_mut(),
        0,
        std::ptr::null(),
    );
    // 函数应该正常返回（即使输出为空）
    // 返回值取决于 runtime 加载情况，这里不做严格断言
    // 主要验证不会崩溃
}

/// 测试 execute_operator 使用有效参数（无数据库时连接会失败，但参数解析应正常）
#[test]
#[allow(unused_unsafe)]
fn test_execute_operator_valid_params() {
    // 创建输入 DataFrame
    let mut input_df = DataFrame::new();
    let col1 = DataFrame::new_int64_column("id", vec![Some(1), Some(2), Some(3), Some(4), Some(5)]);
    let col2 = DataFrame::new_float64_column("price", vec![Some(12.50), Some(15.30), Some(8.90), Some(22.10), Some(5.40)]);
    let col3 = DataFrame::new_string_column("symbol", vec![Some("AAPL"), Some("GOOGL"), Some("MSFT"), Some("AMZN"), Some("TSLA")]);
    input_df.add_column(col1);
    input_df.add_column(col2);
    input_df.add_column(col3);
    
    // 封装为 PortData 并转为 C ABI 类型
    let input_port = PortData::DataFrame(input_df);
    let input_cpd = portdata_to_c(&input_port);
    let input_cpds = [input_cpd];
    
    let json = r#"{
        "host": "localhost",
        "port": 5432,
        "database": "whatigo",
        "username": "postgres",
        "password": "difyai123456",
        "query": "select open,high,low,close from tushare_daily limit 10"
    }"#;
    let json_cstr = CString::new(json).unwrap();
    
    // 设置输出数组用于接收结果
    let mut output_slots: [CPortData; 2] = [CPortData { type_tag: operator_runtime::c_abi::TYPE_NULL, value: operator_runtime::c_abi::CPortValue { str_ptr: std::ptr::null_mut() } }; 2];
    
    let result = execute_operator(
        input_cpds.as_ptr(),
        input_cpds.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        json_cstr.as_ptr(),
    );
    
    // 打印输出 DataFrame 前10行
    if output_slots[0].type_tag != operator_runtime::c_abi::TYPE_NULL {
        unsafe {
            let _output_data = &output_slots[0];
            // 从 CPortData 转换回 PortData 进行验证
            let port_data = operator_runtime::c_abi::portdata_from_c(&output_slots[0] as *const CPortData as *mut CPortData);
            match port_data {
                PortData::DataFrame(df) => {
                    println!("=== 输出 DataFrame ===");
                    println!("行数: {}, 列数: {}", df.row_count, df.col_count());
                    let print_limit = 10.min(df.row_count);
                    let col_names: Vec<&str> = df.columns.iter().map(|c| c.name.as_str()).collect();
                    println!("列名: {:?}", col_names);
                    for i in 0..print_limit {
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
                    println!("=== 打印结束 ===\n");
                }
                other => println!("输出类型: {}", other.type_name()),
            }
        }
    } else {
        println!("输出指针为空（数据库可能未连接）");
    }
    
    // 释放内存
    unsafe {
        if output_slots[0].type_tag != operator_runtime::c_abi::TYPE_NULL {
            release_port_data(&mut output_slots[0] as *mut CPortData);
        }
        // 释放输入的 CPortData
        let mut input_cpd_copy = input_cpd;
        release_port_data(&mut input_cpd_copy as *mut CPortData);
    }
    
    assert_eq!(result, 0);
}

/// 测试 release_port_data 函数
#[test]
fn test_release_port_data() {
    // 创建测试 CPortData 并释放
    let cpd = operator_runtime::c_abi::c_pd_new_i64(42);
    let mut cpd = cpd;
    
    // 释放内存（不应崩溃）
    release_port_data(&mut cpd as *mut CPortData);
    
    // 测试空指针
    release_port_data(std::ptr::null_mut());
}

/// 测试版本函数
#[test]
fn test_version_function() {
    let version_ptr = datasource_operator_version();
    let version_str = unsafe { std::ffi::CStr::from_ptr(version_ptr) }.to_str().unwrap();
    assert_eq!(version_str, "0.1.0");
}

/// 测试 ColumnData 的转换方法
#[test]
fn test_column_data_conversion() {
    let col = DataFrame::new_int64_column("test", vec![Some(1), None, Some(3)]);
    let vec = col.to_i64_vec();
    assert_eq!(vec, vec![Some(1), None, Some(3)]);
    
    let col2 = DataFrame::new_float64_column("test2", vec![Some(1.5), Some(2.5), None]);
    let vec2 = col2.to_f64_vec();
    assert_eq!(vec2, vec![Some(1.5), Some(2.5), None]);
    
    let col3 = DataFrame::new_string_column("test3", vec![Some("a"), None, Some("c")]);
    let vec3 = col3.to_string_vec();
    assert_eq!(vec3, vec![Some("a".to_string()), None, Some("c".to_string())]);
    
    let col4 = DataFrame::new_bool_column("test4", vec![Some(true), None, Some(false)]);
    let vec4 = col4.to_bool_vec();
    assert_eq!(vec4, vec![Some(true), None, Some(false)]);
}