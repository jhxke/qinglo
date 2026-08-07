use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::{Once, RwLock};
use std::time::{Duration, Instant};
use operator_executor_client::PortData;
use operator_executor_client::protocol::{
    OperatorCategory, OperatorInfo as ProtoOperatorInfo,
    OperatorPortParamDef as ProtoPortParamDef,
    OperatorExecutionStatus, ExecutionLogEntry, OperatorExecutionResult,
};

/// 参数类型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamType {
    Float,
    Int,
    String,
    Bool,
    DataFrame,
    DataFrameArray,
}

impl ParamType {
    pub fn to_str(&self) -> &str {
        match self {
            ParamType::Float => "浮点",
            ParamType::Int => "整数",
            ParamType::String => "字符串",
            ParamType::Bool => "布尔",
            ParamType::DataFrame => "DataFrame",
            ParamType::DataFrameArray => "DataFrameArray",
        }
    }

    pub fn default_value(&self) -> String {
        match self {
            ParamType::Float => "0.0".to_string(),
            ParamType::Int => "0".to_string(),
            ParamType::String => "".to_string(),
            ParamType::Bool => "false".to_string(),
            ParamType::DataFrame => "".to_string(),
            ParamType::DataFrameArray => "".to_string(),
        }
    }
}

/// 端口/参数方向
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PortDirection {
    Input,
    Output,
    Param,
}

impl PortDirection {
    pub fn to_str(&self) -> &str {
        match self {
            PortDirection::Input => "输入",
            PortDirection::Output => "输出",
            PortDirection::Param => "参数",
        }
    }
}

/// 算子端口/参数定义（统一管理输入端口、输出端口和参数）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorPortParamDef {
    pub name: String,
    pub direction: PortDirection,
    pub param_type: ParamType,
    pub default_value: String,
}

impl Default for OperatorPortParamDef {
    fn default() -> Self {
        Self {
            name: "input".to_string(),
            direction: PortDirection::Input,
            param_type: ParamType::DataFrame,           
            default_value: "".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomOperatorDef {
    pub name: String,
    pub description: String,
    pub code: String,
    pub color: [u8; 3],
    #[serde(default)]
    pub port_params: Vec<OperatorPortParamDef>,
    /// 摘要：一句话说明算子用途，展示在算子列表卡片上。
    #[serde(default)]
    pub summary: String,
    /// 详细描述（Markdown 格式）：包含用法、参数说明、示例等，
    /// 在算子运行参数面板中以阅读模式渲染，帮助用户快速上手。
    #[serde(default)]
    pub description_md: String,
    /// 是否为流式算子（导出 5 个流式 C ABI 符号，逐 chunk 产出数据，如 LLM 对接）。
    /// `operator.json` 中缺省时为 `false`（批量算子）。
    #[serde(default)]
    pub stream: bool,
    /// 是否支持**动态新增输入端口**（如合并算子可按需扩展输入口数）。
    /// `operator.json` 中缺省时为 `false`（端口数固定）。
    /// 为 `true` 时：
    ///   - 节点右键菜单显示「新增输入端口」项
    ///   - 已连接输入端口的右键菜单显示「删除该输入端口」（端口数>=2时才允许删除到剩1）
    #[serde(default)]
    pub dynamic_input_ports: bool,
}

impl Default for CustomOperatorDef {
    fn default() -> Self {
        Self {
            name: "自定义算子".to_string(),
            description: "用户自定义的 Rust 代码算子".to_string(),
            code: r#"// 自定义算子代码 (Rust 版本 - C ABI)
// =============================================================================
// 宿主程序会把本文件编译为 cdylib，运行时通过 C ABI 调用 execute_operator。
//
// 函数签名 (C ABI):
//   - inputs: 输入 CPortData 数组指针
//   - input_count: 输入元素数量
//   - outputs: 输出 CPortData 数组指针（调用方预分配）
//   - output_cap: 输出数组容量
//   - params_json: 参数 JSON 字符串（C 字符串指针）
//
// CPortData 类型标签 (type_tag):
//   - 0 (TYPE_FLOAT): 浮点数 (f64)
//   - 1 (TYPE_INT): 整数 (i64)
//   - 2 (TYPE_STRING): 字符串 (C 字符串)
//   - 3 (TYPE_BOOL): 布尔值
//   - 4 (TYPE_DATAFRAME): DataFrame 句柄
//
// Rust PortData 便捷方法 (在 Rust 代码内部使用):
//   - PortData::Float(f64) / PortData::Int(i64) / PortData::Bool(bool)
//   - PortData::String(String) / PortData::DataFrame(DataFrame)
//   - df.column("name") -> 获取指定列
//   - col.get_f64(i) -> 获取第 i 个 f64 值 (Option<f64>)
//   - col.to_f64_vec() -> 获取所有 f64 值 (Vec<Option<f64>>)
//   - DataFrame::from_f64_vec("name", vec![1.0, 2.0, 3.0]) -> 创建单列表
//
// 参数使用:
//   - 在算子编辑器中定义的参数会自动注入为常量，命名格式为 PARAM_参数名（全大写）
//   - 浮点参数: const PARAM_PERIOD: f64 = 5.0;
//   - 整数参数: const PARAM_COUNT: i64 = 10;
//   - 字符串参数: const PARAM_NAME: &str = "value";
//   - 布尔参数: const PARAM_ENABLED: bool = true;
// =============================================================================

use operator_runtime::{PortData, DataFrame};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_from_c, portdata_to_c,
    TYPE_NULL,
};
use std::ffi::CStr;

/// 业务逻辑: N 日动量因子 (收益率)
///   factor[i] = (price[i] - price[i - N]) / price[i - N]
fn compute_momentum(df: &DataFrame) -> DataFrame {
    let period = PARAM_PERIOD as usize;
    let col = df.column("value").unwrap();
    let values: Vec<f64> = col.to_f64_vec().into_iter().map(|v| v.unwrap_or(0.0)).collect();
    let mut momentum = Vec::with_capacity(values.len());
    for i in 0..values.len() {
        if i >= period && values[i - period] != 0.0 {
            momentum.push((values[i] - values[i - period]) / values[i - period]);
        } else {
            momentum.push(0.0);
        }
    }
    DataFrame::from_f64_vec("momentum", momentum)
}

#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const std::os::raw::c_char,
) -> i32 {
    // 解析参数 JSON (如有需要)
    let _params_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };

    // 将 C ABI 输入转换为 Rust PortData
    let rust_inputs: Vec<PortData> = if inputs.is_null() || input_count == 0 {
        Vec::new()
    } else {
        unsafe {
            (0..input_count)
                .map(|i| portdata_from_c(inputs.add(i) as *mut CPortData))
                .collect()
        }
    };

    // 处理逻辑
    let result = if rust_inputs.is_empty() {
        // 无输入时返回默认数据
        DataFrame::from_f64_vec("value", vec![1.0, 2.0, 3.0, 4.0, 5.0])
    } else {
        // 使用第一个输入计算动量因子
        let input_df = match &rust_inputs[0] {
            PortData::DataFrame(df) => df,
            _ => {
                // 如果输入不是 DataFrame，返回默认数据
                DataFrame::from_f64_vec("value", vec![1.0, 2.0, 3.0, 4.0, 5.0])
            }
        };
        compute_momentum(input_df)
    };

    // 将结果转换为 C ABI 并写入 outputs
    if !outputs.is_null() && output_cap > 0 {
        let port_data = PortData::DataFrame(result);
        let c_pd = portdata_to_c(&port_data);
        unsafe {
            *outputs = c_pd;
            // 在后续槽位置 TYPE_NULL 表示输出结束
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
                    value: CPortValue { str_ptr: std::ptr::null_mut() },
                };
            }
        }
    }

    0
}"#.to_string(),
            color: [142, 68, 173],
            port_params: vec![
                OperatorPortParamDef {
                    name: "input".to_string(),
                    direction: PortDirection::Input,
                    param_type: ParamType::DataFrame,
                    default_value: "".to_string(),
                },
                OperatorPortParamDef {
                    name: "output".to_string(),
                    direction: PortDirection::Output,
                    param_type: ParamType::DataFrame,
                    default_value: "".to_string(),
                },
                OperatorPortParamDef {
                    name: "period".to_string(),
                    direction: PortDirection::Param,
                    param_type: ParamType::Float,
                    default_value: "5.0".to_string(),
                },
            ],
            summary: String::new(),
            description_md: String::new(),
            stream: false,
            dynamic_input_ports: false,
        }
    }
}

