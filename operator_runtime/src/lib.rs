pub use tokio;
pub use chrono;
pub use serde;
pub use serde_json;
use serde::{Serialize, Deserialize};

pub mod dataframe;
pub use dataframe::{DataType, ColumnData, DataFrame, PREVIEW_ROW_LIMIT};

pub mod protocol;

pub mod c_abi_dataframe;

pub mod c_abi;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PortData {
    Float(f64),
    Int(i64),
    String(String),
    Bool(bool),
    DataFrame(DataFrame),
    DataFrameArray(Vec<DataFrame>),
}

impl PortData {
    pub fn type_name(&self) -> &str {
        match self {
            PortData::Float(_) => "Float",
            PortData::Int(_) => "Int",
            PortData::String(_) => "String",
            PortData::Bool(_) => "Bool",
            PortData::DataFrame(_) => "DataFrame",
            PortData::DataFrameArray(_) => "DataFrameArray",
        }
    }

    /// 返回只包含前 `max_rows` 行的预览副本。
    ///
    /// `DataFrame`/`DataFrameArray` 按行截断；标量类型无行概念，直接克隆。
    /// 对于 `DataFrameArray`，从**第一个 DataFrame 开始累积取数**：
    ///   1. 先完整取第一个 DataFrame；
    ///   2. 若总行数不足 `max_rows`，再完整取第二个 DataFrame；
    ///   3. 依次类推，直到累计行数达到 `max_rows`，最后一个 DataFrame 截断到剩余额度；
    ///   4. 后续 DataFrame 全部舍弃。
    /// 这样预览的总数据量不超过 `max_rows`，格式仍为 `DataFrameArray`
    /// （与端口输出类型一致），前端可按 DataFrame 分页浏览累积得到的若干组。
    pub fn preview(&self, max_rows: usize) -> PortData {
        match self {
            PortData::DataFrame(df) => PortData::DataFrame(df.truncate(max_rows)),
            PortData::DataFrameArray(dfs) => {
                let mut result: Vec<DataFrame> = Vec::new();
                let mut remaining = max_rows;
                for df in dfs {
                    if remaining == 0 { break; }
                    if df.row_count <= remaining {
                        result.push(df.clone());
                        remaining -= df.row_count;
                    } else {
                        result.push(df.truncate(remaining));
                        remaining = 0;
                    }
                }
                PortData::DataFrameArray(result)
            }
            other => other.clone(),
        }
    }

    /// 返回 DataFrame 的总行数（用于预览时提示"原始 N 行，仅展示前 M 行"）。
    ///
    /// - `DataFrame`：直接返回其行数；
    /// - `DataFrameArray`：返回**所有 DataFrame 的行数之和**，因为预览阶段
    ///   从首个 DataFrame 开始累积取数，达到 [`PREVIEW_ROW_LIMIT`] 即停止，
    ///   因此需要用「总行数」来判断原始数据是否超过预览上限并给出截断提示。
    ///   无 DataFrame 则返回 None。
    pub fn first_dataframe_row_count(&self) -> Option<usize> {
        match self {
            PortData::DataFrame(df) => Some(df.row_count),
            PortData::DataFrameArray(dfs) => {
                if dfs.is_empty() { None } else { Some(dfs.iter().map(|df| df.row_count).sum()) }
            }
            _ => None,
        }
    }
}

