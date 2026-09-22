//! Axum HTTP 层：单页 UI + JSON API。

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::engine;
use crate::storage::Store;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
}

pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/static/app.js", get(app_js))
        .route("/static/style.css", get(style_css))
        .route("/api/state", get(state))
        .route("/api/projection", get(projection))
        .route("/api/hist", get(hist))
        .route("/api/runs", get(runs))
        .route("/api/gate-versions", get(gate_versions))
        .route("/api/versions", get(versions))
        .route("/api/diff", get(diff))
        .route("/api/export", get(export))
        .route("/api/verify", get(verify))
        .route("/api/switch", post(switch))
        .route("/api/gates/new", post(create_gate))
        .route("/api/gates/version", post(new_gate_version))
        .route("/api/gates/activate", post(activate_gate_version))
        .route("/api/compensations", post(add_compensation))
        .route("/api/transforms", post(add_transform))
        .route("/api/import", post(import_bundle))
        .route("/api/reseed", post(reseed))
        .with_state(AppState { store })
}

async fn index() -> Html<&'static str> {
    Html(include_str!("static/index.html"))
}
async fn app_js() -> impl IntoResponse {
    ([("content-type", "application/javascript; charset=utf-8")], include_str!("static/app.js"))
}
async fn style_css() -> impl IntoResponse {
    ([("content-type", "text/css; charset=utf-8")], include_str!("static/style.css"))
}

fn err500(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
    )
        .into_response()
}
fn err400(e: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))).into_response()
}

// ---------- 只读 ----------

#[derive(Serialize)]
struct RunView {
    #[serde(flatten)]
    info: engine::RunInfo,
    members: Vec<String>,
}

async fn state(State(st): State<AppState>) -> Response {
    match build_state(&st.store) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err500(e),
    }
}

fn build_state(store: &Store) -> Result<serde_json::Value, engine::EngineError> {
    let infos = engine::list_runs(store, false)?;
    let conn = store.conn.lock().unwrap();
    let mut runs = Vec::new();
    for info in infos {
        let members: Vec<String> = if info.status == crate::domain::RunStatus::Active {
            let j: String = conn.query_row(
                "SELECT event_ids_json FROM runs WHERE id=?1",
                params![info.id],
                |r| r.get(0),
            )?;
            serde_json::from_str(&j)?
        } else {
            Vec::new()
        };
        runs.push(RunView { info, members });
    }
    let event_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
    let batches: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT batch FROM events ORDER BY batch")?;
        let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let active_comp: String = conn.query_row(
        "SELECT id FROM compensation_versions WHERE active=1", [], |r| r.get(0))?;
    let active_trans: String = conn.query_row(
        "SELECT id FROM transform_versions WHERE active=1", [], |r| r.get(0))?;
    drop(conn);

    Ok(json!({
        "title": "流式门谱台",
        "channels": crate::fixture::EVENT_CHANNELS,
        "event_count": event_count,
        "batches": batches,
        "active_compensation": active_comp,
        "active_transform": active_trans,
        "runs": runs,
    }))
}

async fn runs(State(st): State<AppState>) -> Response {
    match engine::list_runs(&st.store, false) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err500(e),
    }
}

#[derive(Deserialize)]
struct GateQ {
    gate_id: String,
}
async fn gate_versions(State(st): State<AppState>, Query(q): Query<GateQ>) -> Response {
    match engine::list_gate_versions(&st.store, &q.gate_id) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err500(e),
    }
}

async fn versions(State(st): State<AppState>) -> Response {
    match (|| {
        Ok(json!({
            "compensations": engine::list_compensations(&st.store)?,
            "transforms": engine::list_transforms(&st.store)?,
        }))
    })()
    .and_then(|v| Ok::<_, engine::EngineError>(Json(v).into_response()))
    {
        Ok(r) => r,
        Err(e) => err500(e),
    }
}

#[derive(Deserialize)]
struct DiffQ {
    a: String,
    b: String,
}
async fn diff(State(st): State<AppState>, Query(q): Query<DiffQ>) -> Response {
    match engine::diff_runs(&st.store, &q.a, &q.b) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err500(e),
    }
}