impl Node {
    /// 对 `dynamic_input_ports=true` 的节点，新增一个输入端口（下标紧跟现有最大输入端口）。
    /// 输入端口命名为 `input_N`，类型默认 DataFrameArray（与输入默认类型一致）。
    /// 返回新建端口的下标（作为输入端口集合中的 index）。
    ///
    /// # 错误
    /// 若算子的 `dynamic_input_ports` 为 false，返回 Err。
    pub fn add_input_port(&mut self) -> Result<usize, String> {
        let custom = match &mut self.operator_type {
            OperatorType::Custom(c) => c,
        };
        if !custom.dynamic_input_ports {
            return Err(format!("算子「{}」不支持动态输入端口", custom.name));
        }
        // 找当前最大输入端口 index（用于命名 input_N）
        let mut max_input_idx = 0usize;
        let mut input_count = 0usize;
        for pp in &custom.port_params {
            if pp.direction == PortDirection::Input {
                input_count += 1;
                // 从名字 input_X 里解析出 X；若解析失败则用 input_count-1 兜底
                if let Some(suffix) = pp.name.strip_prefix("input_") {
                    if let Ok(n) = suffix.parse::<usize>() {
                        max_input_idx = max_input_idx.max(n);
                    }
                } else {
                    max_input_idx = max_input_idx.max(input_count - 1);
                }
            }
        }
        // 新端口命名：若当前最后一个是 input_0, input_1 -> 下一个是 input_2
        let new_idx = if input_count == 0 { 0 } else { max_input_idx + 1 };
        let new_def = OperatorPortParamDef {
            name: format!("input_{}", new_idx),
            direction: PortDirection::Input,
            // 默认用 DataFrameArray：合并场景下这是主流；若前几个输入端口
            // 类型都是 DataFrame，则跟着用 DataFrame（保持一致）
            param_type: custom
                .port_params
                .iter()
                .find(|pp| pp.direction == PortDirection::Input)
                .map(|pp| pp.param_type.clone())
                .unwrap_or(ParamType::DataFrameArray),
            default_value: String::new(),
        };
        custom.port_params.push(new_def);
        Ok(input_count)
    }

