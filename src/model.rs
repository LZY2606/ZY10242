//! 领域类型与固定 fixture。
//!
//! 事件原始强度以 0..=1000 的整数刻度保存（fixture 与重导入口径一致）；
//! 所有补偿与变换均在运行评估时按运行所记录的版本复现。

use crate::geometry::Vertex;
use serde::{Deserialize, Serialize};

pub const CHANNELS: [&str; 4] = ["FSC_A", "SSC_A", "CD3", "CD19"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelDef {
    pub name: String,
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchDef {
    pub id: String,
    pub instrument: String,
    pub acquired_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawEvent {
    pub id: String,
    pub batch_id: String,
    pub values: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransformKind {
    Linear,
    Log,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelTransform {
    pub kind: TransformKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransformDef {
    pub id: String,
    pub label: String,
    pub channels: Vec<ChannelTransform>,
}

/// 补偿矩阵系数：out[row] = sum_c coeff[out][in] * raw[in]。
/// 行列顺序由通道名唯一确定，与数据通道的物理位置无关。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompensationDef {
    pub id: String,
    pub label: String,
    pub channels: Vec<String>,
    pub rows: Vec<Vec<f64>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GatePolygon {
    pub x_channel: String,
    pub y_channel: String,
    pub vertices: Vec<Vertex>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GateVersionDef {
    pub id: String,
    pub note: String,
    pub polygon: GatePolygon,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PopulationDef {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub gate: Option<GateVersionDef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fixture {
    pub version: String,
    pub channels: Vec<ChannelDef>,
    pub batches: Vec<BatchDef>,
    pub events: Vec<RawEvent>,
    pub transforms: Vec<TransformDef>,
    pub compensations: Vec<CompensationDef>,
    pub populations: Vec<PopulationDef>,
    pub current_comp_id: String,
    pub current_transform_id: String,
}

pub fn channels() -> Vec<ChannelDef> {
    vec![
        ChannelDef {
            name: "FSC_A".into(),
            label: "FSC-A".into(),
            kind: "scatter".into(),
        },
        ChannelDef {
            name: "SSC_A".into(),
            label: "SSC-A".into(),
            kind: "scatter".into(),
        },
        ChannelDef {
            name: "CD3".into(),
            label: "CD3-FITC".into(),
            kind: "signal".into(),
        },
        ChannelDef {
            name: "CD19".into(),
            label: "CD19-PE".into(),
            kind: "signal".into(),
        },
    ]
}

/// 确定性 LCG（Numerical Recipes 常量），保证 fixture 可复放。
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// 均值 mx/my、标准差 sx/sy 的近似高斯（两次均匀之和 - 均匀，Irwin-Hall 近似）。
    pub fn gauss(&mut self, m: f64, s: f64) -> f64 {
        let z = (0..6).map(|_| self.uniform()).sum::<f64>() - 3.0;
        m + z * s
    }
    pub fn clamp(v: f64) -> f64 {
        v.round().clamp(0.0, 1000.0)
    }
}

fn transforms() -> Vec<TransformDef> {
    let linear = (0..CHANNELS.len())
        .map(|_| ChannelTransform {
            kind: TransformKind::Linear,
        })
        .collect();
    let log = CHANNELS
        .iter()
        .map(|c| ChannelTransform {
            kind: if *c == "FSC_A" || *c == "SSC_A" {
                TransformKind::Linear
            } else {
                TransformKind::Log
            },
        })
        .collect();
    vec![
        TransformDef {
            id: "T1".into(),
            label: "线性（各通道恒等）".into(),
            channels: linear,
        },
        TransformDef {
            id: "T2".into(),
            label: "信号通道 log10、散射线性".into(),
            channels: log,
        },
    ]
}

fn identity_matrix(id: &str, label: &str) -> CompensationDef {
    let n = CHANNELS.len();
    let rows = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| if i == j { 1.0 } else { 0.0 })
                .collect::<Vec<_>>()
        })
        .collect();
    CompensationDef {
        id: id.into(),
        label: label.into(),
        channels: CHANNELS.iter().map(|s| s.to_string()).collect(),
        rows,
    }
}

/// 通道排列变化但内容等价的单位补偿：验证“按通道名对应，不按位置相乘”。
fn reordered_identity() -> CompensationDef {
    let mut m = identity_matrix("C3", "通道顺序重排的单位补偿（名称对应）");
    let order = [2usize, 0, 3, 1]; // CD3, FSC_A, CD19, SSC_A
    let old_channels = m.channels.clone();
    m.channels = order.iter().map(|&i| old_channels[i].clone()).collect();
    let old_rows = m.rows.clone();
    m.rows = order
        .iter()
        .map(|&out_i| {
            order
                .iter()
                .map(|&in_i| old_rows[out_i][in_i])
                .collect::<Vec<_>>()
        })
        .collect();
    m
}

fn spillover() -> CompensationDef {
    let mut m = identity_matrix("C2", "含 CD19→CD3 串色的补偿");
    // rows[out=CD3][in=CD19]
    m.rows[2][3] = 0.06;
    m.rows[2][2] = 0.94;
    m
}

/// 缺失一个数据通道的补偿矩阵：选择它必须被拒绝，而不是按位置继续乘。
fn incomplete_matrix() -> CompensationDef {
    let mut m = identity_matrix("C4", "缺 SSC_A（禁止按位置相乘）");
    m.channels = vec!["FSC_A".into(), "CD3".into(), "CD19".into()];
    m.rows = vec![
        vec![1.0, 0.0, 0.0],
        vec![0.0, 1.0, 0.0],
        vec![0.0, 0.0, 1.0],
    ];
    m
}

fn populations() -> Vec<PopulationDef> {
    let v = |x: f64, y: f64| Vertex { x, y };
    vec![
        PopulationDef {
            id: "ALL".into(),
            name: "全部事件".into(),
            parent: None,
            gate: None,
        },
        PopulationDef {
            id: "BIG".into(),
            name: "大细胞（FSC/SSC 右半）".into(),
            parent: Some("ALL".into()),
            gate: Some(GateVersionDef {
                id: "G_BIG_1".into(),
                note: "初始门：FSC>=450 的右矩形".into(),
                polygon: GatePolygon {
                    x_channel: "FSC_A".into(),
                    y_channel: "SSC_A".into(),
                    vertices: vec![
                        v(450.0, 100.0),
                        v(900.0, 100.0),
                        v(900.0, 850.0),
                        v(450.0, 850.0),
                    ],
                },
            }),
        },
        PopulationDef {
            id: "SMALL".into(),
            name: "小细胞（FSC/SSC 左半）".into(),
            parent: Some("ALL".into()),
            gate: Some(GateVersionDef {
                id: "G_SMALL_1".into(),
                note: "初始门：FSC<450 的左矩形，与大细胞共享边".into(),
                polygon: GatePolygon {
                    x_channel: "FSC_A".into(),
                    y_channel: "SSC_A".into(),
                    vertices: vec![
                        v(50.0, 100.0),
                        v(450.0, 100.0),
                        v(450.0, 850.0),
                        v(50.0, 850.0),
                    ],
                },
            }),
        },
        PopulationDef {
            id: "T".into(),
            name: "T 细胞（CD3+，对角线下）".into(),
            parent: Some("SMALL".into()),
            gate: Some(GateVersionDef {
                id: "G_T_1".into(),
                note: "初始门：对角分割下三角 + CD3 阈值".into(),
                polygon: GatePolygon {
                    x_channel: "CD19".into(),
                    y_channel: "CD3".into(),
                    vertices: vec![
                        v(0.0, 300.0),
                        v(300.0, 300.0),
                        v(300.0, 700.0),
                        v(0.0, 700.0),
                    ],
                },
            }),
        },
        PopulationDef {
            id: "B".into(),
            name: "B 细胞（CD19+，对角线上）".into(),
            parent: Some("SMALL".into()),
            gate: Some(GateVersionDef {
                id: "G_B_1".into(),
                note: "初始门：对角分割上三角 + CD19 阈值".into(),
                polygon: GatePolygon {
                    x_channel: "CD19".into(),
                    y_channel: "CD3".into(),
                    vertices: vec![
                        v(300.0, 0.0),
                        v(700.0, 0.0),
                        v(700.0, 300.0),
                        v(300.0, 300.0),
                    ],
                },
            }),
        },
        PopulationDef {
            id: "RARE".into(),
            name: "稀有亚群（空父）".into(),
            parent: Some("BIG".into()),
            gate: Some(GateVersionDef {
                id: "G_RARE_1".into(),
                note: "初始门：极高 CD3 区，大细胞内无人命中".into(),
                polygon: GatePolygon {
                    x_channel: "CD19".into(),
                    y_channel: "CD3".into(),
                    vertices: vec![
                        v(450.0, 900.0),
                        v(650.0, 900.0),
                        v(650.0, 990.0),
                        v(450.0, 990.0),
                    ],
                },
            }),
        },
        PopulationDef {
            id: "ULTRA".into(),
            name: "极稀有孙群".into(),
            parent: Some("RARE".into()),
            gate: Some(GateVersionDef {
                id: "G_ULTRA_1".into(),
                note: "初始门：任何多边形均可，父群恒空".into(),
                polygon: GatePolygon {
                    x_channel: "CD19".into(),
                    y_channel: "CD3".into(),
                    vertices: vec![
                        v(400.0, 850.0),
                        v(700.0, 850.0),
                        v(700.0, 999.0),
                        v(400.0, 999.0),
                    ],
                },
            }),
        },
    ]
}

/// 构造固定 fixture：事件身份、批次与分布全部确定，可在清空库后重新导入复核。
pub fn fixture() -> Fixture {
    let mut events: Vec<RawEvent> = Vec::new();
    let mut rng = Rng::new(0xF10C_2026_0922);
    let add = |batch: &str, vals: [f64; 4], events: &mut Vec<RawEvent>| {
        let idx = events.len() + 1;
        events.push(RawEvent {
            id: format!("E{idx:04}"),
            batch_id: batch.into(),
            values: vals.iter().map(|v| Rng::clamp(*v)).collect(),
        });
    };

    // 180 个大细胞：FSC/SSC 偏高，荧光偏低。
    for i in 0..180 {
        let batch = if i % 2 == 0 { "B1" } else { "B2" };
        add(
            batch,
            [
                rng.gauss(650.0, 90.0),
                rng.gauss(480.0, 130.0),
                rng.gauss(120.0, 60.0),
                rng.gauss(90.0, 50.0),
            ],
            &mut events,
        );
    }
    // 180 个小细胞：T / B / 双阴性。
    for i in 0..180 {
        let batch = if i % 2 == 0 { "B1" } else { "B2" };
        let (cd3, cd19) = match i % 9 {
            0..=3 => (rng.gauss(520.0, 70.0), rng.gauss(90.0, 40.0)),
            4..=6 => (rng.gauss(80.0, 40.0), rng.gauss(520.0, 70.0)),
            _ => (rng.gauss(80.0, 40.0), rng.gauss(80.0, 40.0)),
        };
        add(
            batch,
            [rng.gauss(250.0, 80.0), rng.gauss(260.0, 90.0), cd3, cd19],
            &mut events,
        );
    }
    // 20 个碎片：FSC/SSC 很低，落在所有细胞门外。
    for i in 0..20 {
        let batch = if i % 2 == 0 { "B1" } else { "B2" };
        add(
            batch,
            [
                rng.gauss(35.0, 15.0),
                rng.gauss(55.0, 25.0),
                rng.gauss(40.0, 20.0),
                rng.gauss(40.0, 20.0),
            ],
            &mut events,
        );
    }
    // 边界事件：两个 FSC/SSC 兄弟门共享边 x=450，y∈[100,850]。
    // 统一半开规则下必须恰好被一个门认领，绝不双计。
    add("B1", [450.0, 400.0, 90.0, 90.0], &mut events); // 共享边内部
    add("B1", [450.0, 850.0, 90.0, 90.0], &mut events); // 共享顶点（上）
    add("B1", [450.0, 100.0, 90.0, 90.0], &mut events); // 共享顶点（下）
                                                        // T/B 门相切点 (CD19=300, CD3=300)：恰好一个门认领。
    add("B1", [250.0, 260.0, 300.0, 300.0], &mut events);

    Fixture {
        version: "fixture-1".into(),
        channels: channels(),
        batches: vec![
            BatchDef {
                id: "B1".into(),
                instrument: "FC-Lab-01".into(),
                acquired_at: "2026-09-20T09:00:00Z".into(),
            },
            BatchDef {
                id: "B2".into(),
                instrument: "FC-Lab-02".into(),
                acquired_at: "2026-09-21T09:00:00Z".into(),
            },
        ],
        events,
        transforms: transforms(),
        compensations: vec![
            identity_matrix("C1", "单位补偿（原始强度）"),
            spillover(),
            reordered_identity(),
            incomplete_matrix(),
        ],
        populations: populations(),
        current_comp_id: "C1".into(),
        current_transform_id: "T1".into(),
    }
}
