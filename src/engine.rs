//! 版本化重放引擎。
//!
//! 口径约定见 README：
//! * 原始强度按运行记录的补偿矩阵版本乘算，矩阵按通道名与数据通道唯一对应；
//!   缺通道/重复通道/未知通道直接报错，绝不按位置继续乘。
//! * 之后按运行记录的变换版本逐通道变换；多边形门在变换后的坐标空间判定。
//! * 点-多边形使用 geometry::contains 的统一半开规则，相邻门共享边不双计。
//! * 父群为空时父子百分比为 null（不可定义），绝不写成 0。
//! * 修改父门/补偿/变换后，受影响子树的旧运行转为 invalid（计数保留但明确失效），
//!   系统重新生成新运行。

use crate::db::Db;
use crate::geometry::{self, Vertex};
use crate::model::{
    CompensationDef, Fixture, GateVersionDef, PopulationDef, RawEvent, TransformDef, TransformKind,
    CHANNELS,
};
use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug)]
pub enum EngineError {
    Db(String),
    Bad(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Db(s) => write!(f, "数据库错误: {s}"),
            EngineError::Bad(s) => write!(f, "{s}"),
        }
    }
}
impl std::error::Error for EngineError {}
impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        EngineError::Db(e.to_string())
    }
}
impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError::Db(format!("序列化错误: {e}"))
    }
}

pub type EngineResult<T> = Result<T, EngineError>;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::Bad(msg.into())
}

// ---------- 数据读取 ----------

pub fn load_events(conn: &Connection) -> EngineResult<Vec<RawEvent>> {
    let mut stmt =
        conn.prepare("SELECT id, batch_id, v0, v1, v2, v3 FROM events ORDER BY rowid")?;
    let mapped = stmt.query_map([], |r| {
        Ok(RawEvent {
            id: r.get(0)?,
            batch_id: r.get(1)?,
            values: vec![r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
        })
    })?;
    let mut out = Vec::new();
    for r in mapped {
        out.push(r?);
    }
    Ok(out)
}

pub fn load_comp(conn: &Connection, id: &str) -> EngineResult<CompensationDef> {
    let spec: String = conn.query_row(
        "SELECT spec FROM compensations WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&spec)?)
}

pub fn load_transform(conn: &Connection, id: &str) -> EngineResult<TransformDef> {
    let spec: String = conn.query_row(
        "SELECT spec FROM transforms WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&spec)?)
}

pub fn load_populations(conn: &Connection) -> EngineResult<Vec<PopulationDef>> {
    let mut stmt = conn.prepare("SELECT id, name, parent_id FROM populations ORDER BY id")?;
    let pops = stmt.query_map([], |r| {
        Ok(PopRow {
            id: r.get(0)?,
            name: r.get(1)?,
            parent: r.get::<_, Option<String>>(2)?,
        })
    })?;
    let mut rows = Vec::new();
    for r in pops {
        rows.push(r?);
    }
    let mut out = Vec::new();
    for row in rows {
        let gate = active_gate(conn, &row.id)?;
        out.push(PopulationDef {
            id: row.id,
            name: row.name,
            parent: row.parent,
            gate,
        });
    }
    Ok(out)
}

struct PopRow {
    id: String,
    name: String,
    parent: Option<String>,
}

pub fn active_gate(conn: &Connection, pop_id: &str) -> EngineResult<Option<GateVersionDef>> {
    let spec: Option<String> = conn
        .query_row(
            "SELECT spec FROM gate_versions WHERE population_id = ?1 AND active = 1",
            params![pop_id],
            |r| r.get(0),
        )
        .ok();
    match spec {
        Some(s) => Ok(Some(serde_json::from_str(&s)?)),
        None => Ok(None),
    }
}

// ---------- 补偿与变换 ----------

/// 按通道名应用补偿。通道集合必须与数据通道一一对应，否则拒绝。
pub fn apply_compensation(
    events: &[RawEvent],
    comp: &CompensationDef,
) -> EngineResult<Vec<[f64; 4]>> {
    let data_channels: Vec<&str> = CHANNELS.to_vec();
    let mut in_index: HashMap<&str, usize> = HashMap::new();
    for c in &comp.channels {
        if in_index.contains_key(c.as_str()) {
            return Err(bad(format!(
                "补偿矩阵 {} 存在重复通道 {c}，无法唯一对应",
                comp.id
            )));
        }
        in_index.insert(c.as_str(), in_index.len());
    }
    for dc in &data_channels {
        if !in_index.contains_key(*dc) {
            return Err(bad(format!(
                "补偿矩阵 {} 缺少数据通道 {dc}，禁止按位置继续乘法",
                comp.id
            )));
        }
    }
    if comp.channels.len() != data_channels.len() {
        return Err(bad(format!(
            "补偿矩阵 {} 通道数 {} 与数据通道数 {} 不一致",
            comp.id,
            comp.channels.len(),
            data_channels.len()
        )));
    }
    if comp.rows.len() != data_channels.len() {
        return Err(bad(format!("补偿矩阵 {} 行数与通道数不符", comp.id)));
    }
    let mut matrix = [[0.0f64; 4]; 4];
    for (out_idx, out_name) in data_channels.iter().enumerate() {
        let row_pos = in_index
            .get(out_name)
            .ok_or_else(|| bad(format!("补偿矩阵 {} 缺少输出通道 {out_name}", comp.id)))?;
        let row = &comp.rows[*row_pos];
        if row.len() != data_channels.len() {
            return Err(bad(format!("补偿矩阵 {} 行 {out_name} 列数不符", comp.id)));
        }
        for (in_idx, in_name) in data_channels.iter().enumerate() {
            let col_pos = in_index[in_name];
            matrix[out_idx][in_idx] = row[col_pos];
        }
    }

    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let mut v = [0.0; 4];
        for i in 0..4 {
            v[i] = (0..4).map(|j| matrix[i][j] * e.values[j]).sum();
        }
        out.push(v);
    }
    Ok(out)
}

fn apply_one(kind: TransformKind, v: f64) -> f64 {
    match kind {
        TransformKind::Linear => v,
        // log10(1+x)，再线性映射回 0..=1000 刻度，使多边形坐标仍在同一刻度上。
        TransformKind::Log => {
            let x = v.max(0.0);
            (1.0 + x).log10() / (1001.0f64).log10() * 1000.0
        }
    }
}

pub fn apply_transform(values: &[[f64; 4]], tr: &TransformDef) -> EngineResult<Vec<[f64; 4]>> {
    if tr.channels.len() != CHANNELS.len() {
        return Err(bad(format!("变换版本 {} 通道数不符", tr.id)));
    }
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        let mut t = [0.0; 4];
        for (i, ct) in tr.channels.iter().enumerate() {
            t[i] = apply_one(ct.kind, v[i]);
        }
        out.push(t);
    }
    Ok(out)
}

pub fn channel_index(name: &str) -> EngineResult<usize> {
    CHANNELS
        .iter()
        .position(|c| *c == name)
        .ok_or_else(|| bad(format!("未知通道 {name}")))
}

// ---------- 运行评估 ----------

pub struct EvaluatedGate<'a> {
    pub spec: &'a GateVersionDef,
    pub x_idx: usize,
    pub y_idx: usize,
    pub polygon: Vec<Vertex>,
}