async fn export(State(st): State<AppState>) -> Response {
    match engine::export_bundle(&st.store) {
        Ok(b) => Json(b).into_response(),
        Err(e) => err500(e),
    }
}

async fn verify(State(st): State<AppState>) -> Response {
    match engine::verify_runs(&st.store) {
        Ok((checked, passed, failures)) => Json(json!({
            "checked": checked, "passed": passed, "failures": failures
        }))
        .into_response(),
        Err(e) => err500(e),
    }
}

// ---------- 投影 / 直方图 ----------

fn prepared_projection(
    store: &Store,
    x: &str,
    y: &str,
) -> Result<Vec<serde_json::Value>, engine::EngineError> {
    let conn = store.conn.lock().unwrap();
    let ctx = crate::storage::active_context(&conn)?;
    let comp = engine::parse_compensation(&ctx.channels_json, &ctx.matrix_json)?;
    let transforms: BTreeMap<String, crate::domain::TransformParams> =
        serde_json::from_str(&ctx.transform_params_json)?;
    drop(conn);
    let events = engine::prepare_events(store, &comp, &transforms)?;
    Ok(events
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id, "batch": e.batch,
                "x": e.values.get(x).copied().unwrap_or(0.0),
                "y": e.values.get(y).copied().unwrap_or(0.0),
            })
        })
        .collect())
}

#[derive(Deserialize)]
struct ProjQ {
    x: String,
    y: String,
}
async fn projection(State(st): State<AppState>, Query(q): Query<ProjQ>) -> Response {
    match prepared_projection(&st.store, &q.x, &q.y) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err500(e),
    }
}

