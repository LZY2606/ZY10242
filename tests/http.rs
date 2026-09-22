//! HTTP 冒烟与验收：页面标题、JSON API、422 语义、导入/导出。

use flow_gate_bench::model::fixture;
use flow_gate_bench::{web, Db};
use std::sync::Arc;

struct TestServer {
    base: String,
}

impl TestServer {
    fn start() -> Self {
        let db = Arc::new(Db::open(":memory:").unwrap());
        db.reseed(&fixture()).unwrap();
        let app = web::router(db);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(l.local_addr().unwrap()).unwrap();
                axum::serve(l, app.into_make_service()).await.unwrap();
            });
        });
        let addr = rx.recv().unwrap();
        TestServer {
            base: format!("http://{addr}"),
        }
    }
}

fn get(url: &str) -> (u16, String) {
    let resp = reqwest_get(url);
    resp
}

fn reqwest_get(url: &str) -> (u16, String) {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let url_part = url.strip_prefix("http://").unwrap_or(url);
    let host_end = url_part.find('/').unwrap_or(url_part.len());
    let host = &url_part[..host_end];
    let path = if host_end == url_part.len() {
        "/"
    } else {
        &url_part[host_end..]
    };
    let mut stream = TcpStream::connect(host).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    parse(&raw)
}

fn post_json(url: &str, body: &str) -> (u16, String) {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let url_part = url.strip_prefix("http://").unwrap_or(url);
    let host_end = url_part.find('/').unwrap_or(url_part.len());
    let host = &url_part[..host_end];
    let path = &url_part[host_end..];
    let mut stream = TcpStream::connect(host).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    parse(&raw)
}

fn parse(raw: &str) -> (u16, String) {
    let head_end = raw.find("\r\n\r\n").unwrap();
    let status: u16 = raw
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, raw[head_end + 4..].to_string())
}

#[test]
fn index_shows_title_and_state_api() {
    let s = TestServer::start();
    let (status, body) = get(&format!("{}/", s.base));
    assert_eq!(status, 200);
    assert!(body.contains("流式门谱台"));
    let (status, body) = get(&format!("{}/api/state", s.base));
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["runs"].as_array().unwrap().len(), 7);
    assert_eq!(v["current_comp_id"], "C1");
}

#[test]
fn missing_comp_channel_returns_422() {
    let s = TestServer::start();
    let (status, body) = post_json(&format!("{}/api/replay/comp", s.base), r#"{"id":"C4"}"#);
    assert_eq!(status, 422);
    assert!(body.contains("SSC_A"));
}

#[test]
fn gate_edit_then_export_import_roundtrip() {
    let s = TestServer::start();
    // 查看初始 T 门。
    let (_, body) = get(&format!("{}/api/state", s.base));
    let state: serde_json::Value = serde_json::from_str(&body).unwrap();
    let t_pop = state["populations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "T")
        .unwrap()
        .clone();
    let mut poly = t_pop["active_gate"].clone();
    poly["y_channel"] = "CD3".into();
    poly["x_channel"] = "CD19".into();
    // 轻微抬高门。
    for v in poly["vertices"].as_array_mut().unwrap() {
        let y = v["y"].as_f64().unwrap();
        v["y"] = (y + 10.0).into();
    }
    let req = serde_json::json!({
        "population_id": "T",
        "note": "HTTP 修正",
        "polygon": poly,
    });
    let (status, _) = post_json(&format!("{}/api/gates/edit", s.base), &req.to_string());
    assert_eq!(status, 200);

    let (status, bundle) = get(&format!("{}/api/export", s.base));
    assert_eq!(status, 200);
    let parsed: serde_json::Value = serde_json::from_str(&bundle).unwrap();
    assert_eq!(parsed["format"], "flow-gate-bench-bundle/1");

    let (status, report) = post_json(&format!("{}/api/import", s.base), &bundle);
    assert_eq!(status, 200);
    let r: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(r["all_ok"], true);
}

#[test]
fn scatter_and_histogram_and_diff_endpoints() {
    let s = TestServer::start();
    let (st, body) = get(&format!("{}/api/scatter?x=FSC_A&y=SSC_A&limit=500", s.base));
    assert_eq!(st, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["points"].as_array().unwrap().len(), 384.min(500));

    let (st, body) = get(&format!("{}/api/histogram?channel=CD3&bins=20", s.base));
    assert_eq!(st, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["bins"], 20);

    let (_, state_body) = get(&format!("{}/api/runs?population_id=T", s.base));
    let runs: serde_json::Value = serde_json::from_str(&state_body).unwrap();
    let rid = runs[0]["id"].as_str().unwrap();
    let (st, body) = get(&format!("{}/api/diff?a={rid}&b={rid}", s.base));
    assert_eq!(st, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["union_count"].as_u64().unwrap() > 0);
}