#[export_name = "operator_runtime_version"]
pub extern "C" fn operator_runtime_version() -> *const u8 {
    "0.1.0\0".as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 辅助：构造一个包含单列 f64 数据的 DataFrame，行数为 `n`（值为 0..n）。
    fn make_df(n: usize) -> DataFrame {
        DataFrame::from_f64_vec("v", (0..n).map(|i| i as f64).collect())
    }

    /// `preview()` 对 DataFrameArray 应**从首个 DataFrame 开始累积取数**，
    /// 直到累计达到 `max_rows` 行为止；最后一个可能被截断，后续全部舍弃。
    #[test]
    fn preview_dataframe_array_accumulates_until_limit() {
        // 第1个: 1500 行 → 已达 1000 上限 → 截断到 1000，第2、3个舍弃
        let dfs = vec![make_df(1500), make_df(300), make_df(2500)];
        let pd = PortData::DataFrameArray(dfs);

        let previewed = pd.preview(1000);
        match previewed {
            PortData::DataFrameArray(out) => {
                assert_eq!(out.len(), 1, "第一个已达上限，应只保留 1 个 DataFrame");
                assert_eq!(out[0].row_count, 1000, "第一个应截断到 1000");
            }
            other => panic!("期望 DataFrameArray，得到 {:?}", other),
        }
    }

    /// 累计多行后刚好触及上限：首个 DataFrame 不满，第二个补齐，第三个舍弃。
    #[test]
    fn preview_dataframe_array_accumulates_across_multiple() {
        // 第1个 300 行，第2个 500 行 → 累计 800；第3个补 200 行到 1000。
        let dfs = vec![make_df(300), make_df(500), make_df(400), make_df(1000)];
        let pd = PortData::DataFrameArray(dfs);

        let previewed = pd.preview(1000);
        match previewed {
            PortData::DataFrameArray(out) => {
                assert_eq!(out.len(), 3, "累计到第3个才达上限，应保留 3 个 DataFrame");
                assert_eq!(out[0].row_count, 300, "第一个完整保留");
                assert_eq!(out[1].row_count, 500, "第二个完整保留");
                assert_eq!(out[2].row_count, 200, "第三个截断到剩余额度（1000-300-500=200）");
                // 总行数应刚好 1000
                let total: usize = out.iter().map(|d| d.row_count).sum();
                assert_eq!(total, 1000, "预览总行数应恰为上限");
            }
            other => panic!("期望 DataFrameArray，得到 {:?}", other),
        }
    }

    /// `preview()` 对 DataFrameArray 中行数均未超限的情况应原样保留全部。
    #[test]
    fn preview_dataframe_array_no_truncation_needed() {
        // 共 350 行 < 1000 → 全部完整保留
        let dfs = vec![make_df(100), make_df(200), make_df(50)];
        let pd = PortData::DataFrameArray(dfs);

        let previewed = pd.preview(1000);
        match previewed {
            PortData::DataFrameArray(out) => {
                assert_eq!(out.len(), 3);
                assert_eq!(out[0].row_count, 100);
                assert_eq!(out[1].row_count, 200);
                assert_eq!(out[2].row_count, 50);
                let total: usize = out.iter().map(|d| d.row_count).sum();
                assert_eq!(total, 350);
            }
            other => panic!("期望 DataFrameArray，得到 {:?}", other),
        }
    }

    /// 每个 DataFrame 都很小：累积大量 DataFrame 直到上限，后续丢弃。
    #[test]
    fn preview_dataframe_array_many_small_dfs() {
        // 构造 200 个 DataFrame，每个 10 行 → 共 2000 行；取前 100 个共 1000 行
        let dfs: Vec<DataFrame> = (0..200).map(|_| make_df(10)).collect();
        let pd = PortData::DataFrameArray(dfs);

        let previewed = pd.preview(1000);
        match previewed {
            PortData::DataFrameArray(out) => {
                assert_eq!(out.len(), 100, "每个 10 行，取 100 个达 1000 行上限");
                for df in &out {
                    assert_eq!(df.row_count, 10, "取入的 DataFrame 均应完整保留");
                }
                let total: usize = out.iter().map(|d| d.row_count).sum();
                assert_eq!(total, 1000);
            }
            other => panic!("期望 DataFrameArray，得到 {:?}", other),
        }
    }

    /// `first_dataframe_row_count()` 对 DataFrameArray 应返回**总行数之和**，
    /// 用于判断「原始总数据量是否超过预览上限」。
    #[test]
    fn first_dataframe_row_count_returns_total_sum() {
        // 300 + 2500 + 800 = 3600
        let dfs = vec![make_df(300), make_df(2500), make_df(800)];
        let pd = PortData::DataFrameArray(dfs);
        assert_eq!(pd.first_dataframe_row_count(), Some(3600));
    }

    /// 空数组应返回 None。
    #[test]
    fn first_dataframe_row_count_empty_array() {
        let pd = PortData::DataFrameArray(Vec::new());
        assert_eq!(pd.first_dataframe_row_count(), None);
    }

    /// 单个 DataFrame 的行数应原样返回。
    #[test]
    fn first_dataframe_row_count_single_df() {
        let pd = PortData::DataFrame(make_df(42));
        assert_eq!(pd.first_dataframe_row_count(), Some(42));
    }
}