//! SQLite 存储：schema、固定 fixture 播种与版本读写。

use crate::model::Fixture;
use rusqlite::{params, Connection};
use std::sync::Mutex;

pub struct Db(pub Mutex<Connection>);

pub const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS channels (
  name TEXT PRIMARY KEY,
  label TEXT NOT NULL,
  kind TEXT NOT NULL,
  ord INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS batches (
  id TEXT PRIMARY KEY,
  instrument TEXT NOT NULL,
  acquired_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
  id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL REFERENCES batches(id),
  v0 REAL NOT NULL,
  v1 REAL NOT NULL,
  v2 REAL NOT NULL,
  v3 REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS transforms (
  id TEXT PRIMARY KEY,
  label TEXT NOT NULL,
  spec TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS compensations (
  id TEXT PRIMARY KEY,
  label TEXT NOT NULL,
  spec TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS populations (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  parent_id TEXT REFERENCES populations(id)
);

CREATE TABLE IF NOT EXISTS gate_versions (
  id TEXT PRIMARY KEY,
  population_id TEXT NOT NULL REFERENCES populations(id),
  note TEXT NOT NULL,
  spec TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  active INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  population_id TEXT NOT NULL REFERENCES populations(id),
  gate_version_id TEXT REFERENCES gate_versions(id),
  comp_id TEXT NOT NULL,
  transform_id TEXT NOT NULL,
  status TEXT NOT NULL,
  event_count INTEGER,
  parent_count INTEGER,
  error TEXT,
  created_at INTEGER NOT NULL,
  note TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS run_members (
  run_id TEXT NOT NULL REFERENCES runs(id),
  event_id TEXT NOT NULL REFERENCES events(id),
  ord INTEGER NOT NULL,
  PRIMARY KEY (run_id, event_id)
);

CREATE TABLE IF NOT EXISTS current_runs (
  population_id TEXT PRIMARY KEY REFERENCES populations(id),
  run_id TEXT NOT NULL REFERENCES runs(id)
);
"#;

impl Db {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory()?
        } else {
            Connection::open(path)?
        };
        conn.execute_batch(SCHEMA)?;
        Ok(Db(Mutex::new(conn)))
    }

    pub fn meta_get(&self, key: &str) -> Option<String> {
        let conn = self.0.lock().unwrap();
        conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get::<_, String>(0)
        })
        .ok()
    }

    pub fn meta_set(&self, conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

/// 清空并写入 fixture；不计算运行（由 engine::reseed 负责）。
pub fn replace_with_fixture(conn: &mut Connection, fx: &Fixture) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    wipe_tx(&tx)?;
    seed_tx(&tx, fx)?;
    tx.commit()?;
    Ok(())
}

pub fn wipe_tx(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    tx.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
    for table in [
        "run_members",
        "current_runs",
        "runs",
        "gate_versions",
        "populations",
        "compensations",
        "transforms",
        "events",
        "batches",
        "channels",
        "meta",
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    Ok(())
}

pub fn seed_tx(tx: &rusqlite::Transaction, fx: &Fixture) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO meta(key, value) VALUES('fixture_version', ?1),
         ('current_comp', ?2), ('current_transform', ?3)",
        params![fx.version, fx.current_comp_id, fx.current_transform_id],
    )?;
    for (i, c) in fx.channels.iter().enumerate() {
        tx.execute(
            "INSERT INTO channels(name, label, kind, ord) VALUES(?1,?2,?3,?4)",
            params![c.name, c.label, c.kind, i as i64],
        )?;
    }
    for b in &fx.batches {
        tx.execute(
            "INSERT INTO batches(id, instrument, acquired_at) VALUES(?1,?2,?3)",
            params![b.id, b.instrument, b.acquired_at],
        )?;
    }
    for e in &fx.events {
        tx.execute(
            "INSERT INTO events(id, batch_id, v0, v1, v2, v3) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                e.id,
                e.batch_id,
                e.values[0],
                e.values[1],
                e.values[2],
                e.values[3]
            ],
        )?;
    }
    for t in &fx.transforms {
        tx.execute(
            "INSERT INTO transforms(id, label, spec) VALUES(?1,?2,?3)",
            params![t.id, t.label, serde_json::to_string(t).unwrap()],
        )?;
    }
    for c in &fx.compensations {
        tx.execute(
            "INSERT INTO compensations(id, label, spec) VALUES(?1,?2,?3)",
            params![c.id, c.label, serde_json::to_string(c).unwrap()],
        )?;
    }
    for p in &fx.populations {
        tx.execute(
            "INSERT INTO populations(id, name, parent_id) VALUES(?1,?2,?3)",
            params![p.id, p.name, p.parent],
        )?;
    }
    let mut gate_ts = 1_000i64;
    for p in &fx.populations {
        if let Some(g) = &p.gate {
            gate_ts += 1;
            tx.execute(
                "INSERT INTO gate_versions(id, population_id, note, spec, created_at, active)
                 VALUES(?1,?2,?3,?4,?5,1)",
                params![
                    g.id,
                    p.id,
                    g.note,
                    serde_json::to_string(g).unwrap(),
                    gate_ts,
                ],
            )?;
        }
    }
    Ok(())
}
