//! 端到端口径测试：固定 fixture、半开不双计、空父群不可定义、
//! 修改父门后旧子门运行失效、补偿通道顺序/缺通道、变换重放、导出清空重导、HTTP。

use flow_gate_bench::geometry::{normalize_ccw, Vertex};
use flow_gate_bench::model::fixture;
use flow_gate_bench::Db;
use std::collections::HashSet;

fn temp_db() -> Db {
    let db = Db::open(":memory:").unwrap();
    db.reseed(&fixture()).unwrap();
    db
}

fn count_of(db: &Db, pop: &str) -> Option<i64> {
    let s = db.state().unwrap();
    s.runs
        .iter()
        .find(|r| r.population_id == pop)
        .map(|r| r.event_count)
        .flatten()
}

fn run_id_of(db: &Db, pop: &str) -> Option<String> {
    let s = db.state().unwrap();
    s.runs
        .iter()
        .find(|r| r.population_id == pop)
        .map(|r| r.run_id.clone())
}

#[test]
fn fixture_is_fixed_and_root_covers_all_events() {
    let fx = fixture();
    let db = temp_db();
    assert_eq!(fx.events.len(), 384);
    assert_eq!(count_of(&db, "ALL"), Some(384));
    // 两个仪器批次都存在。
    let batches: HashSet<String> = fx.events.iter().map(|e| e.batch_id.clone()).collect();
    assert_eq!(batches, HashSet::from(["B1".into(), "B2".into()]));
}

#[test]
fn boundary_events_on_shared_edge_are_not_double_counted() {
    let db = temp_db();
    let big = count_of(&db, "BIG").unwrap();
    let small = count_of(&db, "SMALL").unwrap();
    // 两兄弟门在 FSC=450 共享边；并集互不重叠（再加上门外碎片）。
    let union = {
        let b = db
            .diff(
                &run_id_of(&db, "BIG").unwrap(),
                &run_id_of(&db, "SMALL").unwrap(),
            )
            .unwrap();
        b.both.len()
    };
    assert_eq!(union, 0, "共享边两侧的门不得有共同事件");

    // 边界事件 E0381（共享边内部）必须恰好落入一个门。
    let members_big = members(&db, &run_id_of(&db, "BIG").unwrap());
    let members_small = members(&db, &run_id_of(&db, "SMALL").unwrap());
    let edge_event = "E0381";
    assert!(
        members_big.contains(edge_event) ^ members_small.contains(edge_event),
        "共享边内部事件必须恰好属于一个门"
    );
    // 几何层面的直接断言（与引擎一致）。
    let fx = fixture();
    let big_gate = fx
        .populations
        .iter()
        .find(|p| p.id == "BIG")
        .unwrap()
        .gate
        .clone()
        .unwrap();
    let small_gate = fx
        .populations
        .iter()
        .find(|p| p.id == "SMALL")
        .unwrap()
        .gate
        .clone()
        .unwrap();
    let pb = normalize_ccw(&big_gate.polygon.vertices).unwrap();
    let ps = normalize_ccw(&small_gate.polygon.vertices).unwrap();
    for p in [
        Vertex { x: 450.0, y: 400.0 },
        Vertex { x: 450.0, y: 850.0 },
        Vertex { x: 450.0, y: 100.0 },
    ] {
        let hits = flow_gate_bench::geometry::contains(&pb, p) as u32
            + flow_gate_bench::geometry::contains(&ps, p) as u32;
        assert!(hits <= 1, "共享边界点 {p:?} 被双计");
    }
    assert!(big + small <= 384);
}

#[test]
fn empty_parent_percent_is_undefined_not_zero() {
    let db = temp_db();
    let s = db.state().unwrap();
    let rare = s.runs.iter().find(|r| r.population_id == "RARE").unwrap();
    let ultra = s.runs.iter().find(|r| r.population_id == "ULTRA").unwrap();
    assert_eq!(rare.event_count, Some(0));
    assert_eq!(rare.parent_count, Some(count_of(&db, "BIG").unwrap()));
    // RARE 父群非空 -> 百分比定义良好（0%）。
    assert!(rare.percent.unwrap() < 1e-9);
    // ULTRA 的父群 RARE 为空 -> 百分比不可定义（null），绝不写成 0。
    assert_eq!(ultra.parent_count, Some(0));
    assert_eq!(ultra.event_count, Some(0));
    assert!(
        ultra.percent.is_none(),
        "空父群的百分比必须为 null（不可定义）"
    );
}

