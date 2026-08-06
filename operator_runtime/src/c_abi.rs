use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr;
use std::sync::Mutex;

use crate::PortData;

// ===== 算子错误信息传递机制 =====
// 由于算子 DLL 的 extern "C" 函数只能返回 i32 状态码，
// 我们通过全局 Mutex 存储 + C ABI getter 来传递真实错误信息。
//
// 使用全局 Mutex（而非 thread_local）的原因：
// - operator_runtime 同时以 rlib（嵌入 exe）和 cdylib（独立 DLL）存在
// - 算子 DLL 通过 prefer-dynamic 链接 cdylib 版本
// - SDK 中的 rlib 版本与算子使用的 cdylib 版本是独立的编译单元
// - thread_local 在不同实例间不共享，全局 Mutex 至少保证同一实例内可正确传递

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// 设置最后一个错误信息（算子调用）
#[no_mangle]
pub extern "C" fn c_set_last_error(msg: *const c_char) {
    let s = if msg.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() }
    };
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = s;
    }
}

/// 获取最后一个错误信息（SDK 调用）
/// 返回一个新分配的 C 字符串，调用方负责用 c_last_error_free 释放
#[no_mangle]
pub extern "C" fn c_get_last_error() -> *mut c_char {
    let msg = match LAST_ERROR.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => String::new(),
    };
    if msg.is_empty() {
        return ptr::null_mut();
    }
    match CString::new(msg) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// 释放由 c_get_last_error 返回的字符串
#[no_mangle]
pub extern "C" fn c_last_error_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe {
            let _ = CString::from_raw(ptr);
        }
    }
}

// ===== 流式算子 C ABI 契约（文档）=====
//
// 算子 DLL 可【可选】导出以下 5 个符号（与 `execute_operator` 共存），用于逐 chunk
// 流式执行。5 个符号要么全导出要么全不导出。handle 为 `*mut c_void` 不透明指针，
// 由 `stream_start` 返回，单线程内使用（服务端在单个 spawn_blocking 线程内串行调用
// start/push/next/end）。
//
//   execute_operator_stream_start(inputs: *const CPortData, input_count: usize,
//                                 params_json: *const c_char) -> *mut c_void
//     // null = 失败（用 c_get_last_error 取详情）。inputs 为物化的非流式输入。
//
//   execute_operator_stream_push(handle: *mut c_void, chunk: *const CPortData) -> i32
//     // 推入上游 chunk（源节点不会被调用）。返回 0=ok，<0=err。
//     // 【借用语义】chunk 为 *const 借用，支持一个 chunk 扇出给多个下游 transformer。
//     //   算子需保留时必须自行深拷贝；push 返回后该指针即失效，禁止跨调用持有裸指针。
//     //   服务端在 push 返回后自行调用 c_pd_free 释放 owned chunk。
//
//   execute_operator_stream_push_end(handle: *mut c_void) -> i32
//     // 通知上游已 EOF，transformer 可 flush 残留缓冲。返回 0=ok，<0=err。
//
//   execute_operator_stream_next(handle: *mut c_void, out_chunk: *mut CPortData) -> i32
//     // 三态返回：0=有 chunk（已写入 *out_chunk）；1=当前暂无更多（非永久）；<0=err。
//     //   永久结束判定 = push_end 已调用 且 next 返回 1。
//     //   三态是支持 1:1 / 1:many / many:1 chunk 比例的前提。
//     // 【输出所有权】out_chunk 由调用方预分配（TYPE_NULL 初始化）；算子在 rc==0 时
//     //   写入 owned 数据（CString/DataFrameHandle），rc==1 时不触碰。调用方取出后
//     //   无论如何 c_pd_free 一次。
//
//   execute_operator_stream_end(handle: *mut c_void)
//     // 释放 handle 及关联资源。
//
// 复用本模块的 portdata_to_c / portdata_from_c / c_pd_free 进行 CPortData 的转换与释放。


// ===== Re-export DataFrame C ABI =====
// 外部通过 use operator_runtime::c_abi::* 获取所有接口