pub fn prepare_gate(g: &GateVersionDef) -> EngineResult<(usize, usize, Vec<Vertex>)> {
    let x_idx = channel_index(&g.polygon.x_channel)?;
    let y_idx = channel_index(&g.polygon.y_channel)?;
    let poly = geometry::normalize_ccw(&g.polygon.vertices)
        .ok_or_else(|| bad(format!("门 {} 多边形退化（顶点不足或零面积）", g.id)))?;
    Ok((x_idx, y_idx, poly))
}

pub struct RunOutcome {
    pub ids: HashSet<String>,
}

/// 对单个群体求值；返回命中事件身份集合。
/// parent_ids 为 None 表示根群体。
fn evaluate_pop(
    events: &[RawEvent],
    transformed: &[[f64; 4]],
    gate: Option<&GateVersionDef>,
    parent_ids: Option<&HashSet<String>>,
) -> EngineResult<RunOutcome> {
    let prepared = match gate {
        Some(g) => Some(prepare_gate(g)?),
        None => None,
    };
    let mut ids = HashSet::new();
    for (i, e) in events.iter().enumerate() {
        if let Some(parent) = parent_ids {
            if !parent.contains(&e.id) {
                continue;
            }
        }
        if let Some((x_idx, y_idx, poly)) = &prepared {
            let p = Vertex {
                x: transformed[i][*x_idx],
                y: transformed[i][*y_idx],
            };
            if !geometry::contains(poly, p) {
                continue;
            }
        }
        ids.insert(e.id.clone());
    }
    Ok(RunOutcome { ids })
}

// ---------- 运行写入与失效传播 ----------

fn next_seq(conn: &Connection) -> EngineResult<i64> {
    let v: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(CAST(substr(id, 2) AS INTEGER)), 0) FROM runs",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(v + 1)
}

fn invalidate_subtree(
    tx: &Transaction,
    pops: &[PopulationDef],
    changed: &HashSet<String>,
) -> EngineResult<()> {
    // changed 群体自身生成新运行；其所有后代的旧 current 运行标记 invalid。
    let children: HashMap<&str, Vec<&str>> = {
        let mut m: HashMap<&str, Vec<&str>> = HashMap::new();
        for p in pops {
            if let Some(par) = &p.parent {
                m.entry(par.as_str()).or_default().push(p.id.as_str());
            }
        }
        m
    };
    let mut affected: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = changed.iter().cloned().collect();
    while let Some(id) = stack.pop() {
        if affected.contains(&id) {
            continue;
        }
        affected.insert(id.clone());
        if let Some(kids) = children.get(id.as_str()) {
            for k in kids {
                stack.push((*k).to_string());
            }
        }
    }
    // 被修改节点自身的旧运行同样失效（其引用的门版本已停用），
    // 随后整棵受影响子树都会由重放生成新运行。
    for id in &affected {
        if let Some(cur) = current_run_id(tx, id)? {
            tx.execute(
                "UPDATE runs SET status = 'invalid' WHERE id = ?1 AND status != 'invalid'",
                params![cur],
            )?;
            tx.execute(
                "DELETE FROM current_runs WHERE population_id = ?1",
                params![id],
            )?;
        }
    }
    Ok(())
}

fn current_run_id(tx: &Transaction, pop_id: &str) -> EngineResult<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT run_id FROM current_runs WHERE population_id = ?1",
            params![pop_id],
            |r| r.get::<_, String>(0),
        )
        .ok())
}

fn subtree_set(pops: &[PopulationDef], root: &str) -> EngineResult<HashSet<String>> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for p in pops {
        if let Some(par) = &p.parent {
            children
                .entry(par.as_str())
                .or_default()
                .push(p.id.as_str());
        }
    }
    let mut set = HashSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(id) = stack.pop() {
        if set.insert(id.clone()) {
            if let Some(kids) = children.get(id.as_str()) {
                for k in kids {
                    stack.push((*k).to_string());
                }
            }
        }
    }
    Ok(set)
}

#[derive(Clone)]
pub struct ReplayOptions {
    pub comp_id: String,
    pub transform_id: String,
    pub note: String,
}

