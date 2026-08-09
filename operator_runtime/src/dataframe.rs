use serde::{Serialize, Deserialize};

/// 节点输出预览时最多保留的行数。服务端序列化 DAG 结果时按此截断，
/// 避免大数据量经网络回传（完整数据保留在服务端内存中供下游算子使用）。
pub const PREVIEW_ROW_LIMIT: usize = 200;

mod base64_bytes {
    use serde::{self, Serialize, Serializer, Deserializer, Deserialize};
    use base64::{Engine as _, engine::general_purpose};

    pub fn serialize<S>(data: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer
    {
        let s = general_purpose::STANDARD.encode(data);
        String::serialize(&s, serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where D: Deserializer<'de>
    {
        let s = String::deserialize(deserializer)?;
        general_purpose::STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DataType {
    Float64,
    Int64,
    String,
    Bool,
    Null,
}

impl DataType {
    pub fn size_of(&self) -> usize {
        match self {
            DataType::Float64 => std::mem::size_of::<f64>(),
            DataType::Int64 => std::mem::size_of::<i64>(),
            DataType::Bool => std::mem::size_of::<bool>(),
            DataType::String => 0,
            DataType::Null => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnData {
    pub name: String,
    pub data_type: DataType,
    #[serde(with = "base64_bytes")]
    pub values: Vec<u8>,
    pub offsets: Vec<usize>,
    #[serde(with = "base64_bytes")]
    pub null_bitmap: Vec<u8>,
    pub null_count: usize,
}

impl ColumnData {
    pub fn new(name: String, data_type: DataType) -> Self {
        ColumnData {
            name,
            data_type,
            values: Vec::new(),
            offsets: vec![0],
            null_bitmap: Vec::new(),
            null_count: 0,
        }
    }

    pub fn len(&self) -> usize {
        match self.data_type {
            DataType::String => self.offsets.len() - 1,
            _ => self.values.len() / self.data_type.size_of(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 构造只包含前 `n` 行的列副本。行数不超过 `n` 时直接克隆，避免无谓拷贝。
    ///
    /// 所有切片均带边界保护，畸形数据（offsets/bitmap 不足）退化为克隆，不会 panic。
    pub fn truncate(&self, n: usize) -> ColumnData {
        if n >= self.len() {
            return self.clone();
        }
        let mut new_col = ColumnData {
            name: self.name.clone(),
            data_type: self.data_type.clone(),
            values: Vec::new(),
            offsets: Vec::new(),
            null_bitmap: Vec::new(),
            null_count: 0,
        };
        let take_bitmap = n.min(self.null_bitmap.len());
        match self.data_type {
            DataType::Float64 | DataType::Int64 => {
                let elem = self.data_type.size_of();
                let take_bytes = (n * elem).min(self.values.len());
                new_col.values = self.values[..take_bytes].to_vec();
                new_col.null_bitmap = self.null_bitmap[..take_bitmap].to_vec();
            }
            DataType::Bool => {
                let take_bytes = n.min(self.values.len());
                new_col.values = self.values[..take_bytes].to_vec();
                new_col.null_bitmap = self.null_bitmap[..take_bitmap].to_vec();
            }
            DataType::String => {
                // offsets 长度 = 行数 + 1；前 n 行需要 offsets[0..=n] 共 n+1 个。
                if n + 1 <= self.offsets.len() {
                    new_col.offsets = self.offsets[..=n].to_vec();
                    let end = self.offsets[n].min(self.values.len());
                    new_col.values = self.values[..end].to_vec();
                } else {
                    new_col.offsets = self.offsets.clone();
                    new_col.values = self.values.clone();
                }
                new_col.null_bitmap = self.null_bitmap[..take_bitmap].to_vec();
            }
            DataType::Null => {
                new_col.null_bitmap = self.null_bitmap[..take_bitmap].to_vec();
            }
        }
        new_col.null_count = new_col.null_bitmap.iter().filter(|b| **b != 0).count();
        new_col
    }

    pub fn push_f64(&mut self, value: Option<f64>) {
        if self.data_type != DataType::Float64 {
            panic!("Column data type mismatch: expected Float64");
        }
        if let Some(v) = value {
            self.values.extend_from_slice(&v.to_ne_bytes());
            self.null_bitmap.push(0);
        } else {
            self.values.extend_from_slice(&0u64.to_ne_bytes());
            self.null_bitmap.push(1);
            self.null_count += 1;
        }
    }

    pub fn push_i64(&mut self, value: Option<i64>) {
        if self.data_type != DataType::Int64 {
            panic!("Column data type mismatch: expected Int64");
        }
        if let Some(v) = value {
            self.values.extend_from_slice(&v.to_ne_bytes());
            self.null_bitmap.push(0);
        } else {
            self.values.extend_from_slice(&0i64.to_ne_bytes());
            self.null_bitmap.push(1);
            self.null_count += 1;
        }
    }

    pub fn push_string(&mut self, value: Option<&str>) {
        if self.data_type != DataType::String {
            panic!("Column data type mismatch: expected String");
        }
        if let Some(v) = value {
            self.values.extend_from_slice(v.as_bytes());
            self.offsets.push(self.values.len());
            self.null_bitmap.push(0);
        } else {
            self.offsets.push(self.offsets.last().copied().unwrap_or(0));
            self.null_bitmap.push(1);
            self.null_count += 1;
        }
    }

    pub fn push_bool(&mut self, value: Option<bool>) {
        if self.data_type != DataType::Bool {
            panic!("Column data type mismatch: expected Bool");
        }
        if let Some(v) = value {
            self.values.push(v as u8);
            self.null_bitmap.push(0);
        } else {
            self.values.push(0);
            self.null_bitmap.push(1);
            self.null_count += 1;
        }
    }

    pub fn get_f64(&self, index: usize) -> Option<f64> {
        if self.data_type != DataType::Float64 {
            return None;
        }
        if self.is_null(index) {
            return None;
        }
        let offset = index * std::mem::size_of::<f64>();
        if offset + std::mem::size_of::<f64>() > self.values.len() {
            return None;
        }
        let bytes: [u8; 8] = self.values[offset..offset + 8].try_into().ok()?;
        Some(f64::from_ne_bytes(bytes))
    }

    pub fn get_i64(&self, index: usize) -> Option<i64> {
        if self.data_type != DataType::Int64 {
            return None;
        }
        if self.is_null(index) {
            return None;
        }
        let offset = index * std::mem::size_of::<i64>();
        if offset + std::mem::size_of::<i64>() > self.values.len() {
            return None;
        }
        let bytes: [u8; 8] = self.values[offset..offset + 8].try_into().ok()?;
        Some(i64::from_ne_bytes(bytes))
    }

    pub fn get_string(&self, index: usize) -> Option<&str> {
        if self.data_type != DataType::String {
            return None;
        }
        if self.is_null(index) {
            return None;
        }
        if index + 1 >= self.offsets.len() {
            return None;
        }
        let start = self.offsets[index];
        let end = self.offsets[index + 1];
        if end > self.values.len() {
            return None;
        }
        std::str::from_utf8(&self.values[start..end]).ok()
    }

    pub fn get_bool(&self, index: usize) -> Option<bool> {
        if self.data_type != DataType::Bool {
            return None;
        }
        if self.is_null(index) {
            return None;
        }
        if index >= self.values.len() {
            return None;
        }
        Some(self.values[index] != 0)
    }

    pub fn is_null(&self, index: usize) -> bool {
        if index >= self.null_bitmap.len() {
            return true;
        }
        self.null_bitmap[index] != 0
    }

    pub fn to_f64_vec(&self) -> Vec<Option<f64>> {
        if self.data_type != DataType::Float64 {
            return Vec::new();
        }
        let n = self.len();
        let mut result = Vec::with_capacity(n);
        // 批量提取：用 chunks_exact(8) 一次性遍历 values 字节流，
        // 避免逐元素调用 get_f64 时的重复类型检查 + 偏移计算 + 边界检查。
        let bitmap = &self.null_bitmap;
        for (i, chunk) in self.values.chunks_exact(8).enumerate() {
            let is_null = i < bitmap.len() && bitmap[i] != 0;
            if is_null {
                result.push(None);
            } else {
                let bytes: [u8; 8] = chunk.try_into().unwrap();
                result.push(Some(f64::from_ne_bytes(bytes)));
            }
        }
        // values 长度不是 8 的倍数时的兜底（正常不应发生）
        result
    }

    pub fn to_i64_vec(&self) -> Vec<Option<i64>> {
        if self.data_type != DataType::Int64 {
            return Vec::new();
        }
        let n = self.len();
        let mut result = Vec::with_capacity(n);
        let bitmap = &self.null_bitmap;
        for (i, chunk) in self.values.chunks_exact(8).enumerate() {
            let is_null = i < bitmap.len() && bitmap[i] != 0;
            if is_null {
                result.push(None);
            } else {
                let bytes: [u8; 8] = chunk.try_into().unwrap();
                result.push(Some(i64::from_ne_bytes(bytes)));
            }
        }
        result
    }

    pub fn to_string_vec(&self) -> Vec<Option<String>> {
        (0..self.len()).map(|i| self.get_string(i).map(|s| s.to_string())).collect()
    }

    pub fn to_bool_vec(&self) -> Vec<Option<bool>> {
        (0..self.len()).map(|i| self.get_bool(i)).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataFrame {
    pub columns: Vec<ColumnData>,
    pub row_count: usize,
}

impl DataFrame {
    pub fn new() -> Self {
        DataFrame {
            columns: Vec::new(),
            row_count: 0,
        }
    }

    pub fn column(&self, name: &str) -> Option<&ColumnData> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_mut(&mut self, name: &str) -> Option<&mut ColumnData> {
        self.columns.iter_mut().find(|c| c.name == name)
    }

    pub fn add_column(&mut self, column: ColumnData) {
        if !self.columns.is_empty() && column.len() != self.row_count {
            panic!("Column length mismatch: expected {}, got {}", self.row_count, column.len());
        }
        if self.columns.is_empty() {
            self.row_count = column.len();
        }
        self.columns.push(column);
    }

    pub fn new_float64_column(name: &str, values: Vec<Option<f64>>) -> ColumnData {
        let n = values.len();
        let mut col = ColumnData::new(name.to_string(), DataType::Float64);
        // 预分配 exact 容量，一次性写入，避免逐元素 push_f64 的反复 extend_from_slice
        col.values.reserve_exact(n * std::mem::size_of::<f64>());
        col.null_bitmap.reserve_exact(n);
        for v in &values {
            let val = v.unwrap_or(0.0);
            col.values.extend_from_slice(&val.to_ne_bytes());
            col.null_bitmap.push(if v.is_none() { 1 } else { 0 });
        }
        col.null_count = values.iter().filter(|v| v.is_none()).count();
        col
    }

    pub fn new_int64_column(name: &str, values: Vec<Option<i64>>) -> ColumnData {
        let mut col = ColumnData::new(name.to_string(), DataType::Int64);
        for v in values {
            col.push_i64(v);
        }
        col
    }

    pub fn new_string_column(name: &str, values: Vec<Option<&str>>) -> ColumnData {
        let mut col = ColumnData::new(name.to_string(), DataType::String);
        for v in values {
            col.push_string(v);
        }
        col
    }

    pub fn new_bool_column(name: &str, values: Vec<Option<bool>>) -> ColumnData {
        let mut col = ColumnData::new(name.to_string(), DataType::Bool);
        for v in values {
            col.push_bool(v);
        }
        col
    }

    pub fn from_f64_vec(name: &str, values: Vec<f64>) -> Self {
        let col = Self::new_float64_column(name, values.into_iter().map(Some).collect());
        let mut table = DataFrame::new();
        table.add_column(col);
        table
    }

    pub fn col_count(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    /// 构造只包含前 `max_rows` 行的 DataFrame 副本。行数不超过阈值时直接克隆。
    pub fn truncate(&self, max_rows: usize) -> DataFrame {
        if self.row_count <= max_rows {
            return self.clone();
        }
        let n = max_rows.min(self.row_count);
        let columns = self.columns.iter().map(|c| c.truncate(n)).collect();
        DataFrame { columns, row_count: n }
    }

    /// 按指定列对 DataFrame 行进行排序，返回新的排序后 DataFrame。
    ///
    /// `descending` 为 true 时降序，false 时升序。空值排到末尾。
    pub fn sort_by_column(&self, column_name: &str, descending: bool) -> DataFrame {
        if self.row_count == 0 {
            return self.clone();
        }
        let col = match self.column(column_name) {
            Some(c) => c,
            None => return self.clone(),
        };

        // 生成行索引并按列值排序
        let mut indices: Vec<usize> = (0..self.row_count).collect();

        // 比较函数：返回 Ordering，空值视为最大（排末尾）
        macro_rules! cmp_values {
            ($getter:ident, $a:expr, $b:expr) => {
                match (col.$getter($a), col.$getter($b)) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (Some(va), Some(vb)) => va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal),
                }
            };
        }

        indices.sort_by(|&a, &b| {
            let ord = match col.data_type {
                DataType::Float64 => cmp_values!(get_f64, a, b),
                DataType::Int64 => cmp_values!(get_i64, a, b),
                DataType::String => {
                    match (col.get_string(a), col.get_string(b)) {
                        (None, None) => std::cmp::Ordering::Equal,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (Some(va), Some(vb)) => va.cmp(vb),
                    }
                }
                DataType::Bool => {
                    match (col.get_bool(a), col.get_bool(b)) {
                        (None, None) => std::cmp::Ordering::Equal,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (Some(va), Some(vb)) => va.cmp(&vb),
                    }
                }
                DataType::Null => std::cmp::Ordering::Equal,
            };
            if descending { ord.reverse() } else { ord }
        });

        // 根据排好序的索引重建各列
        let new_columns: Vec<ColumnData> = self.columns.iter().map(|c| {
            let mut new_col = ColumnData::new(c.name.clone(), c.data_type.clone());
            for &idx in &indices {
                match c.data_type {
                    DataType::Float64 => new_col.push_f64(c.get_f64(idx)),
                    DataType::Int64 => new_col.push_i64(c.get_i64(idx)),
                    DataType::String => new_col.push_string(c.get_string(idx)),
                    DataType::Bool => new_col.push_bool(c.get_bool(idx)),
                    DataType::Null => {}
                }
            }
            new_col
        }).collect();

        DataFrame {
            columns: new_columns,
            row_count: self.row_count,
        }
    }

    /// 按指定列的值对 DataFrame 行进行分组，返回分组后的多个 DataFrame。
    ///
    /// 分组顺序与该列值首次出现的顺序一致。
    pub fn group_by_column(&self, column_name: &str) -> Vec<DataFrame> {
        if self.row_count == 0 {
            return Vec::new();
        }
        let col = match self.column(column_name) {
            Some(c) => c,
            None => return vec![self.clone()],
        };

        // 收集每行的分组键（字符串形式），保持出现顺序
        let mut group_order: Vec<String> = Vec::new();
        let mut group_indices: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();

        for i in 0..self.row_count {
            let key = match col.data_type {
                DataType::Float64 => col.get_f64(i).map(|v| format!("{:?}", v)).unwrap_or_else(|| "__NULL__".to_string()),
                DataType::Int64 => col.get_i64(i).map(|v| format!("{}", v)).unwrap_or_else(|| "__NULL__".to_string()),
                DataType::String => col.get_string(i).map(|s| s.to_string()).unwrap_or_else(|| "__NULL__".to_string()),
                DataType::Bool => col.get_bool(i).map(|v| format!("{}", v)).unwrap_or_else(|| "__NULL__".to_string()),
                DataType::Null => "__NULL__".to_string(),
            };
            if !group_indices.contains_key(&key) {
                group_order.push(key.clone());
            }
            group_indices.entry(key).or_default().push(i);
        }

        // 按分组顺序生成各 DataFrame
        group_order.iter().filter_map(|key| {
            let indices = group_indices.get(key)?;
            if indices.is_empty() {
                return None;
            }
            let new_columns: Vec<ColumnData> = self.columns.iter().map(|c| {
                let mut new_col = ColumnData::new(c.name.clone(), c.data_type.clone());
                for &idx in indices {
                    match c.data_type {
                        DataType::Float64 => new_col.push_f64(c.get_f64(idx)),
                        DataType::Int64 => new_col.push_i64(c.get_i64(idx)),
                        DataType::String => new_col.push_string(c.get_string(idx)),
                        DataType::Bool => new_col.push_bool(c.get_bool(idx)),
                        DataType::Null => {}
                    }
                }
                new_col
            }).collect();
            Some(DataFrame {
                columns: new_columns,
                row_count: indices.len(),
            })
        }).collect()
    }

    /// 根据给定的行索引集合构造新的 DataFrame（子集）。
    pub fn take_rows(&self, indices: &[usize]) -> DataFrame {
        if indices.is_empty() {
            return DataFrame::new();
        }
        let new_columns: Vec<ColumnData> = self.columns.iter().map(|c| {
            let mut new_col = ColumnData::new(c.name.clone(), c.data_type.clone());
            for &idx in indices {
                if idx >= self.row_count { continue; }
                match c.data_type {
                    DataType::Float64 => new_col.push_f64(c.get_f64(idx)),
                    DataType::Int64 => new_col.push_i64(c.get_i64(idx)),
                    DataType::String => new_col.push_string(c.get_string(idx)),
                    DataType::Bool => new_col.push_bool(c.get_bool(idx)),
                    DataType::Null => {}
                }
            }
            new_col
        }).collect();
        let row_count = new_columns.first().map(|c| c.len()).unwrap_or(0);
        DataFrame { columns: new_columns, row_count }
    }
}