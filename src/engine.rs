//! 重放引擎：把“事件 + 补偿版本 + 变换版本 + 门定义版本”确定性地求值成运行。
//!
//! 关键不变量：
//! * 运行只追加。父门几何被修改后，该门旧运行标记 superseded，
//!   其所有后代旧运行标记 invalidated（count 置 NULL，UI 不得再显示旧数）。
//! * 补偿/变换切换影响所有门：旧活动运行全部 superseded 后整体重放。
//! * 父群体为空 => 子门 count 仍可计算，但百分比为 null（不可定义），不是 0。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::compensation::Compensation;
use crate::domain::{Point, Polygon, RunStatus};
use crate::geometry;
use crate::storage::{
    active_context, active_gate_versions, gate_version_by_id, GateVersionRow, Store,
};
use crate::fixture;

#[derive(Debug, Clone)]
pub struct PreparedEvent {
    pub id: String,
    pub batch: String,
    /// 变换后按通道名取值
    pub values: BTreeMap<String, f64>,
}

#[derive(Debug, Clone)]
pub struct EngineError(pub String);

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EngineError {}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        EngineError(format!("db: {e}"))
    }
}
impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError(format!("json: {e}"))
    }
}
impl From<crate::compensation::CompensationError> for EngineError {
    fn from(e: crate::compensation::CompensationError) -> Self {
        EngineError(format!("compensation channel mismatch: {e:?}"))
    }
}

fn new_id(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{prefix}-{nanos:x}-{n}")
}

pub fn parse_compensation(
    channels_json: &str,
    matrix_json: &str,
) -> Result<Compensation, EngineError> {
    let channels: Vec<String> = serde_json::from_str(channels_json)?;
    let rows: Vec<Vec<f64>> = serde_json::from_str(matrix_json)?;
    Ok(Compensation { channels, rows })
}

pub fn parse_vertices(json: &str) -> Result<Vec<Point>, EngineError> {
    let raw: Vec<(f64, f64)> = serde_json::from_str(json)?;
    Ok(raw.into_iter().map(|(x, y)| Point::new(x, y)).collect())
}

/// 载入事件并应用补偿 + 逐通道变换。
pub fn prepare_events(
    store: &Store,
    comp: &Compensation,
    transforms: &BTreeMap<String, crate::domain::TransformParams>,
) -> Result<Vec<PreparedEvent>, EngineError> {
    let data_channels = fixture::data_channels();
    comp.validate(&data_channels)?;
    let raw = store.load_events()?;
    let mut out = Vec::with_capacity(raw.len());
    for (id, batch, raw_json) in raw {
        let map: BTreeMap<String, f64> = serde_json::from_str(&raw_json)?;
        let ordered: Vec<f64> = data_channels
            .iter()
            .map(|c| map.get(c).copied().unwrap_or(0.0))
            .collect();
        let compensated = comp.apply(&data_channels, &ordered)?;
        let mut values = BTreeMap::new();
        for (ch, v) in data_channels.iter().zip(compensated) {
            let tp = transforms.get(ch).cloned().unwrap_or_default();
            values.insert(ch.clone(), tp.apply(v));
        }
        out.push(PreparedEvent { id, batch, values });
    }
    Ok(out)
}

fn polygon_of(gv: &GateVersionRow) -> Result<Polygon, EngineError> {
    let pts = parse_vertices(&gv.vertices_json)?;
    Polygon::new(pts).map_err(|e| EngineError(format!("gate {} invalid polygon: {e:?}", gv.gate_id)))
}

fn topo_order(gvs: &[GateVersionRow]) -> Vec<usize> {
    let by_gate: HashMap<&str, &GateVersionRow> =
        gvs.iter().map(|g| (g.gate_id.as_str(), g)).collect();
    let mut order = Vec::new();
    let mut visiting = BTreeSet::new();
    fn visit<'a>(
        gv: &'a GateVersionRow,
        by_gate: &HashMap<&str, &'a GateVersionRow>,
        visiting: &mut BTreeSet<String>,
        order: &mut Vec<&'a GateVersionRow>,
    ) {
        if order.iter().any(|g| g.gate_id == gv.gate_id) {
            return;
        }
        if !visiting.insert(gv.gate_id.clone()) {
            return; // 环保护
        }
        if let Some(p) = &gv.parent {
            if let Some(pgv) = by_gate.get(p.as_str()) {
                visit(pgv, by_gate, visiting, order);
            }
        }
        visiting.remove(&gv.gate_id);
        if !order.iter().any(|g| g.gate_id == gv.gate_id) {
            order.push(gv);
        }
    }
    for gv in gvs {
        visit(gv, &by_gate, &mut visiting, &mut order);
    }
    order.into_iter().map(|g| gvs.iter().position(|x| x.gate_id == g.gate_id).unwrap()).collect()
}