/// 重算：scope 为 None 时从根重算整棵树；scope 为 Some 时仅重算集合内群体，
/// 集合外群体复用其当前运行的成员集合与版本（用于修正单个门时保留无关分支原计数）。
fn compute_runs(
    tx: &Transaction,
    comp_id: &str,
    transform_id: &str,
    note: &str,
    scope: Option<&HashSet<String>>,
) -> EngineResult<BTreeMap<String, (String, RunOutcome)>> {
    let comp = load_comp(tx, comp_id)?;
    let tr = load_transform(tx, transform_id)?;
    let events = load_events(tx)?;
    let pops = {
        let mut v = load_populations(tx)?;
        v.sort_by_key(|p| p.id.clone());
        v
    };
    let order = topological_order(&pops)?;
    let comp_values = apply_compensation(&events, &comp)?;
    let transformed = apply_transform(&comp_values, &tr)?;

    // 既有当前运行（scope 外复用）。
    let mut previous: HashMap<String, (String, RunOutcome)> = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT c.population_id, c.run_id FROM current_runs c JOIN runs r ON r.id=c.run_id
             WHERE r.status='ok'",
        )?;
        let mapped =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in mapped {
            let (pop_id, run_id) = row?;
            let ids = run_event_ids(tx, &run_id)?;
            previous.insert(pop_id, (run_id, RunOutcome { ids }));
        }
    }

    let mut results: BTreeMap<String, (String, RunOutcome)> = BTreeMap::new();
    let mut new_ids: HashSet<String> = HashSet::new();
    let mut seq = next_seq(tx)?;
    let now = unix_ms();
    for pop_id in order {
        let in_scope = scope.map(|sc| sc.contains(&pop_id)).unwrap_or(true);
        if !in_scope {
            // 复用未受影响分支的既有运行，原样保留门系和计数。
            if let Some(existing) = previous.remove(&pop_id) {
                results.insert(pop_id.clone(), existing);
                continue;
            }
            // 父链无现成运行（理论上不该发生）：仍需重算。
        }
        let pop = pops.iter().find(|p| p.id == pop_id).unwrap();
        let run_id = format!("R{seq:05}");
        seq += 1;
        let parent_ids = pop
            .parent
            .as_ref()
            .map(|pid| &results.get(pid).expect("父运行必须先计算").1.ids);
        let outcome = evaluate_pop(&events, &transformed, pop.gate.as_ref(), parent_ids)?;
        new_ids.insert(run_id.clone());
        results.insert(pop_id.clone(), (run_id, outcome));
    }

    // 仅落库 scope 内的新运行；scope 外不产生运行、不改 current。
    for (pop_id, (run_id, outcome)) in &results {
        let in_scope = scope.map(|sc| sc.contains(pop_id)).unwrap_or(true);
        if !in_scope {
            continue;
        }
        let pop = pops.iter().find(|p| p.id == *pop_id).unwrap();
        if !new_ids.contains(run_id) {
            continue;
        }
        let gate_id = pop.gate.as_ref().map(|g| g.id.clone());
        let parent_count = pop
            .parent
            .as_ref()
            .map(|pid| results.get(pid).unwrap().1.ids.len() as i64);
        let n = outcome.ids.len() as i64;
        tx.execute(
            "INSERT INTO runs(id, population_id, gate_version_id, comp_id, transform_id,
             status, event_count, parent_count, error, created_at, note)
             VALUES(?1,?2,?3,?4,?5,'ok',?6,?7,NULL,?8,?9)",
            params![
                run_id,
                pop_id,
                gate_id,
                comp_id,
                transform_id,
                n,
                parent_count,
                now,
                note
            ],
        )?;
        tx.execute(
            "DELETE FROM current_runs WHERE population_id = ?1",
            params![pop_id],
        )?;
        tx.execute(
            "INSERT INTO current_runs(population_id, run_id) VALUES(?1, ?2)",
            params![pop_id, run_id],
        )?;
        tx.execute("DELETE FROM run_members WHERE run_id = ?1", params![run_id])?;
        let mut ids: Vec<&String> = outcome.ids.iter().collect();
        ids.sort();
        for (ord, eid) in ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO run_members(run_id, event_id, ord) VALUES(?1,?2,?3)",
                params![run_id, eid, ord as i64],
            )?;
        }
    }
    Ok(results)
}

/// 从根开始重算整棵树。
fn compute_all_runs(
    tx: &Transaction,
    comp_id: &str,
    transform_id: &str,
    note: &str,
) -> EngineResult<BTreeMap<String, (String, RunOutcome)>> {
    compute_runs(tx, comp_id, transform_id, note, None)
}

fn topological_order(pops: &[PopulationDef]) -> EngineResult<Vec<String>> {
    let mut indeg: HashMap<&str, usize> = pops.iter().map(|p| (p.id.as_str(), 0usize)).collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for p in pops {
        if let Some(par) = &p.parent {
            if !indeg.contains_key(par.as_str()) {
                return Err(bad(format!("群体 {} 的父群体 {par} 不存在", p.id)));
            }
            *indeg.get_mut(p.id.as_str()).unwrap() += 1;
            children
                .entry(par.as_str())
                .or_default()
                .push(p.id.as_str());
        }
    }
    let mut queue: Vec<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();
    queue.sort();
    let mut order = Vec::new();
    while let Some(id) = queue.pop() {
        order.push(id.to_string());
        if let Some(kids) = children.get(id) {
            for k in kids {
                let d = indeg.get_mut(k).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push(k);
                }
            }
        }
        queue.sort();
    }
    if order.len() != pops.len() {
        return Err(bad("门系树存在环"));
    }
    Ok(order)
}

