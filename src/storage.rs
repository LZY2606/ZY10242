//! SQLite 存储层。所有版本定义只追加、不改写；运行结果也永久保留，
//! 失效/被取代的运行以状态标记区分，保证审计与重放。

use rusqlite::{params, Connection};
use std::sync::Mutex;


#[derive(Debug)]
pub struct Store {
    pub conn: Mutex<Connection>,
}

pub fn current_ts() -> String {
    // 单调递增的逻辑时钟由 created_at 单调 + 整数 id 共同保证；
    // 这里给出可读时间戳。
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS events (
            id         TEXT PRIMARY KEY,
            batch      TEXT NOT NULL,
            raw_json   TEXT NOT NULL,
            sort_index INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS compensation_versions (
            id         TEXT PRIMARY KEY,
            label      TEXT NOT NULL,
            channels   TEXT NOT NULL,  -- JSON array，显式通道顺序
            matrix_json TEXT NOT NULL, -- JSON rows
            active     INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS transform_versions (
            id         TEXT PRIMARY KEY,
            label      TEXT NOT NULL,
            params_json TEXT NOT NULL, -- JSON: 每通道一个 TransformParams
            active     INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS gates (
            id     TEXT PRIMARY KEY,
            label  TEXT NOT NULL,
            parent TEXT REFERENCES gates(id),
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS gate_versions (
            id         TEXT PRIMARY KEY,
            gate_id    TEXT NOT NULL REFERENCES gates(id),
            version    INTEGER NOT NULL,
            label      TEXT NOT NULL,
            parent     TEXT,
            x_channel  TEXT NOT NULL,
            y_channel  TEXT NOT NULL,
            vertices_json TEXT NOT NULL,
            active     INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            UNIQUE(gate_id, version)
        );

        CREATE TABLE IF NOT EXISTS runs (
            id           TEXT PRIMARY KEY,
            gate_id      TEXT NOT NULL REFERENCES gates(id),
            gate_version TEXT NOT NULL REFERENCES gate_versions(id),
            parent_run   TEXT REFERENCES runs(id),
            compensation_version TEXT NOT NULL REFERENCES compensation_versions(id),
            transform_version    TEXT NOT NULL REFERENCES transform_versions(id),
            status       TEXT NOT NULL,            -- active|superseded|invalidated
            count        INTEGER,                  -- 失效后为 NULL：绝不显示旧数
            parent_count INTEGER,
            event_ids_json TEXT NOT NULL DEFAULT '[]',
            lineage      TEXT NOT NULL DEFAULT '', -- 根到该节点的运行 id 路径
            created_at   TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS run_verify (
            run_id TEXT PRIMARY KEY REFERENCES runs(id),
            ok     INTEGER NOT NULL,
            expected_count INTEGER NOT NULL,
            detail TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_runs_gate ON runs(gate_id, status);
        CREATE INDEX IF NOT EXISTS idx_gate_versions_gate ON gate_versions(gate_id, active);
        "#,
    )
}

impl Store {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        init(&conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    pub fn is_empty(&self) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        Ok(n == 0)
    }

    // ---------- 事件 ----------

    pub fn replace_events(
        &self,
        events: &[(String, String, String)],
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM events", [])?;
        {
            let mut stmt =
                tx.prepare("INSERT INTO events(id,batch,raw_json,sort_index) VALUES(?1,?2,?3,?4)")?;
            for (i, (id, batch, raw)) in events.iter().enumerate() {
                stmt.execute(params![id, batch, raw, i as i64])?;
            }
        }
        tx.commit()
    }

    pub fn load_events(&self) -> rusqlite::Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id,batch,raw_json FROM events ORDER BY sort_index")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---------- 补偿 / 变换 ----------

    pub fn insert_compensation(
        &self,
        id: &str,
        label: &str,
        channels_json: &str,
        matrix_json: &str,
        active: bool,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO compensation_versions(id,label,channels,matrix_json,active,created_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![id, label, channels_json, matrix_json, active as i64, current_ts()],
        )?;
        Ok(())
    }

    pub fn insert_transform(
        &self,
        id: &str,
        label: &str,
        params_json: &str,
        active: bool,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO transform_versions(id,label,params_json,active,created_at)
             VALUES(?1,?2,?3,?4,?5)",
            params![id, label, params_json, active as i64, current_ts()],
        )?;
        Ok(())
    }

    // ---------- 门 ----------

    pub fn insert_gate(&self, id: &str, label: &str, parent: Option<&str>) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO gates(id,label,parent,created_at) VALUES(?1,?2,?3,?4)",
            params![id, label, parent, current_ts()],
        )?;
        Ok(())
    }

    pub fn insert_gate_version(
        &self,
        id: &str,
        gate_id: &str,
        version: i64,
        label: &str,
        parent: Option<&str>,
        x_channel: &str,
        y_channel: &str,
        vertices_json: &str,
        active: bool,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO gate_versions(id,gate_id,version,label,parent,x_channel,y_channel,
                 vertices_json,active,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                id,
                gate_id,
                version,
                label,
                parent,
                x_channel,
                y_channel,
                vertices_json,
                active as i64,
                current_ts()
            ],
        )?;
        Ok(())
    }

    /// 清空全部业务数据（事件、定义、运行），用于“清空数据库后重新导入复核”。
    pub fn wipe(&self) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for table in [
            "run_verify",
            "runs",
            "gate_versions",
            "gates",
            "transform_versions",
            "compensation_versions",
            "events",
        ] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        tx.commit()
    }
}

