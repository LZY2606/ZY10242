pub mod compensation;
pub mod domain;
pub mod engine;
pub mod fixture;
pub mod geometry;
pub mod storage;
pub mod web;

#[cfg(test)]
mod geometry_tests;

/// 仅供集成测试使用的确定性测试支撑（临时 SQLite 文件 + HTTP 直调）。
pub mod test_support {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::engine;
    use crate::storage::Store;

    #[derive(Clone)]
pub struct Harness {
        pub dir: Arc<tempfile_path::TempPath>,
        pub store: Arc<Store>,
        app: axum::Router,
    }

    mod tempfile_path {
        use std::path::PathBuf;
        pub struct TempPath(pub PathBuf);
        impl TempPath {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicU64, Ordering};
                static N: AtomicU64 = AtomicU64::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                let pid = std::process::id();
                let p = std::env::temp_dir().join(format!("fgs-test-{pid}-{n}-{}.db", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
                TempPath(p)
            }
            pub fn as_str(&self) -> String {
                self.0.to_string_lossy().to_string()
            }
        }
        impl Drop for TempPath {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
                let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
                let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct RunRow {
        pub id: String,
        pub gate_id: String,
        pub status: String,
        pub count: Option<i64>,
        pub parent_count: Option<i64>,
        pub percent_of_parent: Option<f64>,
        pub members: Vec<String>,
    }

    impl Harness {
        pub fn new() -> Self {
            let dir = tempfile_path::TempPath::new();
            let path = dir.as_str();
            let store = Arc::new(Store::open(&path).expect("open db"));
            engine::seed_fixture(&store).expect("seed");
            let app = crate::web::router(store.clone());
            Harness { dir: Arc::new(dir), store, app }
        }

        async fn call(&self, req: Request<Body>) -> (StatusCode, Value) {
            let resp = self.app.clone().oneshot(req).await.expect("request");
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, v)
        }

        pub async fn post_json(&self, uri: &str, body: Value) -> (StatusCode, Value) {
            self.call(Request::builder()
                .method("POST").uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())).unwrap()).await
        }

        pub async fn get_json(&self, uri: &str) -> Value {
            let (_, v) = self.call(Request::builder().uri(uri).body(Body::empty()).unwrap()).await;
            v
        }

