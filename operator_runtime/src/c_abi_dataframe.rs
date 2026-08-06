use std::ffi::{c_char, CStr, CString};
use std::ptr;

use crate::{ColumnData, DataFrame, DataType};

// ===== 数据类型标签常量 =====

pub const DTYPE_FLOAT64: u32 = 0;
pub const DTYPE_INT64: u32 = 1;
pub const DTYPE_STRING: u32 = 2;
pub const DTYPE_BOOL: u32 = 3;
pub const DTYPE_NULL: u32 = 4;

// ===== 句柄类型 =====

pub type DataFrameHandle = *mut u8;
pub type ColumnHandle = *mut u8;

// ===== C ABI 数据结构 =====

#[repr(C)]
pub struct CDataFrameArray {
    pub data: *mut DataFrameHandle,
    pub len: usize,
    pub cap: usize,
}

// ===== 内部类型 =====

struct DataFrameOpaque {
    inner: DataFrame,
}

struct ColumnOpaque {
    inner: ColumnData,
}

// ===== DataType 转换 =====

fn dtype_to_rust(dt: u32) -> DataType {
    match dt {
        DTYPE_FLOAT64 => DataType::Float64,
        DTYPE_INT64 => DataType::Int64,
        DTYPE_STRING => DataType::String,
        DTYPE_BOOL => DataType::Bool,
        _ => DataType::Null,
    }
}

pub(crate) fn dtype_from_rust(dt: &DataType) -> u32 {
    match dt {
        DataType::Float64 => DTYPE_FLOAT64,
        DataType::Int64 => DTYPE_INT64,
        DataType::String => DTYPE_STRING,
        DataType::Bool => DTYPE_BOOL,
        DataType::Null => DTYPE_NULL,
    }
}

// ===== DataFrame 生命周期 =====

#[no_mangle]
pub extern "C" fn c_df_new() -> DataFrameHandle {
    let df = DataFrame::new();
    let opaque = Box::new(DataFrameOpaque { inner: df });
    Box::into_raw(opaque) as DataFrameHandle
}

#[no_mangle]
pub extern "C" fn c_df_free(df: DataFrameHandle) {
    if df.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(df as *mut DataFrameOpaque);
    }
}

// ===== DataFrame 属性 =====

#[no_mangle]
pub extern "C" fn c_df_row_count(df: DataFrameHandle) -> usize {
    if df.is_null() {
        return 0;
    }
    unsafe {
        let opaque = &*(df as *const DataFrameOpaque);
        opaque.inner.row_count
    }
}

#[no_mangle]
pub extern "C" fn c_df_col_count(df: DataFrameHandle) -> usize {
    if df.is_null() {
        return 0;
    }
    unsafe {
        let opaque = &*(df as *const DataFrameOpaque);
        opaque.inner.columns.len()
    }
}

// ===== Column 访问 =====

#[no_mangle]
pub extern "C" fn c_df_get_col(df: DataFrameHandle, name: *const c_char) -> ColumnHandle {
    if df.is_null() || name.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let opaque = &*(df as *const DataFrameOpaque);
        let name_str = CStr::from_ptr(name).to_str().unwrap_or("");
        match opaque.inner.column(name_str) {
            Some(col) => {
                let col_clone = col.clone();
                let col_opaque = Box::new(ColumnOpaque { inner: col_clone });
                Box::into_raw(col_opaque) as ColumnHandle
            }
            None => ptr::null_mut(),
        }
    }
}

#[no_mangle]
pub extern "C" fn c_df_add_col(df: DataFrameHandle, col: ColumnHandle) {
    if df.is_null() || col.is_null() {
        return;
    }
    unsafe {
        let df_opaque = &mut *(df as *mut DataFrameOpaque);
        let col_opaque = Box::from_raw(col as *mut ColumnOpaque);
        df_opaque.inner.add_column(col_opaque.inner);
    }
}

#[no_mangle]
pub extern "C" fn c_df_col_name(col: ColumnHandle) -> *const c_char {
    if col.is_null() {
        return ptr::null();
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        let c_name = CString::new(col_opaque.inner.name.as_str()).unwrap_or_default();
        c_name.into_raw() as *const c_char
    }
}

#[no_mangle]
pub extern "C" fn c_df_col_type(col: ColumnHandle) -> u32 {
    if col.is_null() {
        return DTYPE_NULL;
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        dtype_from_rust(&col_opaque.inner.data_type)
    }
}

// ===== Column 生命周期 =====

#[no_mangle]
pub extern "C" fn c_col_new(name: *const c_char, dt: u32) -> ColumnHandle {
    let name_str = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
    };
    let col = ColumnData::new(name_str, dtype_to_rust(dt));
    let opaque = Box::new(ColumnOpaque { inner: col });
    Box::into_raw(opaque) as ColumnHandle
}

#[no_mangle]
pub extern "C" fn c_col_free(col: ColumnHandle) {
    if col.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(col as *mut ColumnOpaque);
    }
}

#[no_mangle]
pub extern "C" fn c_col_len(col: ColumnHandle) -> usize {
    if col.is_null() {
        return 0;
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        col_opaque.inner.len()
    }
}

// ===== Column push =====

#[no_mangle]
pub extern "C" fn c_col_push_f64(col: ColumnHandle, val: f64, is_null: bool) {
    if col.is_null() {
        return;
    }
    unsafe {
        let col_opaque = &mut *(col as *mut ColumnOpaque);
        col_opaque.inner.push_f64(if is_null { None } else { Some(val) });
    }
}