    /// 对 `dynamic_input_ports=true` 的节点，删除指定**输入端口索引**（按输入顺序的
    /// index，不是 port_params 的整体下标）。
    ///
    /// # 规则
    /// - `dynamic_input_ports` 必须为 true
    /// - 删除后至少保留 1 个输入端口（否则该算子将无法接收任何输入）
    /// - 删除时同步把目标端口上的连线一起移除（调用方再对 DAG 调用 remove_edges_targetting_port）
    ///   本方法只改 OperatorType 的端口声明。
    ///
    /// 返回被删除端口在 `port_params` 中的名字，便于调用方同步删除连向该端口的连线。
    pub fn remove_input_port(&mut self, input_port_index: usize) -> Result<String, String> {
        let custom = match &mut self.operator_type {
            OperatorType::Custom(c) => c,
        };
        if !custom.dynamic_input_ports {
            return Err(format!("算子「{}」不支持动态输入端口", custom.name));
        }
        let input_count = custom.port_params.iter()
            .filter(|pp| pp.direction == PortDirection::Input)
            .count();
        if input_count <= 1 {
            return Err("删除后至少需要保留 1 个输入端口".to_string());
        }
        if input_port_index >= input_count {
            return Err(format!(
                "输入端口索引越界: index={}, input_count={}",
                input_port_index, input_count
            ));
        }

        // 找到第 input_port_index 个方向为 Input 的端口在 port_params 中的位置
        let pos_in_vec = custom
            .port_params
            .iter()
            .enumerate()
            .filter(|(_, pp)| pp.direction == PortDirection::Input)
            .nth(input_port_index)
            .map(|(i, _)| i)
            .ok_or_else(|| "输入端口不存在".to_string())?;

        let removed_name = custom.port_params[pos_in_vec].name.clone();
        custom.port_params.remove(pos_in_vec);
        Ok(removed_name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OperatorType {
    Custom(CustomOperatorDef),
}

impl OperatorType {
    pub fn name(&self) -> &str {
        match self {
            OperatorType::Custom(def) => &def.name,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            OperatorType::Custom(def) => &def.description,
        }
    }

    /// 摘要：展示在算子列表卡片上的一句话说明。
    /// 若 `summary` 为空则回退到 `description`，保证旧算子也有可展示文本。
    pub fn summary(&self) -> &str {
        match self {
            OperatorType::Custom(def) => {
                if def.summary.is_empty() {
                    &def.description
                } else {
                    &def.summary
                }
            }
        }
    }

    /// 详细描述（Markdown 格式），在算子运行参数面板中以阅读模式渲染。
    pub fn description_md(&self) -> &str {
        match self {
            OperatorType::Custom(def) => &def.description_md,
        }
    }

    /// 获取输入端口数量（从 port_params 中统计 Direction::Input 的项）
    pub fn input_count(&self) -> usize {
        match self {
            OperatorType::Custom(def) => {
                def.port_params.iter()
                    .filter(|pp| pp.direction == PortDirection::Input)
                    .count()
            }
        }
    }

    /// 获取输出端口数量（从 port_params 中统计 Direction::Output 的项）
    pub fn output_count(&self) -> usize {
        match self {
            OperatorType::Custom(def) => {
                def.port_params.iter()
                    .filter(|pp| pp.direction == PortDirection::Output)
                    .count()
            }
        }
    }

    pub fn color(&self) -> egui::Color32 {
        match self {
            OperatorType::Custom(def) => egui::Color32::from_rgb(def.color[0], def.color[1], def.color[2]),
        }
    }

    pub fn is_custom(&self) -> bool {
        true
    }

    pub fn as_custom_mut(&mut self) -> &mut CustomOperatorDef {
        match self {
            OperatorType::Custom(def) => def,
        }
    }

    /// 不可变借用算子定义。供预览窗口等只读场景读取节点参数（如折线算子的
    /// `date_col`/`close_col`），避免为读取参数而升级到 `&mut` 借用。
    pub fn as_custom(&self) -> &CustomOperatorDef {
        match self {
            OperatorType::Custom(def) => def,
        }
    }

    /// 算子是否支持**动态新增输入端口**。合并类算子返回 true，其余算子默认 false。
    pub fn dynamic_input_ports(&self) -> bool {
        match self {
            OperatorType::Custom(def) => def.dynamic_input_ports,
        }
    }

    /// 获取所有输入端口定义
    pub fn input_defs(&self) -> Vec<&OperatorPortParamDef> {
        match self {
            OperatorType::Custom(def) => {
                def.port_params.iter()
                    .filter(|pp| pp.direction == PortDirection::Input)
                    .collect()
            }
        }
    }

    /// 获取所有输出端口定义
    pub fn output_defs(&self) -> Vec<&OperatorPortParamDef> {
        match self {
            OperatorType::Custom(def) => {
                def.port_params.iter()
                    .filter(|pp| pp.direction == PortDirection::Output)
                    .collect()
            }
        }
    }

    /// 获取指定输出端口的类型
    pub fn get_output_port_type(&self, port_index: usize) -> Option<&ParamType> {
        match self {
            OperatorType::Custom(def) => {
                def.port_params.iter()
                    .filter(|pp| pp.direction == PortDirection::Output)
                    .nth(port_index)
                    .map(|pp| &pp.param_type)
            }
        }
    }

    /// 获取指定输入端口的类型
    pub fn get_input_port_type(&self, port_index: usize) -> Option<&ParamType> {
        match self {
            OperatorType::Custom(def) => {
                def.port_params.iter()
                    .filter(|pp| pp.direction == PortDirection::Input)
                    .nth(port_index)
                    .map(|pp| &pp.param_type)
            }
        }
    }

    /// 获取所有参数定义
    pub fn param_defs(&self) -> Vec<&OperatorPortParamDef> {
        match self {
            OperatorType::Custom(def) => {
                def.port_params.iter()
                    .filter(|pp| pp.direction == PortDirection::Param)
                    .collect()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub operator_type: OperatorType,
    pub position: egui::Vec2,
}

impl Node {
    pub fn new(operator_type: OperatorType, position: egui::Vec2) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            operator_type,
            position,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub source_node_id: String,
    pub source_port: usize,
    pub target_node_id: String,
    pub target_port: usize,
}

impl Edge {
    pub fn new(
        source_node_id: String,
        source_port: usize,
        target_node_id: String,
        target_port: usize,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            source_node_id,
            source_port,
            target_node_id,
            target_port,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Default for DagGraph {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

impl DagGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, node: Node) -> &Node {
        self.nodes.push(node);
        self.nodes.last().unwrap()
    }

    pub fn remove_node(&mut self, node_id: &str) {
        self.nodes.retain(|n| n.id != node_id);
        self.edges.retain(|e| e.source_node_id != node_id && e.target_node_id != node_id);
    }

    pub fn add_edge(&mut self, edge: Edge) -> Result<(), String> {
        let source_node = self.get_node(&edge.source_node_id)
            .ok_or_else(|| "源节点不存在".to_string())?;
        let target_node = self.get_node(&edge.target_node_id)
            .ok_or_else(|| "目标节点不存在".to_string())?;

        if edge.source_port >= source_node.operator_type.output_count() {
            return Err(format!(
                "源节点 \"{}\" 的输出端口 {} 不存在 (仅有 {} 个输出端口)",
                source_node.operator_type.name(),
                edge.source_port,
                source_node.operator_type.output_count()
            ));
        }
        if edge.target_port >= target_node.operator_type.input_count() {
            return Err(format!(
                "目标节点 \"{}\" 的输入端口 {} 不存在 (仅有 {} 个输入端口)",
                target_node.operator_type.name(),
                edge.target_port,
                target_node.operator_type.input_count()
            ));
        }

        // 类型检查：源输出端口类型必须与目标输入端口类型匹配
        let source_port_type = source_node.operator_type.get_output_port_type(edge.source_port);
        let target_port_type = target_node.operator_type.get_input_port_type(edge.target_port);
        if let (Some(source_type), Some(target_type)) = (source_port_type, target_port_type) {
            if source_type != target_type {
                return Err(format!(
                    "端口类型不匹配: 源节点 \"{}\" 的输出端口 {} 类型为 {}，目标节点 \"{}\" 的输入端口 {} 类型为 {}",
                    source_node.operator_type.name(),
                    edge.source_port,
                    source_type.to_str(),
                    target_node.operator_type.name(),
                    edge.target_port,
                    target_type.to_str()
                ));
            }
        }

        // 防止完全相同的重复边 (优先于端口占用检查, 给出更准确的提示)
        if self.edges.iter().any(|e|
            e.source_node_id == edge.source_node_id
                && e.source_port == edge.source_port
                && e.target_node_id == edge.target_node_id
                && e.target_port == edge.target_port
        ) {
            return Err("该连线已存在".to_string());
        }

        // 仅限制「同一个输入端口」不可被多条边占用: 多条边映射到同一 target_port 会让
        // 服务端输入装配出现重复入边而报错. 「一个输出端口 → 多个目标节点的输入端口」
        // 的扇出是允许的——服务端批量/流式路径都按 target_port 下标物化上游输出并
        // clone 给各目标节点 (见 operator_runtime_server 输入装配 input_slots 逻辑).
        if self.edges.iter().any(|e|
            e.target_node_id == edge.target_node_id && e.target_port == edge.target_port
        ) {
            return Err(format!(
                "目标节点 \"{}\" 的输入端口 {} 已被占用, 一个输入端口只能连接一条线",
                target_node.operator_type.name(),
                edge.target_port
            ));
        }

        if self.would_create_cycle(&edge) {
            return Err("会形成循环依赖".to_string());
        }

        self.edges.push(edge);
        Ok(())
    }

    pub fn remove_edge(&mut self, edge_id: &str) {
        self.edges.retain(|e| e.id != edge_id);
    }

    pub fn get_node(&self, node_id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == node_id)
    }

    pub fn get_node_mut(&mut self, node_id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == node_id)
    }

    pub fn get_all_nodes(&self) -> Vec<&Node> {
        self.nodes.iter().collect()
    }

    pub fn has_node(&self, node_id: &str) -> bool {
        self.nodes.iter().any(|n| n.id == node_id)
    }

    /// 移除所有连向 `node_id` 的 **第 `target_port_index` 个输入端口** 的边。
    /// 在删除动态输入端口时调用，避免保留指向已不存在端口的"悬空连线"。
    /// 返回被移除的边数量。
    pub fn remove_edges_on_input_port(&mut self, node_id: &str, target_port_index: usize) -> usize {
        let before = self.edges.len();
        self.edges.retain(|e| !(e.target_node_id == node_id && e.target_port == target_port_index));
        before - self.edges.len()
    }

    /// 当节点的输入端口定义顺序发生变化（例如删除中间某个输入端口导致后续端口
    /// 下标整体前移 1）时，需要把所有以该节点为目标的 `target_port` 进行重排：
    /// - 删除前下标 < `removed_index`：不变
    /// - 删除前下标 == `removed_index`：上面 `remove_edges_on_input_port` 已移除
    /// - 删除前下标 > `removed_index`：`target_port -= 1`
    pub fn shift_target_ports_after_remove(
        &mut self,
        node_id: &str,
        removed_index: usize,
    ) {
        for edge in self.edges.iter_mut() {
            if edge.target_node_id == node_id && edge.target_port > removed_index {
                edge.target_port -= 1;
            }
        }
    }

    fn would_create_cycle(&self, edge: &Edge) -> bool {
        let mut visited = HashSet::new();
        let mut stack = vec![edge.target_node_id.clone()];

        while let Some(node_id) = stack.pop() {
            if node_id == edge.source_node_id {
                return true;
            }

            if visited.contains(&node_id) {
                continue;
            }

            visited.insert(node_id.clone());

            for e in &self.edges {
                if e.source_node_id == node_id {
                    stack.push(e.target_node_id.clone());
                }
            }
        }

        false
    }

    pub fn get_edges_from_node(&self, node_id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.source_node_id == node_id).collect()
    }

    pub fn get_edges_to_node(&self, node_id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.target_node_id == node_id).collect()
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        for node in &self.nodes {
            let input_count = node.operator_type.input_count();
            let incoming_edges = self.get_edges_to_node(&node.id).len();

            if incoming_edges < input_count {
                errors.push(format!(
                    "节点 {} ({}) 需要 {} 个输入，当前只有 {} 个",
                    node.id,
                    node.operator_type.name(),
                    input_count,
                    incoming_edges
                ));
            }
        }

        errors
    }

    pub fn topological_sort(&self) -> Result<Vec<String>, String> {
        let mut in_degree = HashMap::new();
        for node in &self.nodes {
            in_degree.insert(node.id.clone(), 0);
        }
        for edge in &self.edges {
            *in_degree.get_mut(&edge.target_node_id).unwrap() += 1;
        }

        let mut queue = Vec::new();
        for (node_id, degree) in &in_degree {
            if *degree == 0 {
                queue.push(node_id.clone());
            }
        }

        let mut result = Vec::new();
        while let Some(node_id) = queue.pop() {
            result.push(node_id.clone());
            for edge in self.get_edges_from_node(&node_id) {
                let target_id = &edge.target_node_id;
                *in_degree.get_mut(target_id).unwrap() -= 1;
                if in_degree[target_id] == 0 {
                    queue.push(target_id.clone());
                }
            }
        }

        if result.len() != self.nodes.len() {
            return Err("图中存在循环，无法进行拓扑排序".to_string());
        }

        Ok(result)
    }

    pub fn get_ancestors(&self, node_id: &str) -> Result<Vec<String>, String> {
        let mut visited = HashSet::new();
        let mut ancestors = Vec::new();
        let mut stack = vec![node_id.to_string()];

        while let Some(current_id) = stack.pop() {
            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());
            ancestors.push(current_id.clone());

            for edge in self.get_edges_to_node(&current_id) {
                stack.push(edge.source_node_id.clone());
            }
        }

        let topo = self.topological_sort()?;
        let result: Vec<String> = topo
            .into_iter()
            .filter(|id| ancestors.contains(id))
            .collect();

        Ok(result)
    }
}



/// 缓存已加载的算子类型（使用 RwLock 支持线程安全的刷新）
static OPERATOR_TYPES_CACHE: RwLock<Option<Vec<OperatorType>>> = RwLock::new(None);

/// 缓存从服务器获取的层级化算子分类
static OPERATOR_CATEGORIES_CACHE: RwLock<Option<Vec<OperatorCategory>>> = RwLock::new(None);

/// 上次从服务器刷新算子分类的时间戳，仅供诊断缓存新旧程度使用。
static LAST_CATEGORIES_REFRESH: RwLock<Option<Instant>> = RwLock::new(None);

/// 后台线程刷新算子分类缓存的间隔。
///
/// 渲染线程的 [`get_operator_categories`] 只读缓存，真正向 runtime 服务拉取由
/// 后台线程按本间隔周期执行（复用全局持久连接，不产生新端口），避免在渲染线程
/// 上做阻塞 TCP 调用、也避免每帧新建短连接造成「端口不断变化 + early eof」刷屏。
const OPERATOR_CATEGORIES_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// 后台刷新线程启动标记（仅启动一次）
static CATEGORIES_REFRESH_STARTED: Once = Once::new();

/// 将 proto 端口参数转换为 app 端格式
fn convert_port_param_def(pp: &ProtoPortParamDef) -> OperatorPortParamDef {
    let direction = match pp.direction.as_str() {
        "Output" => PortDirection::Output,
        "Param" => PortDirection::Param,
        _ => PortDirection::Input,
    };
    let param_type = match pp.param_type.as_str() {
        "Float" => ParamType::Float,
        "Int" => ParamType::Int,
        "Bool" => ParamType::Bool,
        "String" => ParamType::String,
        "DataFrame" => ParamType::DataFrame,
        "DataFrameArray" => ParamType::DataFrameArray,
        _ => ParamType::DataFrame,
    };
    OperatorPortParamDef {
        name: pp.name.clone(),
        direction,
        param_type,
        default_value: pp.default_value.clone(),
    }
}

/// 将 OperatorInfo 转换为 OperatorType::Custom
pub fn operator_info_to_type(info: &ProtoOperatorInfo) -> OperatorType {
    let port_params: Vec<OperatorPortParamDef> = info
        .port_params
        .iter()
        .map(convert_port_param_def)
        .collect();

    OperatorType::Custom(CustomOperatorDef {
        name: info.name.clone(),
        description: info.description.clone(),
        code: String::new(),
        color: info.color,
        port_params,
        summary: info.summary.clone(),
        description_md: info.description_md.clone(),
        stream: info.stream,
        // 由 operator.json 的 dynamic_input_ports 字段经服务端 OperatorInfo 透传，
        // 不再按算子名兜底判断，避免依赖命名约定造成误判。
        dynamic_input_ports: info.dynamic_input_ports,
    })
}

/// 从服务器加载层级化算子列表（同步阻塞，应在后台线程调用）。
///
/// 复用 [`crate::operator_executor::with_runtime_client`] 的全局持久连接，
/// 不再各自 `RuntimeClient::new` 产生端口不断变化的短连接。
pub fn load_operators_from_server() -> Option<Vec<OperatorCategory>> {
    crate::operator_executor::with_runtime_client(|client| client.list_operators())
        .map_err(|e| eprintln!("从服务器获取算子列表失败: {}", e))
        .ok()
}

/// 单次拉取并更新算子分类缓存（供后台刷新线程与 [`refresh_operator_categories`] 复用）。
fn refresh_categories_once() {
    if let Some(categories) = load_operators_from_server() {
        if !categories.is_empty() {
            if let Ok(mut cache) = OPERATOR_CATEGORIES_CACHE.write() {
                *cache = Some(categories);
            }
            if let Ok(mut ts) = LAST_CATEGORIES_REFRESH.write() {
                *ts = Some(Instant::now());
            }
        }
    }
}

/// 启动后台线程周期性刷新算子分类缓存（仅启动一次）。
///
/// 渲染线程的 [`get_operator_categories`] 只读缓存，刷新交给本后台线程，避免在
/// 渲染线程上做阻塞 TCP 调用。刷新通过 [`load_operators_from_server`] 复用全局
/// 持久连接，不会产生端口不断变化的短连接。启动时立即刷新一次以缩短首屏空窗。
pub fn start_operator_categories_refresh() {
    CATEGORIES_REFRESH_STARTED.call_once(|| {
        std::thread::Builder::new()
            .name("operator-categories-refresh".into())
            .spawn(|| {
                refresh_categories_once(); // 启动后立即刷新一次
                loop {
                    std::thread::sleep(OPERATOR_CATEGORIES_REFRESH_INTERVAL);
                    refresh_categories_once();
                }
            })
            .expect("启动算子分类刷新线程失败");
    });
}

/// 获取层级化算子分类（只读缓存，不在渲染线程上发起 TCP）。
///
/// 渲染线程每帧都会调用本函数，因此仅读取 [`OPERATOR_CATEGORIES_CACHE`]；真正的
/// 服务器拉取由 [`start_operator_categories_refresh`] 启动的后台线程周期完成。
/// 首次调用会启动该后台线程；缓存尚未就绪时返回空向量（界面先显示默认模板算子，
/// 后台线程就绪后自动填充）。
pub fn get_operator_categories() -> Vec<OperatorCategory> {
    start_operator_categories_refresh();
    if let Ok(cache) = OPERATOR_CATEGORIES_CACHE.read() {
        if let Some(cats) = &*cache {
            return cats.clone();
        }
    }
    Vec::new()
}

/// 获取所有可用算子类型（从服务器分类树递归收集）
pub fn get_all_operator_types() -> Vec<OperatorType> {
    let mut result = Vec::new();

    // 从服务器获取的算子分类树（递归收集所有层级的算子）
    let categories = get_operator_categories();
    collect_operators_from_categories(&categories, &mut result);

    result
}

/// 递归收集分类树中所有层级的算子（去重）
fn collect_operators_from_categories(
    categories: &[OperatorCategory],
    out: &mut Vec<OperatorType>,
) {
    for cat in categories {
        for op_info in &cat.operators {
            let op_type = operator_info_to_type(op_info);
            if !is_operator_in_list(out, op_type.name()) {
                out.push(op_type);
            }
        }
        collect_operators_from_categories(&cat.subcategories, out);
    }
}

/// 根据算子名称查找其 DLL 路径（从服务器缓存的分类中查找）
pub fn find_operator_dll_path(name: &str) -> Option<String> {
    if let Ok(cache) = OPERATOR_CATEGORIES_CACHE.read() {
        if let Some(categories) = &*cache {
            return find_dll_in_categories(categories, name);
        }
    }
    None
}

/// 递归在分类树中查找算子 DLL 路径（支持任意深度）
fn find_dll_in_categories(categories: &[OperatorCategory], name: &str) -> Option<String> {
    for cat in categories {
        for op_info in &cat.operators {
            if op_info.name == name {
                return Some(op_info.dll_path.clone());
            }
        }
        if let Some(p) = find_dll_in_categories(&cat.subcategories, name) {
            return Some(p);
        }
    }
    None
}

/// 从服务器算子缓存中按名称查找算子的摘要与详细描述（`summary`, `description_md`）。
///
/// 用于补全从旧建模文件加载的节点：旧文件保存时这两个字段尚不存在，加载后为空。
/// 此处按算子名从最新的服务器缓存中回填，使「算子运行参数」面板能正常展示文档，
/// 无需用户重新拖拽节点。
pub fn lookup_operator_doc(name: &str) -> Option<(String, String)> {
    if let Ok(cache) = OPERATOR_CATEGORIES_CACHE.read() {
        if let Some(categories) = &*cache {
            return lookup_doc_in_categories(categories, name);
        }
    }
    None
}

fn lookup_doc_in_categories(
    categories: &[OperatorCategory],
    name: &str,
) -> Option<(String, String)> {
    for cat in categories {
        for op_info in &cat.operators {
            if op_info.name == name {
                return Some((op_info.summary.clone(), op_info.description_md.clone()));
            }
        }
        if let Some(found) = lookup_doc_in_categories(&cat.subcategories, name) {
            return Some(found);
        }
    }
    None
}

fn is_operator_in_list(ops: &[OperatorType], name: &str) -> bool {
    ops.iter().any(|op| op.name() == name)
}

/// 加载已启用的算子：扫描 operator/ 目录下的子目录，读取 JSON 文件
pub fn load_enabled_operators() -> Vec<OperatorType> {
    let operator_dir = crate::config::get_operator_directory();
    let mut operators = Vec::new();
    
    if let Ok(entries) = fs::read_dir(&operator_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let json_path = path.join("operator.json");
                if json_path.exists() {
                    if let Ok(content) = fs::read_to_string(&json_path) {
                        if let Ok(def) = serde_json::from_str::<CustomOperatorDef>(&content) {
                            operators.push(OperatorType::Custom(def));
                        }
                    }
                }
            }
        }
    }
    
