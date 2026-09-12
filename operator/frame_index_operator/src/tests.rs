use super::*;
use operator_runtime::c_abi::{portdata_from_c, portdata_to_c_owned, CPortData, CPortValue};
use operator_runtime::DataFrame;
use std::ffi::CString;

/// 构造单列 Float64 DataFrame，列值用于标识帧身份
fn df_f64(name: &str, vals: Vec<Option<f64>>) -> DataFrame {
    let col = DataFrame::new_float64_column(name, vals);
    let mut df = DataFrame::new();
    df.add_column(col);
    df
}

/// 构造 3 帧 DataFrameArray，每帧 close 列值不同以便识别
fn three_frames() -> Vec<DataFrame> {
    vec![
        df_f64("close", vec![Some(1.0), Some(1.1)]),
        df_f64("close", vec![Some(2.0), Some(2.1)]),
        df_f64("close", vec![Some(3.0), Some(3.1)]),
    ]
}

/// 调用 execute_operator 并返回 (返回码, 输出 PortData)
fn run(input: CPortData, params: &str) -> (i32, Option<PortData>) {
    let mut c_outputs: Vec<CPortData> = vec![
        CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue {
                str_ptr: std::ptr::null_mut(),
            },
        },
        CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue {
                str_ptr: std::ptr::null_mut(),
            },
        },
    ];
    let params_json = CString::new(params).unwrap();
    let rc = execute_operator(
        &input as *const CPortData,
        1,
        c_outputs.as_mut_ptr(),
        c_outputs.len(),
        params_json.as_ptr(),
    );
    let out = if rc == 0 {
        Some(unsafe { portdata_from_c(&mut c_outputs[0]) })
    } else {
        None
    };
    (rc, out)
}

fn null_input() -> CPortData {
    CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue {
            str_ptr: std::ptr::null_mut(),
        },
    }
}

/// 断言输出为 DataFrame 且 close 列等于预期
fn assert_frame(out: Option<PortData>, expected: Vec<Option<f64>>) {
    match out.unwrap() {
        PortData::DataFrame(df) => {
            assert_eq!(df.columns.len(), 1);
            assert_eq!(df.columns[0].name, "close");
            assert_eq!(df.columns[0].to_f64_vec(), expected);
        }
        other => panic!("期望 DataFrame，得到 {:?}", other),
    }
}

#[test]
fn take_first_frame() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":0}"#);
    assert_eq!(rc, 0);
    assert_frame(out, vec![Some(1.0), Some(1.1)]);
}

#[test]
fn take_middle_frame() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":1}"#);
    assert_eq!(rc, 0);
    assert_frame(out, vec![Some(2.0), Some(2.1)]);
}

#[test]
fn take_last_with_negative_one() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":-1}"#);
    assert_eq!(rc, 0);
    assert_frame(out, vec![Some(3.0), Some(3.1)]);
}

#[test]
fn negative_index_counts_backward() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":-3}"#);
    assert_eq!(rc, 0);
    assert_frame(out, vec![Some(1.0), Some(1.1)]);
}

#[test]
fn missing_param_defaults_to_zero() {
    // 空串 / 缺字段 / null 均应默认取下标 0
    for params in ["", "{}", r#"{"frame_index":null}"#] {
        let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
        let (rc, out) = run(input, params);
        assert_eq!(rc, 0, "params={:?} 应成功并默认取下标 0", params);
        assert_frame(out, vec![Some(1.0), Some(1.1)]);
    }
}

#[test]
fn string_numeric_param_is_accepted() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":"2"}"#);
    assert_eq!(rc, 0);
    assert_frame(out, vec![Some(3.0), Some(3.1)]);
}

#[test]
fn positive_index_out_of_range_is_error() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":3}"#);
    assert_eq!(rc, -6);
    assert!(out.is_none());
}

#[test]
fn negative_index_out_of_range_is_error() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":-4}"#);
    assert_eq!(rc, -6);
    assert!(out.is_none());
}

#[test]
fn empty_array_is_error() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(vec![]));
    let (rc, out) = run(input, r#"{"frame_index":0}"#);
    assert_eq!(rc, -5);
    assert!(out.is_none());
}

#[test]
fn single_dataframe_input_is_type_error() {
    let input = portdata_to_c_owned(PortData::DataFrame(df_f64(
        "close",
        vec![Some(1.0)],
    )));
    let (rc, out) = run(input, r#"{"frame_index":0}"#);
    assert_eq!(rc, -4);
    assert!(out.is_none());
}

#[test]
fn null_input_is_missing_error() {
    let (rc, out) = run(null_input(), r#"{"frame_index":0}"#);
    assert_eq!(rc, -3);
    assert!(out.is_none());
}

#[test]
fn invalid_param_type_is_error() {
    let input = portdata_to_c_owned(PortData::DataFrameArray(three_frames()));
    let (rc, out) = run(input, r#"{"frame_index":true}"#);
    assert_eq!(rc, -2);
    assert!(out.is_none());
}

#[test]
fn resolve_index_rules() {
    assert_eq!(resolve_index(0, 3).unwrap(), 0);
    assert_eq!(resolve_index(2, 3).unwrap(), 2);
    assert_eq!(resolve_index(-1, 3).unwrap(), 2);
    assert_eq!(resolve_index(-3, 3).unwrap(), 0);
    assert!(resolve_index(3, 3).is_err());
    assert!(resolve_index(-4, 3).is_err());
}