fn validate_matrix_choice(conn: &Connection, comp_id: &str) -> EngineResult<()> {
    // 仅验证补偿矩阵：缺通道时必须报错且不产生任何新运行。
    let comp = load_comp(conn, comp_id)?;
    let events = load_events(conn)?;
    apply_compensation(&events, &comp)?;
    let _ = load_transform(
        conn,
        &conn.query_row(
            "SELECT value FROM meta WHERE key='current_transform'",
            [],
            |r| r.get::<_, String>(0),
        )?,
    )?;
    Ok(())
}

// ---------- 对外操作 ----------

impl Db {
    /// 清空并重新播种 fixture，再从根重放。返回新运行摘要。
    pub fn reseed(&self, fx: &Fixture) -> EngineResult<RunSummary> {
        let mut conn = self.0.lock().unwrap();
        crate::db::replace_with_fixture(&mut conn, fx)?;
        {
            let tx = conn.transaction()?;
            let summary = commit_recompute(
                &tx,
                &fx.current_comp_id,
                &fx.current_transform_id,
                "固定 fixture 首次重放",
            )?;
            tx.commit()?;
            Ok(summary)
        }
    }

    /// 修正多边形门：生成新门版本，旧运行失效，受影响子树重新生成新运行。
    pub fn edit_gate(
        &self,
        pop_id: &str,
        note: &str,
        polygon: crate::model::GatePolygon,
    ) -> EngineResult<RunSummary> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        if pop_id == "ALL" {
            return Err(bad("根群体没有门，不能编辑"));
        }
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM populations WHERE id = ?1",
            params![pop_id],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Err(bad(format!("群体 {pop_id} 不存在")));
        }
        // 先校验几何，避免写入坏版本。
        let candidate = GateVersionDef {
            id: "candidate".into(),
            note: note.into(),
            polygon: polygon.clone(),
        };
        prepare_gate(&candidate)?;

        let seq: i64 = tx
            .query_row(
                "SELECT COUNT(*) + 1 FROM gate_versions WHERE population_id = ?1",
                params![pop_id],
                |r| r.get(0),
            )
            .unwrap_or(1);
        let new_id = format!("G_{pop_id}_{seq}");
        let gate = GateVersionDef {
            id: new_id.clone(),
            note: note.into(),
            polygon,
        };
        tx.execute(
            "UPDATE gate_versions SET active = 0 WHERE population_id = ?1",
            params![pop_id],
        )?;
        tx.execute(
            "INSERT INTO gate_versions(id, population_id, note, spec, created_at, active)
             VALUES(?1,?2,?3,?4,?5,1)",
            params![
                new_id,
                pop_id,
                note,
                serde_json::to_string(&gate).unwrap(),
                unix_ms(),
            ],
        )?;
        let pops = load_populations(&tx)?;
        let scope = subtree_set(&pops, pop_id)?;
        invalidate_subtree(&tx, &pops, &HashSet::from([pop_id.to_string()]))?;
        let comp_id = meta(&tx, "current_comp")?;
        let tr_id = meta(&tx, "current_transform")?;
        let _ = compute_runs(
            &tx,
            &comp_id,
            &tr_id,
            &format!("修正群体 {pop_id} 的多边形门"),
            Some(&scope),
        )?;
        set_meta(&tx, "current_comp", &comp_id)?;
        set_meta(&tx, "current_transform", &tr_id)?;
        let summary = run_summary(&tx)?;
        tx.commit()?;
        Ok(summary)
    }

    /// 切换补偿矩阵版本：缺通道直接拒绝，不产生运行；否则整树重放。
    pub fn replay_comp(&self, comp_id: &str) -> EngineResult<RunSummary> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        validate_matrix_choice(&tx, comp_id)?;
        invalidate_all(&tx)?;
        set_meta(&tx, "current_comp", comp_id)?;
        let tr_id = meta(&tx, "current_transform")?;
        let summary = commit_recompute(&tx, comp_id, &tr_id, &format!("切换补偿矩阵至 {comp_id}"))?;
        tx.commit()?;
        Ok(summary)
    }

    /// 切换变换版本：整树重放。
    pub fn replay_transform(&self, transform_id: &str) -> EngineResult<RunSummary> {
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM transforms WHERE id = ?1",
            params![transform_id],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Err(bad(format!("变换版本 {transform_id} 不存在")));
        }
        invalidate_all(&tx)?;
        set_meta(&tx, "current_transform", transform_id)?;
        let comp_id = meta(&tx, "current_comp")?;
        let summary = commit_recompute(
            &tx,
            &comp_id,
            transform_id,
            &format!("切换变换至 {transform_id}"),
        )?;
        tx.commit()?;
        Ok(summary)
    }
}

fn commit_recompute(
    tx: &Transaction,
    comp_id: &str,
    transform_id: &str,
    note: &str,
) -> EngineResult<RunSummary> {
    let _ = compute_all_runs(tx, comp_id, transform_id, note)?;
    set_meta(tx, "current_comp", comp_id)?;
    set_meta(tx, "current_transform", transform_id)?;
    run_summary(tx)
}

fn invalidate_all(tx: &Transaction) -> EngineResult<()> {
    // 全树使用旧版本计算的运行全部明确失效。
    let mut ids: Vec<String> = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT run_id FROM current_runs")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for r in rows {
            ids.push(r?);
        }
    }
    for id in ids {
        tx.execute(
            "UPDATE runs SET status='invalid' WHERE id=?1 AND status!='invalid'",
            params![id],
        )?;
    }
    tx.execute("DELETE FROM current_runs", [])?;
    Ok(())
}

fn meta(conn: &Connection, key: &str) -> EngineResult<String> {
    conn.query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| {
        r.get::<_, String>(0)
    })
    .map_err(|_| bad(format!("缺少元数据 {key}")))
}

