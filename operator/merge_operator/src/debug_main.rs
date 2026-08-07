use operator_runtime::{DataFrame, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_to_c_owned, c_pd_free, TYPE_NULL,
};
use std::ffi::CString;

/// 构造单列 Float64 DataFrame
fn df_f64(name: &str, vals: Vec<Option<f64>>) -> DataFrame {
    let col = DataFrame::new_float64_column(name, vals);
    let mut df = DataFrame::new();
    df.add_column(col);
    df
}

/// 向 DataFrame 追加一列 Float64
fn add_f64_col(df: &mut DataFrame, name: &str, vals: Vec<Option<f64>>) {
    df.add_column(DataFrame::new_float64_column(name, vals));
}

fn main() {
    println!("=== 合并算子 Debug 运行（passthrough）===");

    // 模拟 runtime 层对象共享 + 回写后的场景：
    //   数据源 df 经过 MA/RSI/MACD 三个分支就地加列后，
    //   由于 Rc<RefCell<PortData>> 共享 + 回写，所有分支的输出已累积到同一对象上。
    //   合并算子收到的三个输入端口实际上指向同一个 DataFrame（全列）。
    //
    // 合并算子只需输出第一个非空输入即可。

    let close = vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)];

    // 构造"已累积全部列"的 DataFrame（模拟共享对象经过三分支加列后的状态）
    let mut full_df = df_f64("close", close.clone());
    add_f64_col(&mut full_df, "ma_5", vec![Some(1.0), Some(1.5), Some(2.0), Some(2.5)]);
    add_f64_col(&mut full_df, "rsi_14", vec![Some(50.0), Some(55.0), Some(60.0), Some(65.0)]);
    add_f64_col(&mut full_df, "macd", vec![Some(0.1), Some(0.2), Some(0.3), Some(0.4)]);
    add_f64_col(&mut full_df, "signal", vec![Some(0.05), Some(0.1), Some(0.15), Some(0.2)]);

    // 三个端口都指向同一份完整数据（模拟共享对象）
    let mut c_inputs: Vec<CPortData> = Vec::new();
    c_inputs.push(portdata_to_c_owned(PortData::DataFrameArray(vec![full_df.clone()])));
    c_inputs.push(portdata_to_c_owned(PortData::DataFrameArray(vec![full_df.clone()])));
    c_inputs.push(portdata_to_c_owned(PortData::DataFrameArray(vec![full_df])));

    // 第 4 个端口模拟未连接
    c_inputs.push(CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    });

    let input_count = c_inputs.len();
    let inputs_ptr = c_inputs.as_ptr();

    let mut c_outputs: Vec<CPortData> = vec![
        CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } },
        CPortData { type_tag: TYPE_NULL, value: CPortValue { str_ptr: std::ptr::null_mut() } },
    ];

    let params_json = CString::new("").unwrap();

    let rc = merge_operator::execute_operator(
            inputs_ptr,
            input_count,
            c_outputs.as_mut_ptr(),
            c_outputs.len(),
            params_json.as_ptr(),
        );

    println!("execute_operator 返回码: {}", rc);

    if rc == 0 {
        let out_pd = unsafe {
            operator_runtime::c_abi::portdata_from_c(&mut c_outputs[0])
        };
        match out_pd {
            PortData::DataFrameArray(dfs) => {
                println!("输出 DataFrame 数: {}", dfs.len());
                assert_eq!(dfs.len(), 1, "应输出 1 个 DataFrame");
                let df = &dfs[0];
                println!("  df[0]: 行数={}, 列数={}", df.row_count, df.columns.len());

                let names: Vec<&str> = df.columns.iter().map(|c| c.name.as_str()).collect();
                println!("  列名: {:?}", names);
                assert_eq!(names, vec!["close", "ma_5", "rsi_14", "macd", "signal"],
                    "应原样输出包含全部列的 DataFrame");

                for col in &df.columns {
                    println!("    {} = {:?}", col.name, col.to_f64_vec());
                }
                println!("[OK] passthrough 结果符合预期：原样输出第一个非空输入");
            }
            _ => eprintln!("输出不是 DataFrameArray"),
        }

        c_pd_free(&mut c_outputs[0]);
    }

    // 释放输入
    for mut cpd in c_inputs {
        c_pd_free(&mut cpd);
    }
}