#[derive(Deserialize)]
struct HistQ {
    channel: String,
    bins: Option<usize>,
}
async fn hist(State(st): State<AppState>, Query(q): Query<HistQ>) -> Response {
    let conn = st.store.conn.lock().unwrap();
    let ctx = match crate::storage::active_context(&conn) {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    let comp = match engine::parse_compensation(&ctx.channels_json, &ctx.matrix_json) {
        Ok(c) => c,
        Err(e) => return err500(e),
    };
    let transforms: BTreeMap<String, crate::domain::TransformParams> =
        match serde_json::from_str(&ctx.transform_params_json) {
            Ok(t) => t,
            Err(e) => return err500(e),
        };
    drop(conn);
    let events = match engine::prepare_events(&st.store, &comp, &transforms) {
        Ok(e) => e,
        Err(e) => return err500(e),
    };
    let vals: Vec<f64> = events
        .iter()
        .map(|e| *e.values.get(&q.channel).unwrap_or(&0.0))
        .collect();
    let lo = vals.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let n = q.bins.unwrap_or(30).clamp(1, 200);
    let width = if hi > lo { (hi - lo) / n as f64 } else { 1.0 };
    let mut counts = vec![0i64; n];
    for v in vals {
        let mut idx = if width > 0.0 { ((v - lo) / width).floor() as i64 } else { 0 };
        if idx < 0 { idx = 0; }
        if idx >= n as i64 { idx = n as i64 - 1; }
        counts[idx as usize] += 1;
    }
    Json(json!({"channel": q.channel, "lo": lo, "hi": hi, "bin_width": width, "counts": counts}))
        .into_response()
}

// ---------- 变更 ----------

#[derive(Deserialize)]
struct SwitchReq {
    compensation: Option<String>,
    transform: Option<String>,
}
async fn switch(State(st): State<AppState>, Json(req): Json<SwitchReq>) -> Response {
    if req.compensation.is_none() && req.transform.is_none() {
        return err400("nothing to switch");
    }
    match engine::switch_context(&st.store, req.compensation.as_deref(), req.transform.as_deref())
    {
        Ok(()) => build_state(&st.store).map(Json).map(|j| j.into_response()).unwrap_or_else(err500),
        Err(e) => map_engine_error(e),
    }
}

fn map_engine_error(e: engine::EngineError) -> Response {
    let msg = e.to_string();
    let code = if msg.contains("channel mismatch")
        || msg.contains("invalid polygon")
        || msg.contains("shared-edge")
        || msg.contains("not found")
        || msg.contains("parent gate")
    {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (code, Json(json!({"error": msg}))).into_response()
}

#[derive(Deserialize)]
struct NewGateReq {
    id: String,
    label: String,
    parent: Option<String>,
    x_channel: String,
    y_channel: String,
    vertices: Vec<[f64; 2]>,
}
async fn create_gate(State(st): State<AppState>, Json(req): Json<NewGateReq>) -> Response {
    let verts: Vec<(f64, f64)> = req.vertices.iter().map(|p| (p[0], p[1])).collect();
    match engine::create_gate(
        &st.store, &req.id, &req.label, req.parent.as_deref(),
        &req.x_channel, &req.y_channel, verts,
    ) {
        Ok(()) => build_state(&st.store).map(Json).map(|j| j.into_response()).unwrap_or_else(err500),
        Err(e) => map_engine_error(e),
    }
}

#[derive(Deserialize)]
struct NewVersionReq {
    gate_id: String,
    label: String,
    parent: Option<String>,
    x_channel: String,
    y_channel: String,
    vertices: Vec<[f64; 2]>,
}
async fn new_gate_version(State(st): State<AppState>, Json(req): Json<NewVersionReq>) -> Response {
    let verts: Vec<(f64, f64)> = req.vertices.iter().map(|p| (p[0], p[1])).collect();
    match engine::add_gate_version(
        &st.store, &req.gate_id, &req.label, req.parent.as_deref(),
        &req.x_channel, &req.y_channel, verts,
    ) {
        Ok(id) => {
            let _ = id;
            build_state(&st.store).map(Json).map(|j| j.into_response()).unwrap_or_else(err500)
        }
        Err(e) => map_engine_error(e),
    }
}

#[derive(Deserialize)]
struct ActivateReq {
    gate_id: String,
    gate_version: String,
}
async fn activate_gate_version(State(st): State<AppState>, Json(req): Json<ActivateReq>) -> Response {
    match engine::activate_gate_version(&st.store, &req.gate_id, &req.gate_version) {
        Ok(()) => build_state(&st.store).map(Json).map(|j| j.into_response()).unwrap_or_else(err500),
        Err(e) => map_engine_error(e),
    }
}

#[derive(Deserialize)]
struct NewCompReq {
    label: String,
    channels: Vec<String>,
    matrix: Vec<Vec<f64>>,
    activate: bool,
}
async fn add_compensation(State(st): State<AppState>, Json(req): Json<NewCompReq>) -> Response {
    match engine::add_compensation_version(
        &st.store, &req.label, &req.channels, &req.matrix, req.activate,
    ) {
        Ok(id) => Json(json!({"id": id})).into_response(),
        Err(e) => map_engine_error(e),
    }
}

#[derive(Deserialize)]
struct NewTransReq {
    label: String,
    params: BTreeMap<String, crate::domain::TransformParams>,
    activate: bool,
}
async fn add_transform(State(st): State<AppState>, Json(req): Json<NewTransReq>) -> Response {
    match engine::add_transform_version(&st.store, &req.label, &req.params, req.activate) {
        Ok(id) => Json(json!({"id": id})).into_response(),
        Err(e) => map_engine_error(e),
    }
}

async fn import_bundle(State(st): State<AppState>, body: String) -> Response {
    match engine::import_bundle_verify(&st.store, &body) {
        Ok((checked, passed, failures)) => {
            Json(json!({"imported": checked, "passed": passed, "failures": failures}))
                .into_response()
        }
        Err(e) => err400(e),
    }
}

async fn reseed(State(st): State<AppState>) -> Response {
    if let Err(e) = st.store.wipe() {
        return err500(e);
    }
    match engine::seed_fixture(&st.store) {
        Ok(()) => build_state(&st.store).map(Json).map(|j| j.into_response()).unwrap_or_else(err500),
        Err(e) => err500(e),
    }
}