fn set_meta(tx: &Transaction, key: &str, value: &str) -> EngineResult<()> {
    tx.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- 状态摘要 ----------

#[derive(Serialize)]
pub struct RunSummary {
    pub runs: Vec<CurrentRunDto>,
}

#[derive(Serialize, Clone)]
pub struct CurrentRunDto {
    pub population_id: String,
    pub run_id: String,
    pub gate_version_id: Option<String>,
    pub comp_id: String,
    pub transform_id: String,
    pub status: String,
    pub event_count: Option<i64>,
    pub parent_count: Option<i64>,
    pub percent: Option<f64>,
    pub note: String,
}

fn run_summary(conn: &Connection) -> EngineResult<RunSummary> {
    let mut stmt = conn.prepare(
        "SELECT c.population_id, r.id, r.gate_version_id, r.comp_id, r.transform_id,
                r.status, r.event_count, r.parent_count, r.note
         FROM current_runs c JOIN runs r ON r.id = c.run_id
         ORDER BY c.population_id",
    )?;
    let rows = stmt.query_map([], |r| {
        let event_count: Option<i64> = r.get(6)?;
        let parent_count: Option<i64> = r.get(7)?;
        let percent = match (event_count, parent_count) {
            (Some(n), Some(p)) if p > 0 => Some(n as f64 * 100.0 / p as f64),
            _ => None,
        };
        Ok(CurrentRunDto {
            population_id: r.get(0)?,
            run_id: r.get(1)?,
            gate_version_id: r.get(2)?,
            comp_id: r.get(3)?,
            transform_id: r.get(4)?,
            status: r.get::<_, String>(5)?,
            event_count,
            parent_count,
            percent,
            note: r.get(8)?,
        })
    })?;
    let mut runs = Vec::new();
    for r in rows {
        runs.push(r?);
    }
    Ok(RunSummary { runs })
}

// ---------- 全量状态（页面） ----------

#[derive(Serialize)]
pub struct StateDto {
    pub fixture_version: String,
    pub current_comp_id: String,
    pub current_transform_id: String,
    pub channels: Vec<crate::model::ChannelDef>,
    pub batches: Vec<crate::model::BatchDef>,
    pub transforms: Vec<TransformDef>,
    pub compensations: Vec<CompensationDef>,
    pub populations: Vec<PopDto>,
    pub runs: Vec<CurrentRunDto>,
}

#[derive(Serialize)]
pub struct GateVersionDto {
    pub id: String,
    pub note: String,
    pub active: bool,
    pub created_at: i64,
    pub polygon: crate::model::GatePolygon,
}

#[derive(Serialize)]
pub struct PopDto {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub active_gate: Option<crate::model::GatePolygon>,
    pub active_gate_id: Option<String>,
    pub gate_versions: Vec<GateVersionDto>,
}

impl Db {
    pub fn state(&self) -> EngineResult<StateDto> {
        let conn = self.0.lock().unwrap();
        let fixture_version = conn.query_row(
            "SELECT value FROM meta WHERE key='fixture_version'",
            [],
            |r| r.get::<_, String>(0),
        )?;
        let current_comp_id = meta(&conn, "current_comp")?;
        let current_transform_id = meta(&conn, "current_transform")?;

        let channels: Vec<_> = {
            let mut stmt = conn.prepare("SELECT name,label,kind FROM channels ORDER BY ord")?;
            let mapped = stmt.query_map([], |r| {
                Ok(crate::model::ChannelDef {
                    name: r.get(0)?,
                    label: r.get(1)?,
                    kind: r.get(2)?,
                })
            })?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r?);
            }
            out
        };
        let batches: Vec<_> = {
            let mut stmt =
                conn.prepare("SELECT id,instrument,acquired_at FROM batches ORDER BY id")?;
            let mapped = stmt.query_map([], |r| {
                Ok(crate::model::BatchDef {
                    id: r.get(0)?,
                    instrument: r.get(1)?,
                    acquired_at: r.get(2)?,
                })
            })?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r?);
            }
            out
        };
        let mut transforms = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT spec FROM transforms ORDER BY id")?;
            for spec in stmt.query_map([], |r| r.get::<_, String>(0))? {
                transforms.push(serde_json::from_str::<TransformDef>(&spec?)?);
            }
        }
        let mut compensations = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT spec FROM compensations ORDER BY id")?;
            for spec in stmt.query_map([], |r| r.get::<_, String>(0))? {
                compensations.push(serde_json::from_str::<CompensationDef>(&spec?)?);
            }
        }
        let mut populations = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT id,name,parent_id FROM populations ORDER BY id")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?;
            for row in rows {
                let (id, name, parent_id) = row?;
                let mut gate_versions = Vec::new();
                {
                    let mut gstmt = conn.prepare(
                        "SELECT spec,active,created_at FROM gate_versions
                         WHERE population_id=?1 ORDER BY created_at, id",
                    )?;
                    let grows = gstmt.query_map(params![id], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    })?;
                    for grow in grows {
                        let (spec, active, created_at) = grow?;
                        let gv = serde_json::from_str::<GateVersionDef>(&spec)?;
                        gate_versions.push(GateVersionDto {
                            id: gv.id,
                            note: gv.note,
                            active: active != 0,
                            created_at,
                            polygon: gv.polygon,
                        });
                    }
                }
                let active_gate = active_gate(&conn, &id)?;
                populations.push(PopDto {
                    active_gate_id: active_gate.as_ref().map(|g| g.id.clone()),
                    active_gate: active_gate.map(|g| g.polygon),
                    id,
                    name,
                    parent_id,
                    gate_versions,
                });
            }
        }
        let runs = run_summary(&conn)?.runs;
        Ok(StateDto {
            fixture_version,
            current_comp_id,
            current_transform_id,
            channels,
            batches,
            transforms,
            compensations,
            populations,
            runs,
        })
    }
}

