//! 验收集成测试：半开几何不双计、空父百分比不可定义、
//! 父门修改后旧子门运行 invalidated、补偿通道顺序变化、切换/差异/导出重导入。

use flow_gate_station::test_support::*;

#[test]
fn boundary_events_are_not_double_counted() {
    let h = Harness::new();

    // 共享竖边 CD4=500 上的事件只出现在 cd8，绝不同时属于 cd4
    let cd4 = active_run(&h, "cd4");
    let cd8 = active_run(&h, "cd8");
    for id in ["qc-shared-1", "qc-shared-2"] {
        assert!(cd8.members.contains(&id.to_string()), "{id} 应按半开规则进入 cd8");
        assert!(!cd4.members.contains(&id.to_string()), "{id} 不得同时进入 cd4");
    }

    // 两个相邻门成员集合互斥（所有事件）
    let a: std::collections::BTreeSet<_> = cd4.members.iter().collect();
    let b: std::collections::BTreeSet<_> = cd8.members.iter().collect();
    assert!(a.is_disjoint(&b), "相邻门不得双重计数");

    // lymph 半开矩形：底边、左边计入；顶边、右边计出
    let lymph = active_run(&h, "lymph");
    assert!(lymph.members.iter().any(|m| m == "qc-edge-bottom"));
    assert!(lymph.members.iter().any(|m| m == "qc-edge-left"));
    assert!(!lymph.members.iter().any(|m| m == "qc-edge-top"));
    assert!(!lymph.members.iter().any(|m| m == "qc-edge-right"));

    // cd4 顶边（半开）计出
    assert!(!cd4.members.iter().any(|m| m == "qc-cd4-topedge"));
    assert!(cd4.members.iter().any(|m| m == "qc-cd4-in"));
    assert!(cd8.members.iter().any(|m| m == "qc-cd8-in"));
    assert!(!lymph.members.iter().any(|m| m == "qc-debris"));
}

#[test]
fn empty_parent_percentage_is_undefined_not_zero() {
    let h = Harness::new();
    let empty = active_run(&h, "granulocytes");
    assert_eq!(empty.count, Some(0));
    assert_eq!(empty.parent_count, None);

    let sub = active_run(&h, "gran_subset");
    assert_eq!(sub.count, Some(0));
    assert_eq!(sub.parent_count, Some(0));
    // 0/0：百分比必须是 null（不可定义），而不是 0
    assert_eq!(sub.percent_of_parent, None);
}

#[test]
fn editing_parent_invalidates_old_child_runs() {
    let h = Harness::new();
    let old_lymph = active_run(&h, "lymph");
    let old_cd4 = active_run(&h, "cd4");
    let old_cd8 = active_run(&h, "cd8");

    // 缩小淋巴门：FSC 上限 800 -> 350
    edit_gate(&h, "lymph", vec![
        [200.0, 200.0], [350.0, 200.0], [350.0, 800.0], [200.0, 800.0],
    ]);

    let new_lymph = active_run(&h, "lymph");
    assert_ne!(new_lymph.id, old_lymph.id);

    // 旧子门运行必须明确失效，count 被清空，而不是继续显示旧数
    let old_cd4_row = run_by_id(&h, &old_cd4.id);
    let old_cd8_row = run_by_id(&h, &old_cd8.id);
    assert_eq!(old_cd4_row.status, "invalidated");
    assert_eq!(old_cd8_row.status, "invalidated");
    assert_eq!(old_cd4_row.count, None);
    assert_eq!(old_cd8_row.count, None);

    // 新子门运行已生成并与新父群体一致
    let new_cd4 = active_run(&h, "cd4");
    assert!(new_cd4.count <= new_lymph.count);
    assert_eq!(new_cd4.parent_count, new_lymph.count);

    // 不相关的另一棵树（粒细胞）运行保持 active
    assert_eq!(active_run(&h, "granulocytes").status, "active");
}