        pub async fn state_async(&self) -> Value {
            self.get_json("/api/state").await
        }
    }

    pub fn run_rows(s: &serde_json::Value) -> Vec<RunRow> {
        s["runs"].as_array().unwrap().iter().map(|r| RunRow {
            id: r["id"].as_str().unwrap().to_string(),
            gate_id: r["gate_id"].as_str().unwrap().to_string(),
            status: r["status"].as_str().unwrap().to_string(),
            count: { let v=&r["count"]; if v.is_null() {None} else {v.as_i64()} },
            parent_count: { let v=&r["parent_count"]; if v.is_null() {None} else {v.as_i64()} },
            percent_of_parent: r["percent_of_parent"].as_f64(),
            members: r["members"].as_array().unwrap().iter()
                .map(|m| m.as_str().unwrap().to_string()).collect(),
        }).collect()
    }

    pub fn state(h: &Harness) -> Value {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(h.get_json("/api/state"))
    }
    pub fn all_runs(h: &Harness) -> Vec<RunRow> {
        run_rows(&state(h))
    }
    pub fn active_run(h: &Harness, gate: &str) -> RunRow {
        all_runs(h).into_iter()
            .find(|r| r.gate_id == gate && r.status == "active")
            .expect("active run")
    }
    pub fn run_by_id(h: &Harness, id: &str) -> RunRow {
        all_runs(h).into_iter().find(|r| r.id == id).expect("run")
    }

    pub fn edit_gate(h: &Harness, gate: &str, verts: Vec<[f64; 2]>) {
        let run = active_run(h, gate);
        let s = state(h);
        let cur = s["runs"].as_array().unwrap().iter()
            .find(|r| r["id"] == run.id).unwrap();
        let body = serde_json::json!({
            "gate_id": gate,
            "label": format!("{gate} 修正"),
            "parent": cur["parent_gate"].clone(),
            "x_channel": cur["x_channel"].as_str().unwrap(),
            "y_channel": cur["y_channel"].as_str().unwrap(),
            "vertices": verts,
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (status, v) = rt.block_on(h.post_json("/api/gates/version", body));
        assert!(status.is_success(), "edit failed {status}: {v}");
    }

    pub fn try_edit_gate(h: &Harness, gate: &str, verts: Vec<[f64; 2]>) -> Result<(), String> {
        let run = active_run(h, gate);
        let s = state(h);
        let cur = s["runs"].as_array().unwrap().iter()
            .find(|r| r["id"] == run.id).unwrap();
        let body = serde_json::json!({
            "gate_id": gate, "label": "bad",
            "parent": cur["parent_gate"].clone(),
            "x_channel": cur["x_channel"].as_str().unwrap(),
            "y_channel": cur["y_channel"].as_str().unwrap(),
            "vertices": verts,
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (status, v) = rt.block_on(h.post_json("/api/gates/version", body));
        if status.is_success() { Ok(()) } else {
            Err(v["error"].as_str().unwrap_or("error").to_string())
        }
    }

    pub fn try_add_compensation(h: &Harness, channels: Value, matrix: Value) -> Result<(), String> {
        let body = serde_json::json!({"label":"t","channels":channels,"matrix":matrix,"activate":false});
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (status, v) = rt.block_on(h.post_json("/api/compensations", body));
        if status.is_success() { Ok(()) } else {
            Err(v["error"].as_str().unwrap_or("error").to_string())
        }
    }

    fn switch(h: &Harness, key: &str, id: &str) {
        let body = serde_json::json!({key: id});
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (status, v) = rt.block_on(h.post_json("/api/switch", body));
        assert!(status.is_success(), "switch failed {status}: {v}");
    }
    pub fn try_create_gate(h: &Harness, id: &str, label: &str, parent: Option<&str>,
        x: &str, y: &str, verts: Vec<[f64; 2]>) -> Result<(), String> {
        let body = serde_json::json!({
            "id": id, "label": label,
            "parent": parent, "x_channel": x, "y_channel": y, "vertices": verts,
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (status, v) = rt.block_on(h.post_json("/api/gates/new", body));
        if status.is_success() { Ok(()) } else {
            Err(v["error"].as_str().unwrap_or("error").to_string())
        }
    }

    pub fn switch_compensation(h: &Harness, id: &str) { switch(h, "compensation", id) }
    pub fn switch_transform(h: &Harness, id: &str) { switch(h, "transform", id) }

    pub fn diff(h: &Harness, a: &str, b: &str) -> Value {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(h.get_json(&format!("/api/diff?a={a}&b={b}")))
    }

    pub fn export(h: &Harness) -> Value {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(h.get_json("/api/export"))
    }

    pub struct ImportReport { pub checked: usize, pub passed: usize, pub failures: Vec<String> }
    pub fn wipe_and_import(h: &Harness, bundle: &Value) -> ImportReport {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (status, v) = rt.block_on(h.post_json("/api/import", bundle.clone()));
        assert!(status.is_success(), "import failed {status}: {v}");
        ImportReport {
            checked: v["imported"].as_u64().unwrap() as usize,
            passed: v["passed"].as_u64().unwrap() as usize,
            failures: v["failures"].as_array().unwrap().iter()
                .map(|f| f.as_str().unwrap().to_string()).collect(),
        }
    }

    pub fn verify(h: &Harness) -> ImportReport {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let v = rt.block_on(h.get_json("/api/verify"));
        ImportReport {
            checked: v["checked"].as_u64().unwrap() as usize,
            passed: v["passed"].as_u64().unwrap() as usize,
            failures: v["failures"].as_array().unwrap().iter()
                .map(|f| f.as_str().unwrap().to_string()).collect(),
        }
    }

    #[derive(Debug)]
    pub struct VersionRow { pub db_id: String, pub version: i64 }
    pub fn gate_versions(h: &Harness, gate: &str) -> Vec<VersionRow> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let v = rt.block_on(h.get_json(&format!("/api/gate-versions?gate_id={gate}")));
        v.as_array().unwrap().iter().map(|x| VersionRow {
            db_id: x["db_id"].as_str().unwrap().to_string(),
            version: x["version"].as_i64().unwrap(),
        }).collect()
    }

    pub fn activate_version(h: &Harness, gate: &str, db_id: &str) {
        let body = serde_json::json!({"gate_id":gate,"gate_version":db_id});
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (status, v) = rt.block_on(h.post_json("/api/gates/activate", body));
        assert!(status.is_success(), "activate failed {status}: {v}");
    }

    // 避免未使用目录字段告警
    #[allow(dead_code)]
    fn keep_path(h: &Harness) -> String { h.dir.as_str() }
}