#[no_mangle]
pub extern "C" fn c_col_push_i64(col: ColumnHandle, val: i64, is_null: bool) {
    if col.is_null() {
        return;
    }
    unsafe {
        let col_opaque = &mut *(col as *mut ColumnOpaque);
        col_opaque.inner.push_i64(if is_null { None } else { Some(val) });
    }
}

#[no_mangle]
pub extern "C" fn c_col_push_string(col: ColumnHandle, val: *const c_char, is_null: bool) {
    if col.is_null() {
        return;
    }
    unsafe {
        let col_opaque = &mut *(col as *mut ColumnOpaque);
        let v = if is_null || val.is_null() {
            None
        } else {
            Some(CStr::from_ptr(val).to_str().unwrap_or(""))
        };
        col_opaque.inner.push_string(v);
    }
}

#[no_mangle]
pub extern "C" fn c_col_push_bool(col: ColumnHandle, val: bool, is_null: bool) {
    if col.is_null() {
        return;
    }
    unsafe {
        let col_opaque = &mut *(col as *mut ColumnOpaque);
        col_opaque.inner.push_bool(if is_null { None } else { Some(val) });
    }
}

// ===== Column get =====

#[no_mangle]
pub extern "C" fn c_col_get_f64(col: ColumnHandle, idx: usize, out_val: *mut f64) -> bool {
    if col.is_null() || out_val.is_null() {
        return false;
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        match col_opaque.inner.get_f64(idx) {
            Some(v) => {
                *out_val = v;
                true
            }
            None => false,
        }
    }
}

#[no_mangle]
pub extern "C" fn c_col_get_i64(col: ColumnHandle, idx: usize, out_val: *mut i64) -> bool {
    if col.is_null() || out_val.is_null() {
        return false;
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        match col_opaque.inner.get_i64(idx) {
            Some(v) => {
                *out_val = v;
                true
            }
            None => false,
        }
    }
}

#[no_mangle]
pub extern "C" fn c_col_get_string(
    col: ColumnHandle,
    idx: usize,
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    if col.is_null() || out_buf.is_null() || buf_len == 0 {
        return -2;
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        match col_opaque.inner.get_string(idx) {
            Some(s) => {
                let bytes = s.as_bytes();
                let copy_len = bytes.len().min(buf_len - 1);
                ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf as *mut u8, copy_len);
                *out_buf.add(copy_len) = 0;
                if bytes.len() <= buf_len - 1 {
                    0
                } else {
                    -2
                }
            }
            None => -1,
        }
    }
}

#[no_mangle]
pub extern "C" fn c_col_get_bool(col: ColumnHandle, idx: usize) -> bool {
    if col.is_null() {
        return false;
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        col_opaque.inner.get_bool(idx).unwrap_or(false)
    }
}

#[no_mangle]
pub extern "C" fn c_col_is_null(col: ColumnHandle, idx: usize) -> bool {
    if col.is_null() {
        return true;
    }
    unsafe {
        let col_opaque = &*(col as *const ColumnOpaque);
        col_opaque.inner.is_null(idx)
    }
}

// ===== DataFrameArray =====

#[no_mangle]
pub extern "C" fn c_df_array_new() -> *mut CDataFrameArray {
    let arr = Box::new(CDataFrameArray {
        data: ptr::null_mut(),
        len: 0,
        cap: 0,
    });
    Box::into_raw(arr)
}

#[no_mangle]
pub extern "C" fn c_df_array_free(arr: *mut CDataFrameArray) {
    if arr.is_null() {
        return;
    }
    unsafe {
        let arr = Box::from_raw(arr);
        for i in 0..arr.len {
            let df_handle = *arr.data.add(i);
            c_df_free(df_handle);
        }
        if !arr.data.is_null() {
            let _ = Vec::from_raw_parts(arr.data, arr.len, arr.cap);
        }
    }
}

#[no_mangle]
pub extern "C" fn c_df_array_push(arr: *mut CDataFrameArray, df: DataFrameHandle) {
    if arr.is_null() {
        c_df_free(df);
        return;
    }
    unsafe {
        let arr = &mut *arr;
        if arr.len >= arr.cap {
            let new_cap = if arr.cap == 0 { 4 } else { arr.cap * 2 };
            let mut new_data: Vec<DataFrameHandle> = Vec::with_capacity(new_cap);
            if !arr.data.is_null() {
                let old_slice = std::slice::from_raw_parts(arr.data, arr.len);
                new_data.extend_from_slice(old_slice);
                let _ = Vec::from_raw_parts(arr.data, arr.len, arr.cap);
            }
            new_data.resize(new_cap, ptr::null_mut());
            arr.data = new_data.as_mut_ptr();
            arr.cap = new_cap;
            std::mem::forget(new_data);
        }
        *arr.data.add(arr.len) = df;
        arr.len += 1;
    }
}

#[no_mangle]
pub extern "C" fn c_df_array_get(arr: *const CDataFrameArray, idx: usize) -> DataFrameHandle {
    if arr.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        if idx >= (*arr).len {
            return ptr::null_mut();
        }
        *(*arr).data.add(idx)
    }
}

#[no_mangle]
pub extern "C" fn c_df_array_len(arr: *const CDataFrameArray) -> usize {
    if arr.is_null() {
        return 0;
    }
    unsafe { (*arr).len }
}

// ===== Rust ↔ C 转换 =====

pub fn dataframe_to_c(df: DataFrame) -> DataFrameHandle {
    let opaque = Box::new(DataFrameOpaque { inner: df });
    Box::into_raw(opaque) as DataFrameHandle
}

pub unsafe fn dataframe_from_c(handle: DataFrameHandle) -> DataFrame {
    if handle.is_null() {
        return DataFrame::new();
    }
    let opaque = Box::from_raw(handle as *mut DataFrameOpaque);
    opaque.inner
}