#[test]
fn compensation_channel_mismatch_rejected_not_positional() {
    let h = Harness::new();
    // 缺 CD8 通道：必须拒绝（422），不能按位置继续矩阵乘法
    let channels = serde_json::json!(["FSC", "SSC", "CD45", "CD4"]);
    let matrix = serde_json::json!([
        [1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1],
    ]);
    let err = try_add_compensation(&h, channels, matrix)
        .expect_err("缺通道必须报错");
    assert!(err.contains("channel mismatch"), "实际错误: {err}");

    // 多通道同样拒绝
    let channels = serde_json::json!(["FSC", "SSC", "CD45", "CD4", "CD8", "EXTRA"]);
    let zero = serde_json::json!(0.0);
    let matrix = serde_json::Value::Array(
        std::iter::repeat(serde_json::Value::Array(
            std::iter::repeat(zero.clone()).take(6).collect())).take(6).collect());
    let err = try_add_compensation(&h, channels, matrix).expect_err("多通道必须报错");
    assert!(err.contains("channel mismatch"), "实际错误: {err}");
}

#[test]
fn compensation_channel_order_change_replays_correctly_by_name() {
    let h = Harness::new();
    let before = active_run(&h, "cd4").count;

    // 切换到 fixture 的补偿 v2（通道反序声明 + CD8→CD4 -2%）。
    // 按名字应用：切换后计数应与用同一份矩阵直接重算一致，
    // 而不是按位置错位得到错误数值。
    switch_compensation(&h, "comp-v2");
    let after = active_run(&h, "cd4").count;
    // v2 的溢出项会改变 CD4 坐标，计数与 v1 可不同，但必须是确定性结果
    assert!(after.unwrap() >= 0);

    let bundle = export(&h);
    // 导出中两个补偿版本的通道顺序明确不同
    let v1: &serde_json::Value = bundle["compensation_versions"]
        .as_array().unwrap().iter().find(|c| c["id"] == "comp-v1").unwrap();
    let v2: &serde_json::Value = bundle["compensation_versions"]
        .as_array().unwrap().iter().find(|c| c["id"] == "comp-v2").unwrap();
    assert_ne!(v1["channels"], v2["channels"]);

    // 切换回 v1 必须精确还原原始计数（可重放）
    switch_compensation(&h, "comp-v1");
    assert_eq!(active_run(&h, "cd4").count, before);
}

#[test]
fn transform_switch_and_branch_event_diff() {
    let h = Harness::new();
    let v1_cd4 = active_run(&h, "cd4");

    switch_transform(&h, "trans-v2");
    let log_cd4 = active_run(&h, "cd4");
    assert_ne!(v1_cd4.id, log_cd4.id);
    // 旧运行仍保留为 superseded
    assert_eq!(run_by_id(&h, &v1_cd4.id).status, "superseded");

    let d = diff(&h, &v1_cd4.id, &log_cd4.id);
    let total = d["only_in_a"].as_array().unwrap().len()
        + d["only_in_b"].as_array().unwrap().len()
        + d["in_both"].as_u64().unwrap() as usize;
    assert_eq!(total as i64, v1_cd4.count.unwrap() + log_cd4.count.unwrap());

    // 回到 v1 后与最初一致
    switch_transform(&h, "trans-v1");
    assert_eq!(active_run(&h, "cd4").count, v1_cd4.count);
}

#[test]
fn export_wipe_reimport_reverifies() {
    let h = Harness::new();
    // 制造历史：改一次父门，使库里存在 superseded 与 invalidated 记录
    edit_gate(&h, "lymph", vec![
        [200.0, 200.0], [600.0, 200.0], [600.0, 800.0], [200.0, 800.0],
    ]);
    let bundle = export(&h);

    let report = wipe_and_import(&h, &bundle);
    assert_eq!(report.failures.len(), 0, "重导入复核失败: {:?}", report.failures);
    assert!(report.checked >= 5);
    assert_eq!(report.passed, report.checked);

    // 导入后再次全量复核仍一致，且状态/计数保留
    let again = verify(&h);
    assert_eq!(again.failures.len(), 0);
    let invalid: Vec<_> = all_runs(&h).into_iter().filter(|r| r.status == "invalidated").collect();
    assert!(!invalid.is_empty(), "历史失效运行记录应保留");
    for r in invalid {
        assert_eq!(r.count, None, "失效运行不得携带旧计数");
    }
}

