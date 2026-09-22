//! Axum HTTP 层：静态页面 + JSON API。

use crate::db::Db;
use crate::engine::{Bundle, EngineError};
use crate::model::GatePolygon;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
}

pub fn router(db: Arc<Db>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/static/app.js", get(app_js))
        .route("/static/style.css", get(style_css))
        .route("/api/state", get(state))
        .route("/api/scatter", get(scatter))
        .route("/api/histogram", get(histogram))
        .route("/api/runs", get(runs))
        .route("/api/diff", get(diff))
        .route("/api/gates/edit", post(edit_gate))
        .route("/api/replay/comp", post(replay_comp))
        .route("/api/replay/transform", post(replay_transform))
        .route("/api/reseed", post(reseed))
        .route("/api/export", get(export_bundle))
        .route("/api/import", post(import_bundle))
        .with_state(AppState { db })
}

async fn index() -> Response {
    serve(
        include_str!("../static/index.html"),
        "text/html; charset=utf-8",
    )
}
async fn app_js() -> Response {
    serve(
        include_str!("../static/app.js"),
        "application/javascript; charset=utf-8",
    )
}
async fn style_css() -> Response {
    serve(
        include_str!("../static/style.css"),
        "text/css; charset=utf-8",
    )
}

fn serve(body: &'static str, content_type: &'static str) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], body).into_response()
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.1 });
        (self.0, Json(body)).into_response()
    }
}

fn map_err(e: EngineError) -> ApiError {
    match e {
        EngineError::Bad(msg) => ApiError(StatusCode::UNPROCESSABLE_ENTITY, msg),
        EngineError::Db(msg) => ApiError(StatusCode::INTERNAL_SERVER_ERROR, msg),
    }
}

async fn state(State(s): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(s.db.state().map_err(map_err)?))
}

#[derive(Deserialize)]
struct ScatterQ {
    run_id: Option<String>,
    x: String,
    y: String,
    limit: Option<usize>,
}

async fn scatter(
    State(s): State<AppState>,
    Query(q): Query<ScatterQ>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        s.db.scatter(q.run_id.as_deref(), &q.x, &q.y, q.limit.unwrap_or(1000))
            .map_err(map_err)?,
    ))
}

#[derive(Deserialize)]
struct HistQ {
    run_id: Option<String>,
    parent_run_id: Option<String>,
    channel: String,
    bins: Option<usize>,
}

async fn histogram(
    State(s): State<AppState>,
    Query(q): Query<HistQ>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        s.db.histogram(
            q.run_id.as_deref(),
            q.parent_run_id.as_deref(),
            &q.channel,
            q.bins.unwrap_or(25),
        )
        .map_err(map_err)?,
    ))
}

#[derive(Deserialize)]
struct PopQ {
    population_id: String,
}

async fn runs(
    State(s): State<AppState>,
    Query(q): Query<PopQ>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(s.db.runs_of(&q.population_id).map_err(map_err)?))
}

#[derive(Deserialize)]
struct DiffQ {
    a: String,
    b: String,
}

async fn diff(
    State(s): State<AppState>,
    Query(q): Query<DiffQ>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(s.db.diff(&q.a, &q.b).map_err(map_err)?))
}

#[derive(Deserialize)]
struct EditGateReq {
    population_id: String,
    note: String,
    polygon: GatePolygon,
}

async fn edit_gate(
    State(s): State<AppState>,
    Json(req): Json<EditGateReq>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        s.db.edit_gate(&req.population_id, &req.note, req.polygon)
            .map_err(map_err)?,
    ))
}

#[derive(Deserialize)]
struct ReplayReq {
    id: String,
}

async fn replay_comp(
    State(s): State<AppState>,
    Json(req): Json<ReplayReq>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(s.db.replay_comp(&req.id).map_err(map_err)?))
}

async fn replay_transform(
    State(s): State<AppState>,
    Json(req): Json<ReplayReq>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(s.db.replay_transform(&req.id).map_err(map_err)?))
}

async fn reseed(State(s): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        s.db.reseed(&crate::model::fixture()).map_err(map_err)?,
    ))
}

async fn export_bundle(State(s): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let bundle = s.db.export_bundle().map_err(map_err)?;
    let body = serde_json::to_vec_pretty(&bundle).unwrap();
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"flow-gate-bench-bundle.json\"",
            ),
        ],
        body,
    ))
}

async fn import_bundle(
    State(s): State<AppState>,
    Json(bundle): Json<Bundle>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(s.db.import_and_verify(bundle).map_err(map_err)?))
}
