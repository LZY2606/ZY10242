//! 领域类型：通道、补偿矩阵、变换、多边形门与运行状态。
//!
//! 所有定义都是显式按通道名索引的版本化快照；任何版本一旦创建即不可变，
//! 切换到新版本会为受影响节点派生新运行，旧运行保留并标记为失效。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformKind {
    /// `value = scale * x + offset`
    Linear,
    /// `value = scale * log10(max(x - offset, eps)) + bias`
    Log10,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformParams {
    pub kind: TransformKind,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub offset: f64,
    #[serde(default = "default_bias")]
    pub bias: f64,
}

fn default_scale() -> f64 {
    1.0
}
fn default_bias() -> f64 {
    0.0
}

impl Default for TransformParams {
    fn default() -> Self {
        TransformParams {
            kind: TransformKind::Linear,
            scale: 1.0,
            offset: 0.0,
            bias: 0.0,
        }
    }
}

impl TransformParams {
    pub fn apply(&self, x: f64) -> f64 {
        match self.kind {
            TransformKind::Linear => self.scale * x + self.offset,
            TransformKind::Log10 => {
                let shifted = x - self.offset;
                // 极小正值作为底数下限，保证非正数输入也有确定结果
                let guarded = if shifted > 1e-9 { shifted } else { 1e-9 };
                self.scale * guarded.log10() + self.bias
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

/// 半开多边形（顶点顺序闭合；最后一个顶点自动与第一个相连）。
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    pub vertices: Vec<Point>,
}

/// 共享边必须逐字节（到 f64 位模式）一致时，两侧判定才严格互补；
/// 若顶点因编辑产生了数值漂移，返回错误而不是默默双计/漏计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolygonError {
    TooFewVertices,
    DegenerateEdge,
    SharedEdgeMismatch,
}

impl Polygon {
    pub fn new(vertices: Vec<Point>) -> Result<Self, PolygonError> {
        if vertices.len() < 3 {
            return Err(PolygonError::TooFewVertices);
        }
        let poly = Polygon { vertices };
        for i in 0..poly.vertices.len() {
            let a = poly.vertices[i];
            let b = poly.vertices[(i + 1) % poly.vertices.len()];
            if a.x == b.x && a.y == b.y {
                return Err(PolygonError::DegenerateEdge);
            }
        }
        Ok(poly)
    }
}

/// 运行生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// 当前活动定义下的最新运行
    Active,
    /// 被同一节点的新运行取代（定义/补偿/变换切换）
    Superseded,
    /// 祖先门被修改后，旧子门运行无法再解释 —— 明确失效，绝不显示旧数
    Invalidated,
}

impl RunStatus {
    pub fn parse(s: &str) -> RunStatus {
        match s {
            "superseded" => RunStatus::Superseded,
            "invalidated" => RunStatus::Invalidated,
            _ => RunStatus::Active,
        }
    }
}