#[test]
fn editing_parent_gate_invalidates_old_child_runs() {
    let db = temp_db();
    let small_before = run_id_of(&db, "SMALL").unwrap();
    let t_before = run_id_of(&db, "T").unwrap();
    let b_before = run_id_of(&db, "B").unwrap();

    // 收紧 SMALL 门（父门修改）。
    let mut new_poly = fixture()
        .populations
        .iter()
        .find(|p| p.id == "SMALL")
        .unwrap()
        .gate
        .clone()
        .unwrap()
        .polygon;
    for v in new_poly.vertices.iter_mut() {
        v.x = v.x.max(80.0).min(380.0);
    }
    db.edit_gate("SMALL", "测试收紧", new_poly).unwrap();

    let small_after = run_id_of(&db, "SMALL").unwrap();
    let t_after = run_id_of(&db, "T").unwrap();
    let b_after = run_id_of(&db, "B").unwrap();
    assert_ne!(small_before, small_after);
    assert_ne!(t_before, t_after);
    assert_ne!(b_before, b_after);

    // 旧的子门运行必须处于 invalid，而不是继续显示旧数。
    let t_runs = db.runs_of("T").unwrap();
    assert!(t_runs
        .iter()
        .any(|r| r.id == t_before && r.status == "invalid"));
    assert!(t_runs.iter().any(|r| r.id == t_after && r.status == "ok"));
    let b_runs = db.runs_of("B").unwrap();
    assert!(b_runs
        .iter()
        .any(|r| r.id == b_before && r.status == "invalid"));
    // 旧运行计数仍然保留可审计，但不挂在 current 上。
    let stale = t_runs.iter().find(|r| r.id == t_before).unwrap();
    assert!(stale.event_count.is_some());

    // 兄弟分支按事件身份的进出差异可查。
    let d = db.diff(&t_before, &t_after).unwrap();
    assert_eq!(d.a.id, t_before);
    assert_eq!(d.b.id, t_after);
    assert!(d.entered.len() + d.exited.len() + d.both.len() == d.union_count);
}

#[test]
fn compensation_channel_order_uses_names_not_positions() {
    let db = temp_db();
    let baseline = counts_snapshot(&db);
    db.replay_comp("C3").unwrap(); // 通道顺序重排的单位补偿
    let after = counts_snapshot(&db);
    assert_eq!(
        baseline, after,
        "按通道名对应的重排单位补偿必须与基线完全一致"
    );
}

#[test]
fn missing_compensation_channel_is_rejected_without_new_runs() {
    let db = temp_db();
    let before: Vec<(String, String)> = db
        .state()
        .unwrap()
        .runs
        .iter()
        .map(|r| (r.population_id.clone(), r.run_id.clone()))
        .collect();
    let err = match db.replay_comp("C4") {
        Err(e) => e,
        Ok(_) => panic!("缺通道必须报错"),
    };
    assert!(err.to_string().contains("SSC_A"));
    // 没有任何新运行产生，当前版本仍为 C1。
    let after: Vec<(String, String)> = db
        .state()
        .unwrap()
        .runs
        .iter()
        .map(|r| (r.population_id.clone(), r.run_id.clone()))
        .collect();
    assert_eq!(before, after);
    assert_eq!(db.state().unwrap().current_comp_id, "C1");
}

#[test]
fn transform_switch_replays_entire_tree_and_marks_stale() {
    let db = temp_db();
    let first_t = run_id_of(&db, "T").unwrap();
    db.replay_transform("T2").unwrap();
    let s = db.state().unwrap();
    assert_eq!(s.current_transform_id, "T2");
    for r in &s.runs {
        assert_eq!(r.transform_id, "T2");
    }
    let runs = db.runs_of("T").unwrap();
    assert!(runs
        .iter()
        .any(|r| r.id == first_t && r.status == "invalid"));
}