/// 同层级（同父）多边形共享边一致性校验。
fn validate_siblings(gvs: &[GateVersionRow]) -> Result<(), EngineError> {
    let mut groups: HashMap<Option<String>, Vec<(String, Polygon)>> = HashMap::new();
    for gv in gvs {
        groups
            .entry(gv.parent.clone())
            .or_default()
            .push((gv.gate_id.clone(), polygon_of(gv)?));
    }
    for (_, polys) in groups {
        geometry::validate_shared_edges(&polys)
            .map_err(|e| EngineError(format!("shared-edge validation failed: {e:?}")))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub id: String,
    pub gate_id: String,
    pub gate_label: String,
    pub gate_version_id: String,
    pub gate_version: i64,
    pub parent_gate: Option<String>,
    pub parent_run: Option<String>,
    pub compensation_version: String,
    pub transform_version: String,
    pub status: RunStatus,
    pub count: Option<i64>,
    pub parent_count: Option<i64>,
    /// 父群体为空 => None（不可定义）；失效运行也为 None
    pub percent_of_parent: Option<f64>,
    pub lineage: String,
    pub created_at: String,
    pub x_channel: String,
    pub y_channel: String,
    pub vertices: Vec<(f64, f64)>,
    pub verify_ok: Option<bool>,
    pub verify_detail: Option<String>,
}

const RUN_COLS: &str =
    "id,gate_id,gate_version,parent_run,compensation_version,transform_version,
     status,count,parent_count,lineage,created_at";

fn load_run_infos(conn: &rusqlite::Connection, only_active: bool) -> Result<Vec<RunInfo>, EngineError> {
    let sql = format!(
        "SELECT r.id,r.gate_id,r.gate_version,r.parent_run,r.compensation_version,r.transform_version,
                r.status,r.count,r.parent_count,r.lineage,r.created_at,
                gv.version,gv.label,gv.parent,gv.x_channel,gv.y_channel,gv.vertices_json,
                v.ok,v.detail
         FROM runs r
         JOIN gate_versions gv ON gv.id = r.gate_version
         LEFT JOIN run_verify v ON v.run_id = r.id
         {} ORDER BY r.created_at",
        if only_active { "WHERE r.status='active'" } else { "" }
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut out = Vec::new();
    let rows = stmt.query_map([], |r| {
        let status = RunStatus::parse(&r.get::<_, String>(6)?);
        let count: Option<i64> = r.get(7)?;
        let parent_count: Option<i64> = r.get(8)?;
        let vertices_json: String = r.get(16)?;
        let vertices: Vec<(f64, f64)> = serde_json::from_str(&vertices_json).unwrap_or_default();
        let percent = match (status, count, parent_count) {
            (RunStatus::Active, Some(c), Some(pc)) if pc > 0 => Some(100.0 * c as f64 / pc as f64),
            _ => None,
        };
        Ok(RunInfo {
            id: r.get(0)?,
            gate_id: r.get(1)?,
            gate_version_id: r.get(2)?,
            parent_run: r.get(3)?,
            compensation_version: r.get(4)?,
            transform_version: r.get(5)?,
            status,
            count,
            parent_count,
            percent_of_parent: percent,
            lineage: r.get(9)?,
            created_at: r.get(10)?,
            gate_version: r.get(11)?,
            gate_label: r.get(12)?,
            parent_gate: r.get(13)?,
            x_channel: r.get(14)?,
            y_channel: r.get(15)?,
            vertices,
            verify_ok: r.get::<_, Option<i64>>(17)?.map(|x| x != 0),
            verify_detail: r.get(18)?,
        })
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn list_runs(store: &Store, only_active: bool) -> Result<Vec<RunInfo>, EngineError> {
    let conn = store.conn.lock().unwrap();
    load_run_infos(&conn, only_active)
}

/// 在已开启的事务里，对给定门集合按拓扑序重放，生成新的 active 运行。
fn replay_in_tx(
    tx: &rusqlite::Transaction,
    gate_rows: &[GateVersionRow],
    events: &[PreparedEvent],
    comp_id: &str,
    trans_id: &str,
) -> Result<HashMap<String, String>, EngineError> {
    let mut polygons: HashMap<&str, Polygon> = HashMap::new();
    for gv in gate_rows {
        polygons.insert(gv.gate_id.as_str(), polygon_of(gv)?);
    }
    let mut run_id_by_gate: HashMap<String, String> = HashMap::new();
    let mut members_by_gate: HashMap<String, Vec<String>> = HashMap::new();

    // 子树重放时，父链上的门不在 gate_rows 中；预载其当前活动运行与成员，
    // 使新运行正确挂载 parent_run、只在父群体内求值。
    {
        let gv_by_gate: HashMap<&str, &GateVersionRow> =
            gate_rows.iter().map(|g| (g.gate_id.as_str(), g)).collect();
        for gv in gate_rows {
            let mut pid = gv.parent.clone();
            while let Some(cur) = pid {
                if gv_by_gate.contains_key(cur.as_str()) {
                    break; // 链上剩余部分会在本批拓扑重放中处理
                }
                let row: Option<(String, String)> = tx
                    .query_row(
                        "SELECT r.id, r.event_ids_json FROM runs r
                         JOIN gate_versions gv ON gv.id = r.gate_version
                         WHERE gv.gate_id=?1 AND r.status='active' LIMIT 1",
                        params![cur],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((rid, ids_json)) = row {
                    if run_id_by_gate.contains_key(&cur) {
                        break;
                    }
                    run_id_by_gate.insert(cur.clone(), rid);
                    members_by_gate.insert(cur.clone(), serde_json::from_str(&ids_json)?);
                }
                pid = tx
                    .query_row("SELECT parent FROM gates WHERE id=?1", params![cur], |r| {
                        r.get::<_, Option<String>>(0)
                    })
                    .optional()?
                    .flatten();
            }
        }
    }

    for idx in topo_order(gate_rows) {
        let gv = &gate_rows[idx];
        let poly = &polygons[gv.gate_id.as_str()];
        let (parent_run_id, parent_members): (Option<String>, Option<&Vec<String>>) = match &gv.parent {
            Some(p) => {
                let rid = run_id_by_gate.get(p).cloned();
                (rid, members_by_gate.get(p))
            }
            None => (None, None),
        };
        let parent_count = match &gv.parent {
            Some(p) => Some(members_by_gate.get(p).map(|m| m.len() as i64).unwrap_or(0)),
            None => None,
        };

        let mut selected: Vec<&PreparedEvent> = match parent_members {
            Some(pm) => {
                // 父群体为空时仍正常求值（结果必为空），百分比在展示层标记不可定义
                events.iter().filter(|e| pm.binary_search(&e.id).is_ok()).collect()
            }
            None => events.iter().collect(),
        };
        selected.sort_by(|a, b| a.id.cmp(&b.id));
        selected.dedup_by(|a, b| a.id == b.id);

        let mut members: Vec<String> = Vec::new();
        for e in &selected {
            let x = *e.values.get(&gv.x_channel).unwrap_or(&f64::NAN);
            let y = *e.values.get(&gv.y_channel).unwrap_or(&f64::NAN);
            if geometry::contains(poly, Point::new(x, y)) {
                members.push(e.id.clone());
            }
        }
        let count = members.len() as i64;
        let run_id = new_id("run");
        let lineage = match &parent_run_id {
            Some(pid) => {
                let parent_lineage: String = tx.query_row(
                    "SELECT lineage FROM runs WHERE id=?1", params![pid], |r| r.get(0))?;
                format!("{parent_lineage}{pid}/")
            }
            None => String::new(),
        };
        tx.execute(
            "INSERT INTO runs(id,gate_id,gate_version,parent_run,compensation_version,
                 transform_version,status,count,parent_count,event_ids_json,lineage,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,'active',?7,?8,?9,?10,?11)",
            params![
                run_id, gv.gate_id, gv.id, parent_run_id, comp_id, trans_id,
                count, parent_count,
                serde_json::to_string(&members)?,
                lineage, crate::storage::current_ts()
            ],
        )?;
        run_id_by_gate.insert(gv.gate_id.clone(), run_id);
        members_by_gate.insert(gv.gate_id.clone(), members);
    }
    Ok(run_id_by_gate)
}

fn descendant_gate_ids(rows: &[GateVersionRow], root: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let mut frontier = vec![root.to_string()];
    while let Some(g) = frontier.pop() {
        for r in rows {
            if r.parent.as_deref() == Some(g.as_str()) && result.insert(r.gate_id.clone()) {
                frontier.push(r.gate_id.clone());
            }
        }
    }
    result
}

fn active_context_and_events(
    conn: &rusqlite::Connection,
) -> Result<(Compensation, BTreeMap<String, crate::domain::TransformParams>, Vec<PreparedEvent>, String, String), EngineError> {
    let ctx = active_context(conn)?;
    let comp = parse_compensation(&ctx.channels_json, &ctx.matrix_json)?;
    let transforms: BTreeMap<String, crate::domain::TransformParams> =
        serde_json::from_str(&ctx.transform_params_json)?;
    let data_channels = fixture::data_channels();
    comp.validate(&data_channels)?;
    let raw: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare("SELECT id,batch,raw_json FROM events ORDER BY sort_index")?;
        let mapped = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut events = Vec::with_capacity(raw.len());
    for (id, batch, raw_json) in raw {
        let map: BTreeMap<String, f64> = serde_json::from_str(&raw_json)?;
        let ordered: Vec<f64> = data_channels
            .iter()
            .map(|c| map.get(c).copied().unwrap_or(0.0))
            .collect();
        let compensated = comp.apply(&data_channels, &ordered)?;
        let mut values = BTreeMap::new();
        for (ch, v) in data_channels.iter().zip(compensated) {
            let tp = transforms.get(ch).cloned().unwrap_or_default();
            values.insert(ch.clone(), tp.apply(v));
        }
        events.push(PreparedEvent { id, batch, values });
    }
    Ok((comp, transforms, events, ctx.compensation_id, ctx.transform_id))
}

/// 全量重放（首次播种 / 补偿·变换切换后）。调用方须先完成旧运行状态迁移。
pub fn recompute_all_locked(
    conn: &rusqlite::Connection,
) -> Result<(), EngineError> {
    let (comp, transforms, events, comp_id, trans_id) = active_context_and_events(conn)?;
    let gate_rows = active_gate_versions(conn)?;
    validate_siblings(&gate_rows)?;
    drop((comp, transforms));
    replay_in_tx_unwrapped(conn, &gate_rows, &events, &comp_id, &trans_id)
}

/// Connection 上的即时事务包装（调用方持锁）。
fn replay_in_tx_unwrapped(
    conn: &rusqlite::Connection,
    gate_rows: &[GateVersionRow],
    events: &[PreparedEvent],
    comp_id: &str,
    trans_id: &str,
) -> Result<(), EngineError> {
    let tx = conn.unchecked_transaction()?;
    replay_in_tx(&tx, gate_rows, events, comp_id, trans_id)?;
    tx.commit()?;
    Ok(())
}

/// 为某门添加新定义版本并重放该子树；旧自运行 superseded、旧子孙运行 invalidated。
pub fn add_gate_version(
    store: &Store,
    gate_id: &str,
    label: &str,
    parent: Option<&str>,
    x_channel: &str,
    y_channel: &str,
    vertices: Vec<(f64, f64)>,
) -> Result<String, EngineError> {
    let poly = Polygon::new(
        vertices.iter().map(|(x, y)| Point::new(*x, *y)).collect(),
    )
    .map_err(|e| EngineError(format!("invalid polygon: {e:?}")))?;

    let mut conn = store.conn.lock().unwrap();
    let tx = conn.transaction()?;

    // 先验证新定义与（替换后的）兄弟门之间共享边几何一致
    {
        let mut all = active_gate_versions(&tx)?;
        all.retain(|g| g.gate_id != gate_id);
        let temp_id = "pending-gv";
        all.push(GateVersionRow {
            id: temp_id.to_string(),
            gate_id: gate_id.to_string(),
            version: -1,
            label: label.to_string(),
            parent: parent.map(str::to_string),
            x_channel: x_channel.to_string(),
            y_channel: y_channel.to_string(),
            vertices_json: serde_json::to_string(&vertices)?,
        });
        validate_siblings(&all)?;
    }
    let _ = poly;

    let gvs = active_gate_versions(&tx)?;
    let descendants = descendant_gate_ids(&gvs, gate_id);

    // 1) 该门旧活动运行 -> superseded（保留旧计数作为历史）
    tx.execute(
        "UPDATE runs SET status='superseded' WHERE gate_id=?1 AND status='active'",
        params![gate_id],
    )?;
    // 2) 后代旧活动运行 -> invalidated，并清空计数：绝不继续显示旧数
    if !descendants.is_empty() {
        let placeholders = std::iter::repeat("?").take(descendants.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE runs SET status='invalidated', count=NULL, parent_count=NULL
             WHERE status='active' AND gate_id IN ({placeholders})"
        );
        let mut params_vec: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
        for d in &descendants {
            params_vec.push(d);
        }
        tx.execute(&sql, params_vec.as_slice())?;
    }

    // 3) 写入并激活新版本
    let version = tx.query_row(
        "SELECT COALESCE(MAX(version),0)+1 FROM gate_versions WHERE gate_id=?1",
        params![gate_id],
        |r| r.get::<_, i64>(0),
    )?;
    let gv_id = new_id("gv");
    tx.execute(
        "UPDATE gate_versions SET active=0 WHERE gate_id=?1", params![gate_id])?;
    tx.execute(
        "INSERT INTO gate_versions(id,gate_id,version,label,parent,x_channel,y_channel,
             vertices_json,active,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,?9)",
        params![
            gv_id, gate_id, version, label, parent, x_channel, y_channel,
            serde_json::to_string(&vertices)?, crate::storage::current_ts()
        ],
    )?;

    // 4) 用当前活动补偿/变换准备事件，只重放受影响子树
    let (_, _, events, comp_id, trans_id) = active_context_and_events(&tx)?;
    let mut subtree = active_gate_versions(&tx)?;
    let subtree_ids: BTreeSet<String> =
        std::iter::once(gate_id.to_string()).chain(descendants.iter().cloned()).collect();
    subtree.retain(|g| subtree_ids.contains(&g.gate_id));
    replay_in_tx(&tx, &subtree, &events, &comp_id, &trans_id)?;

    tx.commit()?;
    Ok(gv_id)
}

/// 切换补偿或变换版本：所有旧 active 运行 superseded，然后整体重放。
pub fn switch_context(
    store: &Store,
    compensation_id: Option<&str>,
    transform_id: Option<&str>,
) -> Result<(), EngineError> {
    let mut conn = store.conn.lock().unwrap();
    let tx = conn.transaction()?;
    if let Some(id) = compensation_id {
        let n = tx.execute(
            "UPDATE compensation_versions SET active = CASE WHEN id=?1 THEN 1 ELSE 0 END",
            params![id],
        )?;
        if n == 0 {
            return Err(EngineError("compensation version not found".into()));
        }
    }
    if let Some(id) = transform_id {
        let n = tx.execute(
            "UPDATE transform_versions SET active = CASE WHEN id=?1 THEN 1 ELSE 0 END",
            params![id],
        )?;
        if n == 0 {
            return Err(EngineError("transform version not found".into()));
        }
    }

    // 校验新补偿通道集合与数据通道一致 —— 不通过则回滚，旧运行保持原状
    let ctx = active_context(&tx)?;
    let comp = parse_compensation(&ctx.channels_json, &ctx.matrix_json)?;
    comp.validate(&fixture::data_channels())?;

    tx.execute("UPDATE runs SET status='superseded' WHERE status='active'", [])?;

    let (_, _, events, comp_id, trans_id) = active_context_and_events(&tx)?;
    let gate_rows = active_gate_versions(&tx)?;
    validate_siblings(&gate_rows)?;
    replay_in_tx(&tx, &gate_rows, &events, &comp_id, &trans_id)?;
    tx.commit()?;
    Ok(())
}

/// 激活某门的某个历史定义版本（回放分支）；同样使旧自运行 superseded、旧子孙 invalidated。
pub fn activate_gate_version(
    store: &Store,
    gate_id: &str,
    gate_version_db_id: &str,
) -> Result<(), EngineError> {
    let mut conn = store.conn.lock().unwrap();
    let tx = conn.transaction()?;
    let target = gate_version_by_id(&tx, gate_version_db_id)?;
    if target.gate_id != gate_id {
        return Err(EngineError("gate version belongs to another gate".into()));
    }
    let gvs = active_gate_versions(&tx)?;
    let descendants = descendant_gate_ids(&gvs, gate_id);

    tx.execute(
        "UPDATE runs SET status='superseded' WHERE gate_id=?1 AND status='active'",
        params![gate_id],
    )?;
    if !descendants.is_empty() {
        let placeholders = std::iter::repeat("?").take(descendants.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE runs SET status='invalidated', count=NULL, parent_count=NULL
             WHERE status='active' AND gate_id IN ({placeholders})"
        );
        let p: Vec<&dyn rusqlite::types::ToSql> = descendants.iter().map(|d| d as _).collect();
        tx.execute(&sql, p.as_slice())?;
    }
    tx.execute("UPDATE gate_versions SET active=0 WHERE gate_id=?1", params![gate_id])?;
    tx.execute("UPDATE gate_versions SET active=1 WHERE id=?1", params![gate_version_db_id])?;

    let (_, _, events, comp_id, trans_id) = active_context_and_events(&tx)?;
    let mut subtree = active_gate_versions(&tx)?;
    let ids: BTreeSet<String> =
        std::iter::once(gate_id.to_string()).chain(descendants.iter().cloned()).collect();
    subtree.retain(|g| ids.contains(&g.gate_id));
    replay_in_tx(&tx, &subtree, &events, &comp_id, &trans_id)?;
    tx.commit()?;
    Ok(())
}

pub fn create_gate(
    store: &Store,
    gate_id: &str,
    label: &str,
    parent: Option<&str>,
    x_channel: &str,
    y_channel: &str,
    vertices: Vec<(f64, f64)>,
) -> Result<(), EngineError> {
    Polygon::new(vertices.iter().map(|(x, y)| Point::new(*x, *y)).collect())
        .map_err(|e| EngineError(format!("invalid polygon: {e:?}")))?;
    let mut conn = store.conn.lock().unwrap();
    let tx = conn.transaction()?;
    if let Some(p) = parent {
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM gates WHERE id=?1", params![p], |r| r.get(0))?;
        if exists == 0 {
            return Err(EngineError("parent gate does not exist".into()));
        }
    }
    tx.execute(
        "INSERT INTO gates(id,label,parent,created_at) VALUES(?1,?2,?3,?4)",
        params![gate_id, label, parent, crate::storage::current_ts()],
    )?;
    let gv_id = new_id("gv");
    tx.execute(
        "INSERT INTO gate_versions(id,gate_id,version,label,parent,x_channel,y_channel,
             vertices_json,active,created_at)
         VALUES(?1,?2,1,?3,?4,?5,?6,?7,1,?8)",
        params![
            gv_id, gate_id, label, parent, x_channel, y_channel,
            serde_json::to_string(&vertices)?, crate::storage::current_ts()
        ],
    )?;

    // 共享边校验（含新门）
    validate_siblings(&active_gate_versions(&tx)?)?;

    // 新门及其子树（新门暂无子门）单独重放
    let (_, _, events, comp_id, trans_id) = active_context_and_events(&tx)?;
    let gv = gate_version_by_id(&tx, &gv_id)?;
    replay_in_tx(&tx, &[gv], &events, &comp_id, &trans_id)?;
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct RunDiff {
    pub run_a: String,
    pub run_b: String,
    pub only_in_a: Vec<String>,
    pub only_in_b: Vec<String>,
    pub in_both: usize,
}

pub fn run_event_ids(store: &Store, run_id: &str) -> Result<Vec<String>, EngineError> {
    let conn = store.conn.lock().unwrap();
    let json: String = conn.query_row(
        "SELECT event_ids_json FROM runs WHERE id=?1", params![run_id], |r| r.get(0))?;
    Ok(serde_json::from_str(&json)?)
}

/// 按事件身份比较两个运行（可跨分支/跨版本）的进出差异。
pub fn diff_runs(store: &Store, a: &str, b: &str) -> Result<RunDiff, EngineError> {
    let ia: BTreeSet<String> = run_event_ids(store, a)?.into_iter().collect();
    let ib: BTreeSet<String> = run_event_ids(store, b)?.into_iter().collect();
    Ok(RunDiff {
        run_a: a.to_string(),
        run_b: b.to_string(),
        only_in_a: ia.difference(&ib).cloned().collect(),
        only_in_b: ib.difference(&ia).cloned().collect(),
        in_both: ia.intersection(&ib).count(),
    })
}

#[derive(Debug, Serialize)]
pub struct GateVersionInfo {
    pub db_id: String,
    pub version: i64,
    pub label: String,
    pub parent: Option<String>,
    pub x_channel: String,
    pub y_channel: String,
    pub vertices: Vec<(f64, f64)>,
    pub active: bool,
    pub created_at: String,
}

pub fn list_gate_versions(store: &Store, gate_id: &str) -> Result<Vec<GateVersionInfo>, EngineError> {
    let conn = store.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id,version,label,parent,x_channel,y_channel,vertices_json,active,created_at
         FROM gate_versions WHERE gate_id=?1 ORDER BY version")?;
    let rows = stmt.query_map(params![gate_id], |r| {
        let vj: String = r.get(6)?;
        Ok(GateVersionInfo {
            db_id: r.get(0)?,
            version: r.get(1)?,
            label: r.get(2)?,
            parent: r.get(3)?,
            x_channel: r.get(4)?,
            y_channel: r.get(5)?,
            vertices: serde_json::from_str(&vj).unwrap_or_default(),
            active: r.get::<_, i64>(7)? != 0,
            created_at: r.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    pub id: String,
    pub label: String,
    pub active: bool,
    pub created_at: String,
}

pub fn list_compensations(store: &Store) -> Result<Vec<VersionInfo>, EngineError> {
    let conn = store.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id,label,active,created_at FROM compensation_versions ORDER BY rowid")?;
    let rows = stmt.query_map([], |r| {
        Ok(VersionInfo {
            id: r.get(0)?, label: r.get(1)?,
            active: r.get::<_, i64>(2)? != 0, created_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn list_transforms(store: &Store) -> Result<Vec<VersionInfo>, EngineError> {
    let conn = store.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id,label,active,created_at FROM transform_versions ORDER BY rowid")?;
    let rows = stmt.query_map([], |r| {
        Ok(VersionInfo {
            id: r.get(0)?, label: r.get(1)?,
            active: r.get::<_, i64>(2)? != 0, created_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---------- 播种 / 版本创建 ----------

pub fn add_compensation_version(
    store: &Store,
    label: &str,
    channels: &[String],
    rows: &[Vec<f64>],
    activate: bool,
) -> Result<String, EngineError> {
    let comp = Compensation { channels: channels.to_vec(), rows: rows.to_vec() };
    comp.validate(&fixture::data_channels())?; // 创建时即按名字校验，拒绝缺通道
    let id = new_id("comp");
    store.insert_compensation(
        &id, label,
        &serde_json::to_string(channels)?,
        &serde_json::to_string(rows)?,
        false,
    )?;
    if activate {
        switch_context(store, Some(&id), None)?;
    }
    Ok(id)
}

pub fn add_transform_version(
    store: &Store,
    label: &str,
    params: &BTreeMap<String, crate::domain::TransformParams>,
    activate: bool,
) -> Result<String, EngineError> {
    let id = new_id("trans");
    store.insert_transform(&id, label, &serde_json::to_string(params)?, false)?;
    if activate {
        switch_context(store, None, Some(&id))?;
    }
    Ok(id)
}

/// 空库播种固定 fixture。
pub fn seed_fixture(store: &Store) -> Result<(), EngineError> {
    let channels = fixture::data_channels();

    let events: Vec<(String, String, String)> = fixture::events()
        .iter()
        .map(|e| (e.id.clone(), e.batch.clone(), serde_json::to_string(&fixture::raw_map(e)).unwrap()))
        .collect();
    store.replace_events(&events)?;

    // 补偿 v1：单位矩阵，按数据通道顺序
    store.insert_compensation(
        "comp-v1", "补偿 v1：单位矩阵",
        &serde_json::to_string(&channels)?,
        &serde_json::to_string(&Compensation::identity(&channels).rows)?,
        true,
    )?;
    // 补偿 v2：通道顺序变化（显式反序声明，按名字应用）+ CD8->CD4 2% 溢出项
    let mut reordered = channels.clone();
    reordered.reverse();
    let mut m = Compensation::identity(&reordered);
    let i_cd4_out = reordered.iter().position(|c| c == "CD4").unwrap();
    let i_cd8_src = reordered.iter().position(|c| c == "CD8").unwrap();
    m.rows[i_cd4_out][i_cd8_src] = -0.02;
    store.insert_compensation(
        "comp-v2", "补偿 v2：通道反序声明 + CD8→CD4 2% 溢出校正",
        &serde_json::to_string(&reordered)?,
        &serde_json::to_string(&m.rows)?,
        false,
    )?;

    // 变换 v1：全通道线性（尺度 1）
    let linear: BTreeMap<_, _> = channels
        .iter()
        .map(|c| (c.clone(), crate::domain::TransformParams::default()))
        .collect();
    store.insert_transform(
        "trans-v1", "变换 v1：线性 ×1",
        &serde_json::to_string(&linear)?, true,
    )?;
    // 变换 v2：荧光通道 log10
    let mut logt = linear.clone();
    for c in ["CD45", "CD4", "CD8"] {
        logt.insert(
            c.to_string(),
            crate::domain::TransformParams {
                kind: crate::domain::TransformKind::Log10,
                scale: 250.0,
                offset: 0.0,
                bias: 250.0,
            },
        );
    }
    store.insert_transform(
        "trans-v2", "变换 v2：荧光通道 log10",
        &serde_json::to_string(&logt)?, false,
    )?;

    for (id, label, parent, x, y, verts) in fixture::gates() {
        store.insert_gate(&id, &label, parent.as_deref())?;
        store.insert_gate_version(
            &format!("{id}-v1"), &id, 1, &label, parent.as_deref(), &x, &y,
            &serde_json::to_string(&verts)?, true,
        )?;
    }

    let conn = store.conn.lock().unwrap();
    recompute_all_locked(&conn)?;
    Ok(())
}

// ---------- 导出 / 清空重导入 / 复核 ----------

#[derive(Serialize)]
pub struct Bundle {
    pub format: String,
    pub version: u32,
    pub channels: Vec<String>,
    pub events: Vec<serde_json::Value>,
    pub compensation_versions: Vec<serde_json::Value>,
    pub transform_versions: Vec<serde_json::Value>,
    pub gates: Vec<serde_json::Value>,
    pub gate_versions: Vec<serde_json::Value>,
    pub runs: Vec<serde_json::Value>,
}

pub fn export_bundle(store: &Store) -> Result<Bundle, EngineError> {
    let conn = store.conn.lock().unwrap();
    let json = |sql: &str| -> Result<Vec<serde_json::Value>, EngineError> {
        let mut stmt = conn.prepare(sql)?;
        let col_count = stmt.column_count();
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map([], |r| {
            let mut obj = serde_json::Map::new();
            for i in 0..col_count {
                let val = match r.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => serde_json::json!(x),
                    rusqlite::types::ValueRef::Real(x) => serde_json::json!(x),
                    rusqlite::types::ValueRef::Text(t) => {
                        serde_json::Value::String(std::str::from_utf8(t).unwrap_or("").to_string())
                    }
                    rusqlite::types::ValueRef::Blob(b) => {
                        serde_json::json!(String::from_utf8_lossy(b))
                    }
                };
                obj.insert(names[i].clone(), val);
            }
            Ok(serde_json::Value::Object(obj))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    };

    Ok(Bundle {
        format: "flow-gate-station-bundle".into(),
        version: 1,
        channels: fixture::data_channels(),
        events: json("SELECT id,batch,raw_json AS raw,sort_index FROM events ORDER BY sort_index")?,
        compensation_versions: json(
            "SELECT id,label,channels,matrix_json AS matrix,active,created_at
             FROM compensation_versions ORDER BY rowid")?,
        transform_versions: json(
            "SELECT id,label,params_json AS params,active,created_at
             FROM transform_versions ORDER BY rowid")?,
        gates: json("SELECT id,label,parent,created_at FROM gates ORDER BY rowid")?,
        gate_versions: json(
            "SELECT id,gate_id,version,label,parent,x_channel,y_channel,
                    vertices_json AS vertices,active,created_at
             FROM gate_versions ORDER BY rowid")?,
        runs: json(&format!(
            "SELECT {RUN_COLS},event_ids_json AS event_ids FROM runs ORDER BY created_at,id"))?,
    })
}

#[derive(Deserialize)]
struct BundleIn {
    events: Vec<EvIn>,
    compensation_versions: Vec<CIn>,
    transform_versions: Vec<TIn>,
    gates: Vec<GIn>,
    gate_versions: Vec<GvIn>,
    runs: Vec<RIn>,
}
#[derive(Deserialize)]
struct EvIn { id: String, batch: String, raw: String, sort_index: i64 }
#[derive(Deserialize)]
struct CIn { id: String, label: String, channels: String, matrix: String, active: i64, created_at: String }
#[derive(Deserialize)]
struct TIn { id: String, label: String, params: String, active: i64, created_at: String }
#[derive(Deserialize)]
struct GIn { id: String, label: String, parent: Option<String>, created_at: String }
#[derive(Deserialize)]
struct GvIn {
    id: String, gate_id: String, version: i64, label: String, parent: Option<String>,
    x_channel: String, y_channel: String, vertices: String, active: i64, created_at: String,
}
#[derive(Deserialize)]
struct RIn {
    id: String, gate_id: String, gate_version: String, parent_run: Option<String>,
    compensation_version: String, transform_version: String, status: String,
    count: Option<i64>, parent_count: Option<i64>, event_ids: String,
    lineage: String, created_at: String,
}

/// 清空数据库并导入 bundle（保留原始运行记录），随后对每条记录独立重算复核。
/// 返回 (导入运行数, 通过数, 失败明细)。
pub fn import_bundle_verify(
    store: &Store,
    bundle_json: &str,
) -> Result<(usize, usize, Vec<String>), EngineError> {
    let b: BundleIn = serde_json::from_str(bundle_json)?;
    store.wipe()?;

    {
        let mut conn = store.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for e in &b.events {
            tx.execute(
                "INSERT INTO events(id,batch,raw_json,sort_index) VALUES(?1,?2,?3,?4)",
                params![e.id, e.batch, e.raw.to_string(), e.sort_index],
            )?;
        }
        for c in &b.compensation_versions {
            tx.execute(
                "INSERT INTO compensation_versions(id,label,channels,matrix_json,active,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![c.id, c.label, c.channels, c.matrix, c.active, c.created_at],
            )?;
        }
        for t in &b.transform_versions {
            tx.execute(
                "INSERT INTO transform_versions(id,label,params_json,active,created_at)
                 VALUES(?1,?2,?3,?4,?5)",
                params![t.id, t.label, t.params, t.active, t.created_at],
            )?;
        }
        for g in &b.gates {
            tx.execute(
                "INSERT INTO gates(id,label,parent,created_at) VALUES(?1,?2,?3,?4)",
                params![g.id, g.label, g.parent, g.created_at],
            )?;
        }
        for gv in &b.gate_versions {
            tx.execute(
                "INSERT INTO gate_versions(id,gate_id,version,label,parent,x_channel,y_channel,
                     vertices_json,active,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![gv.id, gv.gate_id, gv.version, gv.label, gv.parent, gv.x_channel,
                        gv.y_channel, gv.vertices, gv.active, gv.created_at],
            )?;
        }
        for r in &b.runs {
            tx.execute(
                "INSERT INTO runs(id,gate_id,gate_version,parent_run,compensation_version,
                     transform_version,status,count,parent_count,event_ids_json,lineage,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![r.id, r.gate_id, r.gate_version, r.parent_run, r.compensation_version,
                        r.transform_version, r.status, r.count, r.parent_count, r.event_ids,
                        r.lineage, r.created_at],
            )?;
        }
        tx.commit()?;
    }
    verify_runs(store)
}

/// 对每条 active 运行按其记录的定义/补偿/变换快照独立重算，与记录计数核对。
pub fn verify_runs(store: &Store) -> Result<(usize, usize, Vec<String>), EngineError> {
    // 事件在锁外读取，避免与下方连接锁形成重入
    let all_events = store.load_events()?;
    let conn = store.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id,gate_version,compensation_version,transform_version,status,event_ids_json
         FROM runs ORDER BY created_at,id")?;
    let targets: Vec<(String, String, String, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let channels = fixture::data_channels();
    let mut failures = Vec::new();
    let mut checked = 0usize;
    let mut passed = 0usize;

    for (run_id, gv_db, comp_id, trans_id, status, ids_json) in targets {
        let recorded_ids: BTreeSet<String> = serde_json::from_str(&ids_json)?;
        if status != "active" {
            // 历史/失效运行保留事件身份用于审计，但不得再携带有效计数字段。
            let (cnt, pcnt): (Option<i64>, Option<i64>) = conn.query_row(
                "SELECT count,parent_count FROM runs WHERE id=?1",
                params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            if status == "invalidated" && (cnt.is_some() || pcnt.is_some()) {
                failures.push(format!("{run_id}: invalidated run must not expose counts"));
            }
            let _ = (gv_db, comp_id, trans_id, recorded_ids);
            continue;
        }
        checked += 1;

        // 取快照定义
        let gv = gate_version_by_id(&conn, &gv_db)?;
        let (cj, mj): (String, String) = conn.query_row(
            "SELECT channels,matrix_json FROM compensation_versions WHERE id=?1",
            params![comp_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let tj: String = conn.query_row(
            "SELECT params_json FROM transform_versions WHERE id=?1",
            params![trans_id], |r| r.get(0))?;
        let comp = parse_compensation(&cj, &mj)?;
        comp.validate(&channels)?;
        let transforms: BTreeMap<String, crate::domain::TransformParams> =
            serde_json::from_str(&tj)?;

        // 沿 lineage 过滤祖先成员
        let parent_lineage: Option<String> = conn.query_row(
            "SELECT parent_run FROM runs WHERE id=?1", params![run_id], |r| r.get(0))?;
        let mut eligible: Option<BTreeSet<String>> = None;
        if let Some(pid) = parent_lineage {
            let pj: String = conn.query_row(
                "SELECT event_ids_json FROM runs WHERE id=?1", params![pid], |r| r.get(0))?;
            eligible = Some(serde_json::from_str(&pj)?);
        }

        let poly = polygon_of(&gv)?;
        let mut recomputed: BTreeSet<String> = BTreeSet::new();
        for e in &all_events {
            if let Some(set) = &eligible {
                if !set.contains(&e.0) {
                    continue;
                }
            }
            let map: BTreeMap<String, f64> = serde_json::from_str(&e.2)?;
            let ordered: Vec<f64> =
                channels.iter().map(|c| map.get(c).copied().unwrap_or(0.0)).collect();
            let cv = comp.apply(&channels, &ordered)?;
            let mut vals = BTreeMap::new();
            for (ch, v) in channels.iter().zip(cv) {
                vals.insert(ch.clone(), transforms.get(ch).cloned().unwrap_or_default().apply(v));
            }
            let p = Point::new(
                *vals.get(&gv.x_channel).unwrap_or(&f64::NAN),
                *vals.get(&gv.y_channel).unwrap_or(&f64::NAN),
            );
            if geometry::contains(&poly, p) {
                recomputed.insert(e.0.clone());
            }
        }

        if recomputed == recorded_ids {
            passed += 1;
            conn.execute(
                "INSERT OR REPLACE INTO run_verify(run_id,ok,expected_count,detail)
                 VALUES(?1,1,?2,'replay matches recorded run')",
                params![run_id, recomputed.len() as i64],
            )?;
        } else {
            let detail = format!(
                "{run_id}: recorded={} recomputed={} only_recorded={} only_recomputed={}",
                recorded_ids.len(),
                recomputed.len(),
                recorded_ids.difference(&recomputed).count(),
                recomputed.difference(&recorded_ids).count()
            );
            conn.execute(
                "INSERT OR REPLACE INTO run_verify(run_id,ok,expected_count,detail)
                 VALUES(?1,0,?2,?3)",
                params![run_id, recomputed.len() as i64, detail],
            )?;
            failures.push(detail);
        }
    }
    Ok((checked, passed, failures))
}