pub use crate::c_abi_dataframe::{
    c_col_free, c_col_get_bool, c_col_get_f64, c_col_get_i64, c_col_get_string,
    c_col_is_null, c_col_len, c_col_new, c_col_push_bool, c_col_push_f64, c_col_push_i64,
    c_col_push_string, c_df_add_col, c_df_array_free, c_df_array_get, c_df_array_len,
    c_df_array_new, c_df_array_push, c_df_col_count, c_df_col_name, c_df_col_type,
    c_df_free, c_df_get_col, c_df_new, c_df_row_count, dataframe_from_c, dataframe_to_c,
    DTYPE_BOOL, DTYPE_FLOAT64, DTYPE_INT64, DTYPE_NULL, DTYPE_STRING,
    ColumnHandle, CDataFrameArray, DataFrameHandle,
};

// ===== 类型标签常量 =====

pub const TYPE_FLOAT: u32 = 0;
pub const TYPE_INT: u32 = 1;
pub const TYPE_STRING: u32 = 2;
pub const TYPE_BOOL: u32 = 3;
pub const TYPE_DATAFRAME: u32 = 4;
pub const TYPE_NULL: u32 = 5;
pub const TYPE_DATAFRAME_ARRAY: u32 = 6;

// ===== C ABI 数据结构 =====

#[repr(C)]
#[derive(Clone, Copy)]
pub union CPortValue {
    pub f64_val: f64,
    pub i64_val: i64,
    pub bool_val: u8,
    pub str_ptr: *mut c_char,
    pub df_ptr: *mut c_void,
    pub df_array_ptr: *mut CDataFrameArray,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CPortData {
    pub type_tag: u32,
    pub value: CPortValue,
}

#[repr(C)]
pub struct CPortDataArray {
    pub data: *mut CPortData,
    pub len: usize,
    pub cap: usize,
}

// ===== PortData C ABI =====

#[no_mangle]
pub extern "C" fn c_pd_new_f64(val: f64) -> CPortData {
    CPortData {
        type_tag: TYPE_FLOAT,
        value: CPortValue { f64_val: val },
    }
}

#[no_mangle]
pub extern "C" fn c_pd_new_i64(val: i64) -> CPortData {
    CPortData {
        type_tag: TYPE_INT,
        value: CPortValue { i64_val: val },
    }
}

#[no_mangle]
pub extern "C" fn c_pd_new_bool(val: bool) -> CPortData {
    CPortData {
        type_tag: TYPE_BOOL,
        value: CPortValue { bool_val: val as u8 },
    }
}

#[no_mangle]
pub extern "C" fn c_pd_new_string(val: *const c_char) -> CPortData {
    if val.is_null() {
        return CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: ptr::null_mut() },
        };
    }
    unsafe {
        let s = CStr::from_ptr(val).to_str().unwrap_or("");
        let owned = CString::new(s).unwrap_or_default();
        let ptr = owned.into_raw();
        CPortData {
            type_tag: TYPE_STRING,
            value: CPortValue { str_ptr: ptr },
        }
    }
}

#[no_mangle]
pub extern "C" fn c_pd_new_df(df: DataFrameHandle) -> CPortData {
    CPortData {
        type_tag: TYPE_DATAFRAME,
        value: CPortValue { df_ptr: df as *mut c_void },
    }
}

#[no_mangle]
pub extern "C" fn c_pd_new_df_array(arr: *mut CDataFrameArray) -> CPortData {
    CPortData {
        type_tag: TYPE_DATAFRAME_ARRAY,
        value: CPortValue { df_array_ptr: arr },
    }
}