    operators
}

/// 刷新算子类型缓存（在启用新算子后调用）
pub fn refresh_operator_types_cache() {
    let ops = load_enabled_operators();
    if let Ok(mut cache) = OPERATOR_TYPES_CACHE.write() {
        *cache = Some(ops);
    }
}

/// 强制立即刷新服务器算子分类缓存。
///
/// 在启用/编译新算子后调用。由于 [`get_operator_categories`] 只读缓存、刷新由
/// 后台线程周期执行，新算子最多要等一个刷新间隔才出现；本函数另起一个一次性
/// 后台线程立即拉取，让新算子马上反映到界面，同时不阻塞调用方（通常是 UI 线程）。
/// 拉取复用全局持久连接，且仅在成功时覆盖缓存，服务器暂时不可达时保留旧列表。
pub fn refresh_operator_categories() {
    std::thread::Builder::new()
        .name("operator-categories-refresh-now".into())
        .spawn(refresh_categories_once)
        .ok();
}

/// 节点执行状态（使用协议中定义的 OperatorExecutionStatus）
/// 为了避免重新导出私有类型，直接使用 protocol 模块路径访问
pub type ExecutionStatus = OperatorExecutionStatus;

/// 单个节点的 I/O 结果封装
#[derive(Debug, Clone)]
pub struct NodeIOResult {
    /// 输入数据（用于调试和追溯）
    pub inputs: Vec<PortData>,
    /// 输出数据（每个输出端口对应一个 PortData）
    pub outputs: Vec<PortData>,
    /// 执行状态（使用协议中定义的统一状态）
    pub status: OperatorExecutionStatus,
    /// 执行时间戳（用于缓存策略）
    pub timestamp: u64,
    /// 完整的执行结果（包含日志、耗时、错误信息等）
    pub execution_result: OperatorExecutionResult,
}

