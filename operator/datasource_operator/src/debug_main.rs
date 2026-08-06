use datasource_operator::{execute_operator, release_port_data};
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{portdata_to_c, portdata_from_c, CPortData};
use std::ffi::CString;

fn main() {
    let mut input_df = DataFrame::new();
    let col1 = DataFrame::new_int64_column("id", vec![Some(1), Some(2), Some(3), Some(4), Some(5)]);
    let col2 = DataFrame::new_float64_column("price", vec![Some(12.50), Some(15.30), Some(8.90), Some(22.10), Some(5.40)]);
    let col3 = DataFrame::new_string_column("symbol", vec![Some("AAPL"), Some("GOOGL"), Some("MSFT"), Some("AMZN"), Some("TSLA")]);
    input_df.add_column(col1);
    input_df.add_column(col2);
    input_df.add_column(col3);

    let input_port = PortData::DataFrame(input_df);
    let c_input = portdata_to_c(&input_port);
    let c_inputs = [c_input];

    let json = r#"{
        "host": "localhost",
        "port": 5432,
        "database": "whatigo",
        "username": "postgres",
        "password": "difyai123456",
        "query": "select open,high,low,close from tushare_daily limit 10000"
    }"#;

    let params_cstr = CString::new(json).unwrap_or_default();

    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: operator_runtime::c_abi::TYPE_NULL,
        value: operator_runtime::c_abi::CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let result = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );

    println!("execute_operator returned: {}", result);

    if output_slots[0].type_tag != operator_runtime::c_abi::TYPE_NULL {
        let output_pd = unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) };
        match output_pd {
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
    } else {
        println!("输出指针为空（数据库可能未连接）");
    }

    // 释放输入
    let mut c_input_owned = c_input;
    release_port_data(&mut c_input_owned as *mut CPortData);
}