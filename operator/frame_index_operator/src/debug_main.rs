use operator_runtime::c_abi::{c_pd_free, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, PortData};
use std::ffi::CString;

/// 构造单列 Float64 DataFrame
fn df_f64(name: &str, vals: Vec<Option<f64>>) -> DataFrame {
    let col = DataFrame::new_float64_column(name, vals);
    let mut df = DataFrame::new();
    df.add_column(col);
    df
}

/// 用指定参数 JSON 跑一次取帧，成功时打印帧信息并校验 close 列首值
fn run_once(frames: Vec<DataFrame>, params: &str, expect_first: f64) {
    println!("--- params={} ---", params);
    let mut c_input = portdata_to_c_owned(PortData::DataFrameArray(frames));
    let mut c_outputs: Vec<CPortData> = vec![CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue {
            str_ptr: std::ptr::null_mut(),
        },
    }];
    let params_json = CString::new(params).unwrap();

    let rc = frame_index_operator::execute_operator(
        &mut c_input as *mut CPortData,
        1,
        c_outputs.as_mut_ptr(),
        c_outputs.len(),
        params_json.as_ptr(),
    );
    println!("execute_operator 返回码: {}", rc);
    assert_eq!(rc, 0, "取帧应成功");

    let out = unsafe { portdata_from_c(&mut c_outputs[0]) };
    match out {
        PortData::DataFrame(df) => {
            println!(
                "  输出 DataFrame: {} 行 × {} 列",
                df.row_count,
                df.columns.len()
            );
            for col in &df.columns {
                println!("    {} = {:?}", col.name, col.to_f64_vec());
            }
            let first = df.columns[0].get_f64(0).unwrap();
            assert!((first - expect_first).abs() < 1e-9, "期望首值 {}", expect_first);
            println!("[OK] 取到的帧首值 = {}", first);
        }
        other => panic!("期望输出 DataFrame，得到 {:?}", other),
    }

    c_pd_free(&mut c_outputs[0]);
    c_pd_free(&mut c_input);
}

fn main() {
    println!("=== 数组取帧算子 Debug 运行 ===");

    let make_frames = || {
        vec![
            df_f64("close", vec![Some(1.0), Some(1.1)]),
            df_f64("close", vec![Some(2.0), Some(2.1)]),
            df_f64("close", vec![Some(3.0), Some(3.1)]),
        ]
    };

    // 正向下标：取第 2 帧
    run_once(make_frames(), r#"{"frame_index":1}"#, 2.0);
    // 负向下标：-1 取最后一帧
    run_once(make_frames(), r#"{"frame_index":-1}"#, 3.0);
    // 缺省参数：默认第 1 帧
    run_once(make_frames(), "{}", 1.0);

    // 越界场景：应返回 -6
    println!("--- 越界场景 params={{\"frame_index\":9}} ---");
    let mut c_input = portdata_to_c_owned(PortData::DataFrameArray(make_frames()));
    let mut c_outputs: Vec<CPortData> = vec![CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue {
            str_ptr: std::ptr::null_mut(),
        },
    }];
    let params_json = CString::new(r#"{"frame_index":9}"#).unwrap();
    let rc = frame_index_operator::execute_operator(
        &mut c_input as *mut CPortData,
        1,
        c_outputs.as_mut_ptr(),
        c_outputs.len(),
        params_json.as_ptr(),
    );
    println!("execute_operator 返回码: {}（期望 -6 越界）", rc);
    assert_eq!(rc, -6);
    c_pd_free(&mut c_input);

    println!("全部 Debug 断言通过。");
}
