//! 固定 fixture：确定性事件集 + 门系 + 补偿/变换版本。
//!
//! 事件通道顺序为 FSC, SSC, CD45, CD4, CD8（原始强度 0..=1000）。
//! 除 LCG 背景事件外，附带一批坐标恰好在门边上的 QC 事件，
//! 用于验收“边界不双计”：
//!   * lymph（FSC/SSC 半开矩形）：底/左边算入，顶/右边算出；
//!   * cd4 与 cd8 在 CD4=500 处共享一条竖边，该边上的事件只属 cd8。
//! 另有一个远在数据范围外的 granulocytes 空门，及其子门 gran_subset
//! —— 父群体为空时子门百分比不可定义。

use std::collections::BTreeMap;

pub const EVENT_CHANNELS: [&str; 5] = ["FSC", "SSC", "CD45", "CD4", "CD8"];

pub fn data_channels() -> Vec<String> {
    EVENT_CHANNELS.map(str::to_string).to_vec()
}

#[derive(Debug, Clone)]
pub struct SeedEvent {
    pub id: String,
    pub batch: String,
    pub values: [f64; 5],
}

/// 数值稳定的 LCG（MMIX 参数），保证任何机器上生成同一批背景事件。
fn lcg_events(n: usize) -> Vec<SeedEvent> {
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut vals = [0.0; 5];
        for v in vals.iter_mut() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let uniform = (state >> 33) as f64 / (1u64 << 31) as f64;
            *v = (uniform * 1000.0 * 10.0).round() / 10.0;
        }
        out.push(SeedEvent {
            id: format!("bg-{i:04}"),
            batch: "sample-A".to_string(),
            values: vals,
        });
    }
    out
}

fn qc(id: &str, values: [f64; 5]) -> SeedEvent {
    SeedEvent {
        id: id.to_string(),
        batch: "qc".to_string(),
        values,
    }
}

pub fn events() -> Vec<SeedEvent> {
    // 顺序: [FSC, SSC, CD45, CD4, CD8]
    let mut crafted = vec![
        // lymph 矩形四条边的中点（底/左计入，顶/右计出）
        qc("qc-edge-bottom", [500.0, 200.0, 700.0, 400.0, 400.0]),
        qc("qc-edge-top", [500.0, 800.0, 700.0, 400.0, 400.0]),
        qc("qc-edge-right", [800.0, 400.0, 700.0, 400.0, 400.0]),
        qc("qc-edge-left", [200.0, 400.0, 700.0, 400.0, 400.0]),
        // cd4/cd8 共享竖边 CD4=500：统一只属 cd8
        qc("qc-shared-1", [300.0, 300.0, 600.0, 500.0, 500.0]),
        qc("qc-shared-2", [400.0, 400.0, 600.0, 500.0, 200.0]),
        // 各自内部
        qc("qc-cd4-in", [300.0, 500.0, 600.0, 300.0, 400.0]),
        qc("qc-cd8-in", [400.0, 500.0, 600.0, 700.0, 400.0]),
        // cd4 顶边（半开规则计出），且不在 cd8
        qc("qc-cd4-topedge", [300.0, 400.0, 600.0, 300.0, 900.0]),
        // 完全在淋巴外
        qc("qc-debris", [50.0, 50.0, 50.0, 50.0, 50.0]),
    ];
    let mut all = lcg_events(200);
    all.append(&mut crafted);
    all
}

pub fn raw_map(e: &SeedEvent) -> BTreeMap<String, f64> {
    EVENT_CHANNELS
        .iter()
        .zip(e.values.iter())
        .map(|(c, v)| ((*c).to_string(), *v))
        .collect()
}

/// 门系（version 1 顶点）。返回 (id, label, parent_id, x, y, vertices)。
pub fn gates() -> Vec<(String, String, Option<String>, String, String, Vec<(f64, f64)>)> {
    vec![
        (
            "lymph".into(),
            "淋巴细胞".into(),
            None,
            "FSC".into(),
            "SSC".into(),
            vec![
                (200.0, 200.0),
                (800.0, 200.0),
                (800.0, 800.0),
                (200.0, 800.0),
            ],
        ),
        (
            "cd4".into(),
            "CD4+".into(),
            Some("lymph".into()),
            "CD4".into(),
            "CD8".into(),
            // 左区；与 cd8 共享 (500,100)->(500,900)
            vec![
                (100.0, 100.0),
                (500.0, 100.0),
                (500.0, 900.0),
                (100.0, 900.0),
            ],
        ),
        (
            "cd8".into(),
            "CD8+".into(),
            Some("lymph".into()),
            "CD4".into(),
            "CD8".into(),
            // 右区；共享边反向遍历 (500,900)->(500,100)
            vec![
                (500.0, 100.0),
                (900.0, 100.0),
                (900.0, 900.0),
                (500.0, 900.0),
            ],
        ),
        (
            "granulocytes".into(),
            "粒细胞(空门)".into(),
            None,
            "FSC".into(),
            "SSC".into(),
            // 远在数据范围之外，计数恒为 0
            vec![
                (-5000.0, -5000.0),
                (-4000.0, -5000.0),
                (-4000.0, -4000.0),
                (-5000.0, -4000.0),
            ],
        ),
        (
            "gran_subset".into(),
            "粒细胞亚群".into(),
            Some("granulocytes".into()),
            "CD4".into(),
            "CD8".into(),
            vec![
                (100.0, 100.0),
                (900.0, 100.0),
                (900.0, 900.0),
                (100.0, 900.0),
            ],
        ),
    ]
}