impl Default for NodeIOResult {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
            status: OperatorExecutionStatus::NotExecuted,
            timestamp: 0,
            execution_result: OperatorExecutionResult::not_executed(),
        }
    }
}

/// 节点 I/O 全局注册表
/// 
/// 集中管理所有节点的输入、输出、执行状态和缓存失效。
/// 当用户修改图结构（增删边、修改参数、删除结点）时，
/// 自动触发受影响节点及其所有下游节点的 invalidate。
#[derive(Debug, Clone)]
pub struct NodeIORegistry {
    /// 节点 I/O 结果映射
    results: std::collections::HashMap<String, NodeIOResult>,
    /// 脏节点集合（需要重新执行的节点）
    dirty_nodes: std::collections::HashSet<String>,
}

impl Default for NodeIORegistry {
    fn default() -> Self {
        Self {
            results: std::collections::HashMap::new(),
            dirty_nodes: std::collections::HashSet::new(),
        }
    }
}

impl NodeIORegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置节点正在执行状态
    pub fn set_executing(&mut self, node_id: &str, inputs: Vec<PortData>) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        self.results.insert(node_id.to_string(), NodeIOResult {
            inputs,
            outputs: Vec::new(),
            status: OperatorExecutionStatus::Executing,
            timestamp,
            execution_result: OperatorExecutionResult::executing(),
        });
    }

    /// 设置节点执行结果
    ///
    /// 仅负责存储执行结果与状态，预览缓存的落盘由算子执行器层负责
    /// （服务端子图执行成功后立即调用
    /// `data_preview::save_preview_from_truncated`）。
    pub fn set_result(&mut self, node_id: &str, inputs: Vec<PortData>, outputs: Vec<PortData>,
                      mut execution_result: OperatorExecutionResult) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        execution_result.status = OperatorExecutionStatus::Completed;
        self.results.insert(node_id.to_string(), NodeIOResult {
            inputs,
            outputs,
            status: OperatorExecutionStatus::Completed,
            timestamp,
            execution_result,
        });
        self.dirty_nodes.remove(node_id);
    }

    /// 设置节点执行失败
    pub fn set_failed(&mut self, node_id: &str, inputs: Vec<PortData>, 
                      mut execution_result: OperatorExecutionResult) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        
        execution_result.status = OperatorExecutionStatus::Failed;
        self.results.insert(node_id.to_string(), NodeIOResult {
            inputs,
            outputs: Vec::new(),
            status: OperatorExecutionStatus::Failed,
            timestamp,
            execution_result,
        });
        self.dirty_nodes.remove(node_id);
    }

    /// 获取节点指定输出端口的数据
    pub fn get_output(&self, node_id: &str, port_index: usize) -> Option<&PortData> {
        self.results
            .get(node_id)
            .and_then(|result| result.outputs.get(port_index))
    }

    /// 获取节点的所有输出数据
    pub fn get_outputs(&self, node_id: &str) -> Option<&Vec<PortData>> {
        self.results.get(node_id).map(|r| &r.outputs)
    }

    /// 获取节点的执行状态
    pub fn get_status(&self, node_id: &str) -> OperatorExecutionStatus {
        self.results
            .get(node_id)
            .map(|r| r.status)
            .unwrap_or(OperatorExecutionStatus::NotExecuted)
    }

    /// 获取节点的执行结果
    pub fn get_result(&self, node_id: &str) -> Option<&NodeIOResult> {
        self.results.get(node_id)
    }

    /// 获取节点的执行日志
    pub fn get_logs(&self, node_id: &str) -> Option<&Vec<ExecutionLogEntry>> {
        self.results.get(node_id).map(|r| &r.execution_result.logs)
    }

    /// 获取节点的执行耗时（毫秒）
    pub fn get_duration_ms(&self, node_id: &str) -> Option<u64> {
        self.results.get(node_id).and_then(|r| r.execution_result.duration_ms)
    }

    /// 获取节点的所有输入数据（用于调试）
    pub fn get_inputs(&self, node_id: &str) -> Option<&Vec<PortData>> {
        self.results.get(node_id).map(|r| &r.inputs)
    }

    /// 获取节点失败时的错误信息
    pub fn get_error(&self, node_id: &str) -> Option<&str> {
        self.results.get(node_id).and_then(|r| r.execution_result.error_message.as_deref())
    }

    /// 使节点及其所有下游节点失效（级联标记）
    pub fn invalidate_downstream(&mut self, node_id: &str, graph: &DagGraph) {
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![node_id.to_string()];

        while let Some(current_id) = stack.pop() {
            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            // 标记当前节点为 Stale
            if let Some(result) = self.results.get_mut(&current_id) {
                result.status = OperatorExecutionStatus::Stale;
                result.execution_result.status = OperatorExecutionStatus::Stale;
                result.execution_result.append_log(
                    ExecutionLogEntry::warn("节点状态已过期，需要重新执行")
                );
            }
            self.dirty_nodes.insert(current_id.clone());

            // 获取当前节点的所有下游节点
            for edge in graph.get_edges_from_node(&current_id) {
                let target_id = &edge.target_node_id;
                if !visited.contains(target_id) {
                    stack.push(target_id.clone());
                }
            }
        }
    }

    /// 使单个节点失效（不级联）
    pub fn invalidate_node(&mut self, node_id: &str) {
        if let Some(result) = self.results.get_mut(node_id) {
            result.status = OperatorExecutionStatus::Stale;
            result.execution_result.status = OperatorExecutionStatus::Stale;
            result.execution_result.append_log(
                ExecutionLogEntry::warn("节点状态已过期，需要重新执行")
            );
        }
        self.dirty_nodes.insert(node_id.to_string());
    }

    /// 追加节点执行日志
    pub fn append_log(&mut self, node_id: &str, entry: ExecutionLogEntry) {
        if let Some(result) = self.results.get_mut(node_id) {
            result.execution_result.append_log(entry);
        }
    }

    /// 移除节点的所有记录
    pub fn remove_node(&mut self, node_id: &str) {
        self.results.remove(node_id);
        self.dirty_nodes.remove(node_id);
    }

    /// 检查节点是否需要重新执行
    pub fn is_dirty(&self, node_id: &str) -> bool {
        self.dirty_nodes.contains(node_id)
    }

    /// 获取所有需要重新执行的节点
    pub fn get_dirty_nodes(&self) -> Vec<String> {
        self.dirty_nodes.iter().cloned().collect()
    }

    /// 根据图结构获取节点的输入数据
    /// 从上游节点的输出中收集数据，按照输入端口顺序排列
    pub fn get_inputs_for_node(&self, node_id: &str, graph: &DagGraph) -> Result<Vec<PortData>, String> {
        let node = graph.get_node(node_id)
            .ok_or_else(|| format!("节点不存在: {}", node_id))?;

        let input_count = node.operator_type.input_count();
        if input_count == 0 {
            return Ok(Vec::new());
        }

        let incoming_edges = graph.get_edges_to_node(node_id);
        if incoming_edges.is_empty() && input_count > 0 {
            return Err(format!("节点 {} 需要 {} 个输入，但没有连接任何边", node_id, input_count));
        }

        // 按目标端口索引排序，确保输入顺序正确
        let mut sorted_edges = incoming_edges.clone();
        sorted_edges.sort_by_key(|e| e.target_port);

        let mut inputs = Vec::with_capacity(input_count);
        for edge in &sorted_edges {
            if let Some(output) = self.get_output(&edge.source_node_id, edge.source_port) {
                inputs.push(output.clone());
            } else {
                return Err(format!(
                    "节点 {} 的输出端口 {} 尚未执行或不存在",
                    edge.source_node_id, edge.source_port
                ));
            }
        }

        Ok(inputs)
    }

    /// 清空所有记录
    pub fn clear(&mut self) {
        self.results.clear();
        self.dirty_nodes.clear();
    }

    /// 获取所有已执行节点的 ID
    pub fn get_executed_nodes(&self) -> Vec<String> {
        self.results
            .iter()
            .filter(|(_, r)| r.status == OperatorExecutionStatus::Completed)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Vec2;

    fn make_graph() -> (DagGraph, String, String, String) {
        let mut g = DagGraph::new();
        // n1: 数据加载类型 (input=0, output=1)
        let n1 = g.add_node(Node::new(
            OperatorType::Custom(CustomOperatorDef {
                port_params: vec![
                    OperatorPortParamDef {
                        name: "output".to_string(),
                        direction: PortDirection::Output,
                        param_type: ParamType::DataFrame,
                        default_value: "".to_string(),
                    },
                ],
                ..Default::default()
            }),
            Vec2::ZERO,
        )).id.clone();
        // n2: 移动平均类型 (input=1, output=1)
        let n2 = g.add_node(Node::new(
            OperatorType::Custom(CustomOperatorDef {
                port_params: vec![
                    OperatorPortParamDef {
                        name: "input".to_string(),
                        direction: PortDirection::Input,
                        param_type: ParamType::DataFrame,
                        default_value: "".to_string(),
                    },
                    OperatorPortParamDef {
                        name: "output".to_string(),
                        direction: PortDirection::Output,
                        param_type: ParamType::DataFrame,
                        default_value: "".to_string(),
                    },
                ],
                ..Default::default()
            }),
            Vec2::ZERO,
        )).id.clone();
        // n3: 合并类型 (input=2, output=1)
        let n3 = g.add_node(Node::new(
            OperatorType::Custom(CustomOperatorDef {
                port_params: vec![
                    OperatorPortParamDef {
                        name: "input_0".to_string(),
                        direction: PortDirection::Input,
                        param_type: ParamType::DataFrame,
                        default_value: "".to_string(),
                    },
                    OperatorPortParamDef {
                        name: "input_1".to_string(),
                        direction: PortDirection::Input,
                        param_type: ParamType::DataFrame,
                        default_value: "".to_string(),
                    },
                    OperatorPortParamDef {
                        name: "output".to_string(),
                        direction: PortDirection::Output,
                        param_type: ParamType::DataFrame,
                        default_value: "".to_string(),
                    },
                ],
                ..Default::default()
            }),
            Vec2::ZERO,
        )).id.clone();
        (g, n1, n2, n3)
    }

    #[test]
    fn add_edge_valid_succeeds() {
        let (mut g, n1, n2, _) = make_graph();
        let edge = Edge::new(n1.clone(), 0, n2.clone(), 0);
        assert!(g.add_edge(edge).is_ok());
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn add_edge_rejects_invalid_source_port() {
        let (mut g, n1, n2, _) = make_graph();
        // n1 只有 1 个输出端口 (index 0), 1 越界
        let edge = Edge::new(n1, 1, n2, 0);
        let err = g.add_edge(edge).unwrap_err();
        assert!(err.contains("输出端口"), "actual: {}", err);
    }

    #[test]
    fn add_edge_rejects_invalid_target_port() {
        let (mut g, n1, n2, _) = make_graph();
        // n2 只有 1 个输入端口, 1 越界
        let edge = Edge::new(n1, 0, n2, 1);
        let err = g.add_edge(edge).unwrap_err();
        assert!(err.contains("输入端口"), "actual: {}", err);
    }

    #[test]
    fn add_edge_rejects_duplicate_target_port() {
        let (mut g, n1, n2, n3) = make_graph();
        // 两个不同的源都连到 n2 的同一个输入端口 0
        let e1 = Edge::new(n1.clone(), 0, n2.clone(), 0);
        assert!(g.add_edge(e1).is_ok());
        let e2 = Edge::new(n3.clone(), 0, n2.clone(), 0);
        let err = g.add_edge(e2).unwrap_err();
        assert!(err.contains("已被占用"), "actual: {}", err);
    }

    #[test]
    fn add_edge_allows_multiple_inputs_on_merge() {
        // n3 有 2 个输入端口, 不同端口应都能连入
        let (mut g, n1, n2, n3) = make_graph();
        assert!(g.add_edge(Edge::new(n1.clone(), 0, n3.clone(), 0)).is_ok());
        assert!(g.add_edge(Edge::new(n2.clone(), 0, n3.clone(), 1)).is_ok());
        assert_eq!(g.edges.len(), 2);
    }

    #[test]
    fn add_edge_allows_fan_out_one_output_to_many_targets() {
        // 一个输出端口可以连到多个目标节点的输入端口 (扇出). n1:0 → n2:0 与
        // n1:0 → n3:0 应同时成功: 服务端会按 target_port 物化 n1 的输出并 clone
        // 给每个目标节点 (见 runtime 批量/流式输入装配 input_slots).
        let (mut g, n1, n2, n3) = make_graph();
        assert!(g.add_edge(Edge::new(n1.clone(), 0, n2.clone(), 0)).is_ok());
        assert!(g.add_edge(Edge::new(n1.clone(), 0, n3.clone(), 0)).is_ok());
        assert_eq!(g.edges.len(), 2);
        let fan_out = g.edges
            .iter()
            .filter(|e| e.source_node_id == n1 && e.source_port == 0)
            .count();
        assert_eq!(fan_out, 2, "同一输出端口应允许扇出到多个目标");
    }

    #[test]
    fn add_edge_rejects_duplicate_edge() {
        let (mut g, n1, n2, _) = make_graph();
        assert!(g.add_edge(Edge::new(n1.clone(), 0, n2.clone(), 0)).is_ok());
        let err = g.add_edge(Edge::new(n1.clone(), 0, n2.clone(), 0)).unwrap_err();
        assert!(err.contains("已存在"), "actual: {}", err);
    }

    #[test]
    fn add_edge_rejects_cycle() {
        // 用两个都有输入端口的节点互相连, 才能触发循环检测
        // n2 (in=1/out=1) <-> n3 (in=2/out=1)
        let (mut g, _, n2, n3) = make_graph();
        g.add_edge(Edge::new(n2.clone(), 0, n3.clone(), 0)).unwrap();
        // n3 -> n2 会形成环 (n2 已能通过 n3 的输出回到 n2 的输入)
        let err = g.add_edge(Edge::new(n3, 0, n2, 0)).unwrap_err();
        assert!(err.contains("循环"), "actual: {}", err);
    }
}