#[no_mangle]
pub extern "C" fn c_pd_free(pd: *mut CPortData) {
    if pd.is_null() {
        return;
    }
    unsafe {
        let pd = &mut *pd;
        match pd.type_tag {
            TYPE_STRING => {
                let ptr = pd.value.str_ptr;
                if !ptr.is_null() {
                    let _ = CString::from_raw(ptr);
                }
            }
            TYPE_DATAFRAME => {
                let handle = pd.value.df_ptr;
                if !handle.is_null() {
                    c_df_free(handle as DataFrameHandle);
                }
            }
            TYPE_DATAFRAME_ARRAY => {
                let arr = pd.value.df_array_ptr;
                if !arr.is_null() {
                    c_df_array_free(arr);
                }
            }
            _ => {}
        }
        pd.type_tag = TYPE_NULL;
        pd.value = CPortValue { str_ptr: ptr::null_mut() };
    }
}

#[no_mangle]
pub extern "C" fn c_pd_type(pd: *const CPortData) -> u32 {
    if pd.is_null() {
        return TYPE_NULL;
    }
    unsafe { (*pd).type_tag }
}

#[no_mangle]
pub extern "C" fn c_pd_as_df(pd: *mut CPortData) -> DataFrameHandle {
    if pd.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let pd = &mut *pd;
        if pd.type_tag == TYPE_DATAFRAME {
            let handle = pd.value.df_ptr as DataFrameHandle;
            pd.value.df_ptr = ptr::null_mut();
            pd.type_tag = TYPE_NULL;
            handle
        } else {
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn c_pd_as_df_array(pd: *mut CPortData) -> *mut CDataFrameArray {
    if pd.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let pd = &mut *pd;
        if pd.type_tag == TYPE_DATAFRAME_ARRAY {
            let arr = pd.value.df_array_ptr;
            pd.value.df_array_ptr = ptr::null_mut();
            pd.type_tag = TYPE_NULL;
            arr
        } else {
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn c_pd_as_f64(pd: *const CPortData, out_val: *mut f64) -> bool {
    if pd.is_null() || out_val.is_null() {
        return false;
    }
    unsafe {
        if (*pd).type_tag == TYPE_FLOAT {
            *out_val = (*pd).value.f64_val;
            true
        } else {
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn c_pd_as_i64(pd: *const CPortData, out_val: *mut i64) -> bool {
    if pd.is_null() || out_val.is_null() {
        return false;
    }
    unsafe {
        if (*pd).type_tag == TYPE_INT {
            *out_val = (*pd).value.i64_val;
            true
        } else {
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn c_pd_as_bool(pd: *const CPortData, out_val: *mut bool) -> bool {
    if pd.is_null() || out_val.is_null() {
        return false;
    }
    unsafe {
        if (*pd).type_tag == TYPE_BOOL {
            *out_val = (*pd).value.bool_val != 0;
            true
        } else {
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn c_pd_as_string(pd: *const CPortData) -> *const c_char {
    if pd.is_null() {
        return ptr::null();
    }
    unsafe {
        if (*pd).type_tag == TYPE_STRING {
            (*pd).value.str_ptr as *const c_char
        } else {
            ptr::null()
        }
    }
}

// ===== PortDataArray =====

#[no_mangle]
pub extern "C" fn c_pd_array_new() -> *mut CPortDataArray {
    let arr = Box::new(CPortDataArray {
        data: ptr::null_mut(),
        len: 0,
        cap: 0,
    });
    Box::into_raw(arr)
}

#[no_mangle]
pub extern "C" fn c_pd_array_free(arr: *mut CPortDataArray) {
    if arr.is_null() {
        return;
    }
    unsafe {
        let arr = Box::from_raw(arr);
        for i in 0..arr.len {
            c_pd_free(arr.data.add(i));
        }
        if !arr.data.is_null() {
            let _ = Vec::from_raw_parts(arr.data, arr.len, arr.cap);
        }
    }
}

#[no_mangle]
pub extern "C" fn c_pd_array_push(arr: *mut CPortDataArray, pd: CPortData) {
    if arr.is_null() {
        return;
    }
    unsafe {
        let arr = &mut *arr;
        if arr.len >= arr.cap {
            let new_cap = if arr.cap == 0 { 4 } else { arr.cap * 2 };
            let mut new_data: Vec<CPortData> = Vec::with_capacity(new_cap);
            if !arr.data.is_null() {
                let old_slice = std::slice::from_raw_parts(arr.data, arr.len);
                new_data.extend_from_slice(old_slice);
                let _ = Vec::from_raw_parts(arr.data, arr.len, arr.cap);
            }
            new_data.resize(new_cap, CPortData {
                type_tag: TYPE_NULL,
                value: CPortValue { str_ptr: ptr::null_mut() },
            });
            arr.data = new_data.as_mut_ptr();
            arr.cap = new_cap;
            std::mem::forget(new_data);
        }
        *arr.data.add(arr.len) = pd;
        arr.len += 1;
    }
}

#[no_mangle]
pub extern "C" fn c_pd_array_get(arr: *const CPortDataArray, idx: usize) -> CPortData {
    if arr.is_null() {
        return CPortData {
            type_tag: TYPE_NULL,
            value: CPortValue { str_ptr: ptr::null_mut() },
        };
    }
    unsafe {
        if idx >= (*arr).len {
            return CPortData {
                type_tag: TYPE_NULL,
                value: CPortValue { str_ptr: ptr::null_mut() },
            };
        }
        ptr::read((*arr).data.add(idx))
    }
}

#[no_mangle]
pub extern "C" fn c_pd_array_len(arr: *const CPortDataArray) -> usize {
    if arr.is_null() {
        return 0;
    }
    unsafe { (*arr).len }
}

// ===== Rust ↔ C 转换 =====

pub fn portdata_to_c(pd: &PortData) -> CPortData {
    match pd {
        PortData::Float(v) => CPortData {
            type_tag: TYPE_FLOAT,
            value: CPortValue { f64_val: *v },
        },
        PortData::Int(v) => CPortData {
            type_tag: TYPE_INT,
            value: CPortValue { i64_val: *v },
        },
        PortData::Bool(v) => CPortData {
            type_tag: TYPE_BOOL,
            value: CPortValue { bool_val: *v as u8 },
        },
        PortData::String(s) => {
            let owned = CString::new(s.as_str()).unwrap_or_default();
            let ptr = owned.into_raw();
            CPortData {
                type_tag: TYPE_STRING,
                value: CPortValue { str_ptr: ptr },
            }
        }
        PortData::DataFrame(df) => {
            let handle = dataframe_to_c(df.clone());
            CPortData {
                type_tag: TYPE_DATAFRAME,
                value: CPortValue { df_ptr: handle as *mut c_void },
            }
        }
        PortData::DataFrameArray(dfs) => {
            let arr = c_df_array_new();
            for df in dfs {
                let handle = dataframe_to_c(df.clone());
                c_df_array_push(arr, handle);
            }
            CPortData {
                type_tag: TYPE_DATAFRAME_ARRAY,
                value: CPortValue { df_array_ptr: arr },
            }
        }
    }
}

/// 消费版本的 [`portdata_to_c`]：直接拿走 PortData 内部数据所有权，
/// 避免 DataFrame / DataFrameArray 在转换时被 `clone()`。
///
/// 算子输出时 PortData 已为 owned 值，用本函数可省去逐表深拷贝。
pub fn portdata_to_c_owned(pd: PortData) -> CPortData {
    match pd {
        PortData::Float(v) => CPortData {
            type_tag: TYPE_FLOAT,
            value: CPortValue { f64_val: v },
        },
        PortData::Int(v) => CPortData {
            type_tag: TYPE_INT,
            value: CPortValue { i64_val: v },
        },
        PortData::Bool(v) => CPortData {
            type_tag: TYPE_BOOL,
            value: CPortValue { bool_val: v as u8 },
        },
        PortData::String(s) => {
            let owned = CString::new(s).unwrap_or_default();
            let ptr = owned.into_raw();
            CPortData {
                type_tag: TYPE_STRING,
                value: CPortValue { str_ptr: ptr },
            }
        }
        PortData::DataFrame(df) => {
            let handle = dataframe_to_c(df);
            CPortData {
                type_tag: TYPE_DATAFRAME,
                value: CPortValue { df_ptr: handle as *mut c_void },
            }
        }
        PortData::DataFrameArray(dfs) => {
            let arr = c_df_array_new();
            for df in dfs {
                let handle = dataframe_to_c(df);
                c_df_array_push(arr, handle);
            }
            CPortData {
                type_tag: TYPE_DATAFRAME_ARRAY,
                value: CPortValue { df_array_ptr: arr },
            }
        }
    }
}

pub unsafe fn portdata_from_c(pd: *mut CPortData) -> PortData {
    if pd.is_null() {
        return PortData::Float(0.0);
    }
    let pd = &mut *pd;
    match pd.type_tag {
        TYPE_FLOAT => {
            let v = pd.value.f64_val;
            pd.type_tag = TYPE_NULL;
            PortData::Float(v)
        }
        TYPE_INT => {
            let v = pd.value.i64_val;
            pd.type_tag = TYPE_NULL;
            PortData::Int(v)
        }
        TYPE_BOOL => {
            let v = pd.value.bool_val != 0;
            pd.type_tag = TYPE_NULL;
            PortData::Bool(v)
        }
        TYPE_STRING => {
            let ptr = pd.value.str_ptr;
            pd.type_tag = TYPE_NULL;
            pd.value = CPortValue { str_ptr: ptr::null_mut() };
            if ptr.is_null() {
                PortData::String(String::new())
            } else {
                let s = CStr::from_ptr(ptr).to_string_lossy().into_owned();
                let _ = CString::from_raw(ptr);
                PortData::String(s)
            }
        }
        TYPE_DATAFRAME => {
            let handle = pd.value.df_ptr as DataFrameHandle;
            pd.type_tag = TYPE_NULL;
            pd.value = CPortValue { df_ptr: ptr::null_mut() };
            if handle.is_null() {
                PortData::DataFrame(crate::DataFrame::new())
            } else {
                PortData::DataFrame(dataframe_from_c(handle))
            }
        }
        TYPE_DATAFRAME_ARRAY => {
            let arr = pd.value.df_array_ptr;
            pd.type_tag = TYPE_NULL;
            pd.value = CPortValue { df_array_ptr: ptr::null_mut() };
            if arr.is_null() {
                PortData::DataFrameArray(Vec::new())
            } else {
                let arr_box = Box::from_raw(arr);
                let mut dfs = Vec::with_capacity(arr_box.len);
                for i in 0..arr_box.len {
                    let handle = *arr_box.data.add(i);
                    if !handle.is_null() {
                        dfs.push(dataframe_from_c(handle));
                    }
                }
                if !arr_box.data.is_null() {
                    let _ = Vec::from_raw_parts(arr_box.data, arr_box.len, arr_box.cap);
                }
                PortData::DataFrameArray(dfs)
            }
        }
        _ => PortData::Float(0.0),
    }
}

pub fn portdata_vec_to_c_portdata_array(pds: Vec<PortData>) -> *mut CPortDataArray {
    let arr = Box::new(CPortDataArray {
        data: ptr::null_mut(),
        len: 0,
        cap: 0,
    });
    let arr_ptr = Box::into_raw(arr);
    for pd in pds {
        let c_pd = portdata_to_c(&pd);
        c_pd_array_push(arr_ptr, c_pd);
    }
    arr_ptr
}

pub unsafe fn portdata_array_to_portdata_vec(arr: *mut CPortDataArray) -> Vec<PortData> {
    if arr.is_null() {
        return Vec::new();
    }
    let arr_box = Box::from_raw(arr);
    let mut result = Vec::with_capacity(arr_box.len);
    for i in 0..arr_box.len {
        let pd_ptr = arr_box.data.add(i);
        result.push(portdata_from_c(pd_ptr));
    }
    if !arr_box.data.is_null() {
        let _ = Vec::from_raw_parts(arr_box.data, arr_box.len, arr_box.cap);
    }
    result
}