#[test]
fn degenerate_gate_edits_are_rejected() {
    let db = temp_db();
    let bad = flow_gate_bench::model::GatePolygon {
        x_channel: "FSC_A".into(),
        y_channel: "SSC_A".into(),
        vertices: vec![
            Vertex { x: 0.0, y: 0.0 },
            Vertex { x: 100.0, y: 100.0 },
            Vertex { x: 200.0, y: 200.0 },
        ],
    };
    let err = match db.edit_gate("BIG", "退化门", bad) {
        Err(e) => e,
        Ok(_) => panic!("退化门必须被拒绝"),
    };
    assert!(err.to_string().contains("退化"));
    // 未产生新门版本。
    let s = db.state().unwrap();
    let big = s.populations.iter().find(|p| p.id == "BIG").unwrap();
    assert_eq!(big.gate_versions.len(), 1);
}

#[test]
fn export_wipe_and_reimport_reverifies_identity_sets() {
    let db = temp_db();
    // 制造一个门版本与失效历史，再导出。
    let mut new_poly = fixture()
        .populations
        .iter()
        .find(|p| p.id == "BIG")
        .unwrap()
        .gate
        .clone()
        .unwrap()
        .polygon;
    for v in new_poly.vertices.iter_mut() {
        v.x = (v.x - 20.0).max(100.0).min(880.0);
    }
    db.edit_gate("BIG", "导出前微调", new_poly).unwrap();
    let bundle = db.export_bundle().unwrap();
    assert!(bundle.runs.iter().any(|r| r.status == "invalid"));

    // 清空库并重新导入复核。
    let report = db.import_and_verify(bundle).unwrap();
    assert!(report.all_ok);
    assert_eq!(report.populations_checked, 7);
    for c in &report.checks {
        assert_eq!(c.expected_count, c.replayed_count);
        assert!(c.only_in_export.is_empty());
        assert!(c.only_in_replay.is_empty());
    }
}

#[test]
fn reseed_restores_fixture_versions() {
    let db = temp_db();
    db.replay_transform("T2").unwrap();
    db.reseed(&fixture()).unwrap();
    let s = db.state().unwrap();
    assert_eq!(s.current_transform_id, "T1");
    assert_eq!(s.current_comp_id, "C1");
    // 历史运行被清空，只留下一次全新重放。
    for p in &s.populations {
        assert_eq!(db.runs_of(&p.id).unwrap().len(), 1);
    }
}

fn members(db: &Db, run_id: &str) -> HashSet<String> {
    let d = db.diff(run_id, run_id).unwrap();
    d.both.into_iter().collect()
}

fn counts_snapshot(db: &Db) -> Vec<(String, Option<i64>)> {
    db.state()
        .unwrap()
        .runs
        .iter()
        .map(|r| (r.population_id.clone(), r.event_count))
        .collect()
}

#[test]
fn concurrent_parent_gate_edits_always_leave_stale_children_invalid() {
    use std::sync::Arc;
    use std::thread;

    let db = Arc::new(temp_db());
    let t_before = run_id_of(&db, "T").unwrap();

    let mut handles = Vec::new();
    for k in 0..6 {
        let db = db.clone();
        handles.push(thread::spawn(move || {
            let fx = fixture();
            let mut poly = fx
                .populations
                .iter()
                .find(|p| p.id == "SMALL")
                .unwrap()
                .gate
                .clone()
                .unwrap()
                .polygon;
            for v in poly.vertices.iter_mut() {
                v.x = (v.x - k as f64 * 5.0).max(80.0).min(420.0);
            }
            db.edit_gate("SMALL", &format!("并发编辑 {k}"), poly)
                .map(|_| ())
        }));
    }
    for h in handles {
        h.join().unwrap().unwrap();
    }

    // 串行化后只有一个最终当前运行；T 的每一个旧运行（含最初的）都必须是 invalid。
    let s = db.state().unwrap();
    let current = s.runs.iter().find(|r| r.population_id == "T").unwrap();
    assert_eq!(current.status, "ok");
    for r in db.runs_of("T").unwrap() {
        if r.id == current.run_id {
            assert_eq!(r.status, "ok");
        } else {
            assert_eq!(r.status, "invalid", "旧子门运行 {} 必须明确失效", r.id);
        }
    }
    assert!(db
        .runs_of("T")
        .unwrap()
        .iter()
        .any(|r| r.id == t_before && r.status == "invalid"));
}