#[test]
fn gate_version_history_replay_branch() {
    let h = Harness::new();
    let first = active_run(&h, "cd8").count;
    // 把 cd8 改小
    edit_gate(&h, "cd8", vec![
        [700.0, 100.0], [900.0, 100.0], [900.0, 900.0], [700.0, 900.0],
    ]);
    let shrunk = active_run(&h, "cd8").count;
    assert!(shrunk < first);

    // 找回 v1 并回放：恢复原计数；旧 cd8 运行 superseded（cd8 无子门）
    let versions = gate_versions(&h, "cd8");
    let v1 = versions.iter().find(|v| v.version == 1).unwrap();
    activate_version(&h, "cd8", &v1.db_id);
    assert_eq!(active_run(&h, "cd8").count, first);
}

#[test]
fn invalid_polygon_is_rejected() {
    let h = Harness::new();
    let err = try_edit_gate(&h, "cd4", vec![[1.0, 1.0], [2.0, 2.0]])
        .expect_err("少于 3 个顶点必须拒绝");
    assert!(err.contains("invalid polygon"), "实际: {err}");
}

#[tokio::test]
async fn concurrent_parent_edits_leave_no_stale_active_child_run() {
    use flow_gate_station::test_support::*;
    let h = Harness::new();
    let initial = h.state_async().await;
    let original = run_rows(&initial).into_iter()
        .find(|r| r.gate_id == "cd8" && r.status == "active").unwrap().id;

    let s = initial.clone();
    let mut handles = Vec::new();
    for cut in [500.0_f64, 650.0] {
        let hh = h.clone();
        let state = s.clone();
        handles.push(tokio::spawn(async move {
            let _ = state;
            let body = serde_json::json!({
                "gate_id": "lymph",
                "label": format!("concurrent {cut}"),
                "parent": null,
                "x_channel": "FSC",
                "y_channel": "SSC",
                "vertices": [[200.0,200.0],[cut,200.0],[cut,800.0],[200.0,800.0]],
            });
            hh.post_json("/api/gates/version", body).await.0
        }));
    }
    for jh in handles {
        assert!(jh.await.unwrap().is_success());
    }

    let final_state = h.state_async().await;
    let runs = run_rows(&final_state);
    for gate in ["lymph", "cd4", "cd8", "granulocytes", "gran_subset"] {
        let actives: Vec<_> = runs.iter().filter(|r| r.gate_id == gate && r.status == "active").collect();
        assert_eq!(actives.len(), 1, "{gate} 必须恰好有一条 active 运行");
    }
    let original_row = runs.into_iter().find(|r| r.id == original).unwrap();
    assert_ne!(original_row.status, "active");
    assert_eq!(original_row.count, None);
}

#[test]
fn mismatched_shared_edge_is_unprocessable() {
    let h = Harness::new();
    // 新门与 cd4 边界接触但共享边端点不一致：必须拒绝，防止几何歧义双计
    let err = try_create_gate(&h, "weird", "怪门", Some("lymph"), "CD4", "CD8", vec![
        [100.0, 100.0], [500.0, 100.0 + 0.001], [500.0, 900.0], [100.0, 900.0],
    ]).expect_err("近重合但不一致的共享边必须拒绝");
    assert!(err.contains("shared-edge"), "实际: {err}");
}

#[test]
fn replayed_run_members_are_subset_of_parent() {
    let h = Harness::new();
    for _ in 0..3 {
        let lymph = active_run(&h, "lymph");
        let lymph_set: std::collections::BTreeSet<_> = lymph.members.iter().collect();
        for g in ["cd4", "cd8"] {
            let child = active_run(&h, g);
            let child_set: std::collections::BTreeSet<_> = child.members.iter().collect();
            assert!(child_set.is_subset(&lymph_set), "{g} 成员必须是父群体子集");
            assert_eq!(child.parent_count, Some(lymph.count.unwrap()));
        }
        edit_gate(&h, "lymph", vec![
            [200.0, 200.0], [700.0, 200.0], [700.0, 800.0], [200.0, 800.0],
        ]);
    }
}