// ---------- 绘图与成员 ----------

#[derive(Serialize)]
pub struct PointDto {
    pub id: String,
    pub batch_id: String,
    pub x: f64,
    pub y: f64,
    pub in_run: bool,
}

#[derive(Serialize)]
pub struct ScatterDto {
    pub run_id: Option<String>,
    pub x_channel: String,
    pub y_channel: String,
    pub space: String,
    pub points: Vec<PointDto>,
}

impl Db {
    /// 散点：在指定运行当前的补偿+变换空间投影；run 为 invalid 时仍可显式查看。
    pub fn scatter(
        &self,
        run_id: Option<&str>,
        x_channel: &str,
        y_channel: &str,
        limit: usize,
    ) -> EngineResult<ScatterDto> {
        let conn = self.0.lock().unwrap();
        let events = load_events(&conn)?;
        let mut member_ids: Option<HashSet<String>> = None;
        let mut resolved_run: Option<String> = None;
        let (comp_id, tr_id) = if let Some(rid) = run_id {
            let row: rusqlite::Result<(String, String)> = conn.query_row(
                "SELECT comp_id, transform_id FROM runs WHERE id=?1",
                params![rid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            );
            let (c, t) = row.map_err(|_| bad(format!("运行 {rid} 不存在")))?;
            let mut stmt = conn.prepare("SELECT event_id FROM run_members WHERE run_id=?1")?;
            let ids = stmt
                .query_map(params![rid], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            member_ids = Some(ids);
            resolved_run = Some(rid.to_string());
            (c, t)
        } else {
            (
                meta(&conn, "current_comp")?,
                meta(&conn, "current_transform")?,
            )
        };
        let comp = load_comp(&conn, &comp_id)?;
        let tr = load_transform(&conn, &tr_id)?;
        let cv = apply_compensation(&events, &comp)?;
        let tv = apply_transform(&cv, &tr)?;
        let xi = channel_index(x_channel)?;
        let yi = channel_index(y_channel)?;
        let mut points = Vec::new();
        for (i, e) in events.iter().enumerate().take(limit) {
            points.push(PointDto {
                id: e.id.clone(),
                batch_id: e.batch_id.clone(),
                x: tv[i][xi],
                y: tv[i][yi],
                in_run: member_ids.as_ref().is_none_or(|m| m.contains(&e.id)),
            });
        }
        Ok(ScatterDto {
            run_id: resolved_run,
            x_channel: x_channel.into(),
            y_channel: y_channel.into(),
            space: format!("补偿 {} + 变换 {}", comp_id, tr_id),
            points,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn histogram(
        &self,
        run_id: Option<&str>,
        parent_run_id: Option<&str>,
        channel: &str,
        bins: usize,
    ) -> EngineResult<HistogramDto> {
        let conn = self.0.lock().unwrap();
        let events = load_events(&conn)?;
        let mut scope_ids: Option<HashSet<String>> = None;
        let mut member_ids: Option<HashSet<String>> = None;
        let (comp_id, tr_id) = if let Some(rid) = run_id {
            let (c, t) = run_versions(&conn, rid)?;
            scope_ids = Some(run_event_ids(&conn, parent_run_id.unwrap_or(rid))?);
            member_ids = Some(run_event_ids(&conn, rid)?);
            (c, t)
        } else {
            (
                meta(&conn, "current_comp")?,
                meta(&conn, "current_transform")?,
            )
        };
        let comp = load_comp(&conn, &comp_id)?;
        let tr = load_transform(&conn, &tr_id)?;
        let cv = apply_compensation(&events, &comp)?;
        let tv = apply_transform(&cv, &tr)?;
        let ci = channel_index(channel)?;
        let bins = bins.clamp(5, 100);
        let (lo, hi) = (0.0, 1000.0);
        let width = (hi - lo) / bins as f64;
        let mut total = vec![0u64; bins];
        let mut selected = vec![0u64; bins];
        let mut scope_n = 0u64;
        for (i, e) in events.iter().enumerate() {
            if let Some(scope) = &scope_ids {
                if !scope.contains(&e.id) {
                    continue;
                }
            }
            scope_n += 1;
            let v = tv[i][ci].clamp(lo, hi - 1e-9);
            let b = (((v - lo) / width) as usize).min(bins - 1);
            total[b] += 1;
            if member_ids.as_ref().is_none_or(|m| m.contains(&e.id)) {
                selected[b] += 1;
            }
        }
        Ok(HistogramDto {
            run_id: run_id.map(|s| s.to_string()),
            parent_run_id: parent_run_id.map(|s| s.to_string()),
            channel: channel.into(),
            space: format!("补偿 {} + 变换 {}", comp_id, tr_id),
            bins,
            bin_width: width,
            scope_count: scope_n,
            total,
            selected,
        })
    }
}

#[derive(Serialize)]
pub struct HistogramDto {
    pub run_id: Option<String>,
    pub parent_run_id: Option<String>,
    pub channel: String,
    pub space: String,
    pub bins: usize,
    pub bin_width: f64,
    pub scope_count: u64,
    pub total: Vec<u64>,
    pub selected: Vec<u64>,
}

fn run_versions(conn: &Connection, run_id: &str) -> EngineResult<(String, String)> {
    conn.query_row(
        "SELECT comp_id, transform_id FROM runs WHERE id=?1",
        params![run_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(|_| bad(format!("运行 {run_id} 不存在")))
}

fn run_event_ids(conn: &Connection, run_id: &str) -> EngineResult<HashSet<String>> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM runs WHERE id=?1",
        params![run_id],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Err(bad(format!("运行 {run_id} 不存在")));
    }
    let mut stmt = conn.prepare("SELECT event_id FROM run_members WHERE run_id=?1")?;
    let mapped = stmt
        .query_map(params![run_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(mapped)
}

// ---------- 分支差异（按事件身份） ----------

#[derive(Serialize)]
pub struct RunMetaDto {
    pub id: String,
    pub population_id: String,
    pub gate_version_id: Option<String>,
    pub comp_id: String,
    pub transform_id: String,
    pub status: String,
    pub event_count: Option<i64>,
    pub parent_count: Option<i64>,
    pub note: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct DiffDto {
    pub a: RunMetaDto,
    pub b: RunMetaDto,
    pub entered: Vec<String>,
    pub exited: Vec<String>,
    pub both: Vec<String>,
    pub union_count: usize,
    pub event_details: Vec<DiffEventDto>,
}

#[derive(Serialize)]
pub struct DiffEventDto {
    pub event_id: String,
    pub batch_id: String,
    pub category: String,
}

impl Db {
    /// 列出群体的全部运行（含失效旧运行），供分支比较。
    pub fn runs_of(&self, population_id: &str) -> EngineResult<Vec<RunMetaDto>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, population_id, gate_version_id, comp_id, transform_id,
                    status, event_count, parent_count, note, created_at
             FROM runs WHERE population_id=?1 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![population_id], |r| {
            Ok(RunMetaDto {
                id: r.get(0)?,
                population_id: r.get(1)?,
                gate_version_id: r.get(2)?,
                comp_id: r.get(3)?,
                transform_id: r.get(4)?,
                status: r.get(5)?,
                event_count: r.get(6)?,
                parent_count: r.get(7)?,
                note: r.get(8)?,
                created_at: r.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        if out.is_empty() {
            return Err(bad(format!("群体 {population_id} 没有任何运行")));
        }
        Ok(out)
    }

    pub fn diff(&self, a_id: &str, b_id: &str) -> EngineResult<DiffDto> {
        let conn = self.0.lock().unwrap();
        let a = self::run_meta(&conn, a_id)?;
        let b = self::run_meta(&conn, b_id)?;
        let set_a = run_event_ids(&conn, a_id)?;
        let set_b = run_event_ids(&conn, b_id)?;
        let mut entered: Vec<String> = set_b.difference(&set_a).cloned().collect();
        let mut exited: Vec<String> = set_a.difference(&set_b).cloned().collect();
        let mut both: Vec<String> = set_a.intersection(&set_b).cloned().collect();
        entered.sort();
        exited.sort();
        both.sort();
        let mut batch_of: HashMap<String, String> = HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT id, batch_id FROM events")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for r in rows {
                let (id, b) = r?;
                batch_of.insert(id, b);
            }
        }
        let mut union: Vec<String> = set_a.union(&set_b).cloned().collect();
        union.sort();
        let event_details = union
            .iter()
            .map(|id| {
                let category = if set_b.contains(id) && !set_a.contains(id) {
                    "entered"
                } else if set_a.contains(id) && !set_b.contains(id) {
                    "exited"
                } else {
                    "both"
                };
                DiffEventDto {
                    event_id: id.clone(),
                    batch_id: batch_of.get(id).cloned().unwrap_or_default(),
                    category: category.into(),
                }
            })
            .collect();
        Ok(DiffDto {
            a,
            b,
            entered,
            exited,
            both,
            union_count: set_a.union(&set_b).count(),
            event_details,
        })
    }
}

fn run_meta(conn: &Connection, id: &str) -> EngineResult<RunMetaDto> {
    conn.query_row(
        "SELECT id, population_id, gate_version_id, comp_id, transform_id,
                status, event_count, parent_count, note, created_at
         FROM runs WHERE id=?1",
        params![id],
        |r| {
            Ok(RunMetaDto {
                id: r.get(0)?,
                population_id: r.get(1)?,
                gate_version_id: r.get(2)?,
                comp_id: r.get(3)?,
                transform_id: r.get(4)?,
                status: r.get(5)?,
                event_count: r.get(6)?,
                parent_count: r.get(7)?,
                note: r.get(8)?,
                created_at: r.get(9)?,
            })
        },
    )
    .map_err(|_| bad(format!("运行 {id} 不存在")))
}

// ---------- 导出 / 清空重导复核 ----------

#[derive(Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub fixture: Fixture,
    pub current_comp_id: String,
    pub current_transform_id: String,
    pub runs: Vec<BundleRun>,
    pub exported_at: i64,
}

#[derive(Serialize, Deserialize)]
pub struct BundleRun {
    pub id: String,
    pub population_id: String,
    pub gate_version_id: Option<String>,
    pub comp_id: String,
    pub transform_id: String,
    pub status: String,
    pub event_count: Option<i64>,
    pub parent_count: Option<i64>,
    pub note: String,
    pub members: Vec<String>,
}

impl Db {
    /// 导出运行记录：完整 fixture 快照 + 每次运行的版本引用与事件身份成员集合。
    pub fn export_bundle(&self) -> EngineResult<Bundle> {
        let conn = self.0.lock().unwrap();
        let fx = fixture_snapshot(&conn)?;
        let current_comp_id = meta(&conn, "current_comp")?;
        let current_transform_id = meta(&conn, "current_transform")?;
        let mut runs = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id, population_id, gate_version_id, comp_id, transform_id,
                        status, event_count, parent_count, note
                 FROM runs ORDER BY id",
            )?;
            let metas = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })?;
            for row in metas {
                let (
                    id,
                    population_id,
                    gate_version_id,
                    comp_id,
                    transform_id,
                    status,
                    event_count,
                    parent_count,
                    note,
                ) = row?;
                let members = run_event_ids(&conn, &id)?.into_iter().collect::<Vec<_>>();
                let mut members = members;
                members.sort();
                runs.push(BundleRun {
                    id,
                    population_id,
                    gate_version_id,
                    comp_id,
                    transform_id,
                    status,
                    event_count,
                    parent_count,
                    note,
                    members,
                });
            }
        }
        Ok(Bundle {
            format: "flow-gate-bench-bundle/1".into(),
            fixture: fx,
            current_comp_id,
            current_transform_id,
            runs,
            exported_at: unix_ms(),
        })
    }

    /// 清空数据库、导入 bundle，从根重放，并将新运行与导出的当前运行逐一比对。
    pub fn import_and_verify(&self, bundle: Bundle) -> EngineResult<VerifyReport> {
        if bundle.format != "flow-gate-bench-bundle/1" {
            return Err(bad("导出格式不被识别"));
        }
        // 先验证 bundle 中引用的矩阵通道完整，避免污染空库。
        for comp in &bundle.fixture.compensations {
            if bundle.runs.iter().any(|r| r.comp_id == comp.id) {
                let zero = Vec::new();
                apply_compensation(&zero, comp)?;
            }
        }
        let mut conn = self.0.lock().unwrap();
        crate::db::replace_with_fixture(&mut conn, &bundle.fixture)?;
        {
            let tx = conn.transaction()?;
            invalidate_all(&tx)?;
            set_meta(&tx, "current_comp", &bundle.current_comp_id)?;
            set_meta(&tx, "current_transform", &bundle.current_transform_id)?;
            commit_recompute(
                &tx,
                &bundle.current_comp_id,
                &bundle.current_transform_id,
                "清空数据库后从导出记录重新导入并重放",
            )?;
            let mut checks = Vec::new();
            // 只核对导出时仍为当前（status=ok）的运行；invalid 旧运行保留于导出中供审计。
            for br in bundle.runs.iter().filter(|r| r.status == "ok") {
                let current_id: Option<String> = tx
                    .query_row(
                        "SELECT run_id FROM current_runs WHERE population_id=?1",
                        params![br.population_id],
                        |r| r.get(0),
                    )
                    .ok();
                let current_id = current_id
                    .ok_or_else(|| bad(format!("重放后群体 {} 缺少当前运行", br.population_id)))?;
                let actual = run_event_ids(&tx, &current_id)?;
                let expected: HashSet<String> = br.members.iter().cloned().collect();
                let match_ok = actual == expected;
                checks.push(PopCheck {
                    population_id: br.population_id.clone(),
                    expected_run: br.id.clone(),
                    replayed_run: current_id,
                    expected_count: br.members.len(),
                    replayed_count: actual.len(),
                    match_ok,
                    only_in_export: expected.difference(&actual).cloned().collect(),
                    only_in_replay: actual.difference(&expected).cloned().collect(),
                });
            }
            let all_ok = checks.iter().all(|c| c.match_ok);
            tx.commit()?;
            Ok(VerifyReport {
                fixture_version: bundle.fixture.version.clone(),
                current_comp_id: bundle.current_comp_id.clone(),
                current_transform_id: bundle.current_transform_id.clone(),
                populations_checked: checks.len(),
                all_ok,
                checks,
            })
        }
    }
}

#[derive(Serialize)]
pub struct VerifyReport {
    pub fixture_version: String,
    pub current_comp_id: String,
    pub current_transform_id: String,
    pub populations_checked: usize,
    pub all_ok: bool,
    pub checks: Vec<PopCheck>,
}

#[derive(Serialize)]
pub struct PopCheck {
    pub population_id: String,
    pub expected_run: String,
    pub replayed_run: String,
    pub expected_count: usize,
    pub replayed_count: usize,
    pub match_ok: bool,
    pub only_in_export: Vec<String>,
    pub only_in_replay: Vec<String>,
}

fn fixture_snapshot(conn: &Connection) -> EngineResult<Fixture> {
    use crate::model::{ChannelDef, RawEvent};
    let mut channels = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT name,label,kind FROM channels ORDER BY ord")?;
        for r in stmt.query_map([], |r| {
            Ok(ChannelDef {
                name: r.get(0)?,
                label: r.get(1)?,
                kind: r.get(2)?,
            })
        })? {
            channels.push(r?);
        }
    }
    let mut batches = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id,instrument,acquired_at FROM batches ORDER BY id")?;
        for r in stmt.query_map([], |r| {
            Ok(crate::model::BatchDef {
                id: r.get(0)?,
                instrument: r.get(1)?,
                acquired_at: r.get(2)?,
            })
        })? {
            batches.push(r?);
        }
    }
    let mut events = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id,batch_id,v0,v1,v2,v3 FROM events ORDER BY rowid")?;
        for r in stmt.query_map([], |r| {
            Ok(RawEvent {
                id: r.get(0)?,
                batch_id: r.get(1)?,
                values: vec![r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
            })
        })? {
            events.push(r?);
        }
    }
    let mut transforms = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT spec FROM transforms ORDER BY id")?;
        for spec in stmt.query_map([], |r| r.get::<_, String>(0))? {
            transforms.push(serde_json::from_str(&spec?)?);
        }
    }
    let mut compensations = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT spec FROM compensations ORDER BY id")?;
        for spec in stmt.query_map([], |r| r.get::<_, String>(0))? {
            compensations.push(serde_json::from_str(&spec?)?);
        }
    }
    // 当前活动门版本构成 populations 的门定义（与页面/重放入口一致）。
    let pops = load_populations(conn)?;
    let fixture_version = conn
        .query_row(
            "SELECT value FROM meta WHERE key='fixture_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "unknown".into());
    Ok(Fixture {
        version: fixture_version,
        channels,
        batches,
        events,
        transforms,
        compensations,
        populations: pops,
        current_comp_id: meta(conn, "current_comp")?,
        current_transform_id: meta(conn, "current_transform")?,
    })
}