/// 供引擎/HTTP 使用的只读行类型。
pub struct ActiveContext {
    pub compensation_id: String,
    pub channels_json: String,
    pub matrix_json: String,
    pub transform_id: String,
    pub transform_params_json: String,
}

pub fn active_context(conn: &Connection) -> rusqlite::Result<ActiveContext> {
    conn.query_row(
        "SELECT c.id,c.channels,c.matrix_json,t.id,t.params_json
         FROM compensation_versions c JOIN transform_versions t
         WHERE c.active=1 AND t.active=1 LIMIT 1",
        [],
        |r| {
            Ok(ActiveContext {
                compensation_id: r.get(0)?,
                channels_json: r.get(1)?,
                matrix_json: r.get(2)?,
                transform_id: r.get(3)?,
                transform_params_json: r.get(4)?,
            })
        },
    )
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GateVersionRow {
    pub id: String,
    pub gate_id: String,
    pub version: i64,
    pub label: String,
    pub parent: Option<String>,
    pub x_channel: String,
    pub y_channel: String,
    pub vertices_json: String,
}

pub fn active_gate_versions(conn: &Connection) -> rusqlite::Result<Vec<GateVersionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id,gate_id,version,label,parent,x_channel,y_channel,vertices_json
         FROM gate_versions WHERE active=1",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(GateVersionRow {
                id: r.get(0)?,
                gate_id: r.get(1)?,
                version: r.get(2)?,
                label: r.get(3)?,
                parent: r.get(4)?,
                x_channel: r.get(5)?,
                y_channel: r.get(6)?,
                vertices_json: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn gate_version_by_id(conn: &Connection, id: &str) -> rusqlite::Result<GateVersionRow> {
    conn.query_row(
        "SELECT id,gate_id,version,label,parent,x_channel,y_channel,vertices_json
         FROM gate_versions WHERE id=?1",
        params![id],
        |r| {
            Ok(GateVersionRow {
                id: r.get(0)?,
                gate_id: r.get(1)?,
                version: r.get(2)?,
                label: r.get(3)?,
                parent: r.get(4)?,
                x_channel: r.get(5)?,
                y_channel: r.get(6)?,
                vertices_json: r.get(7)?,
            })
        },
    )
}

