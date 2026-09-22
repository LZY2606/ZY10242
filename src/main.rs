use std::sync::Arc;

use flow_gate_station::{engine, storage::Store, web};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mut listen = "127.0.0.1:5582".to_string();
    let mut db_path = "flow-gate-station.db".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                i += 1;
                listen = args[i].clone();
            }
            "--db" => {
                i += 1;
                db_path = args[i].clone();
            }
            other => {
                eprintln!("未知参数: {other}（支持 --listen ADDR --db PATH）");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let store = Arc::new(Store::open(&db_path)?);
    if store.is_empty()? {
        engine::seed_fixture(&store)?;
        println!("空数据库，已写入固定 fixture（事件/门系/补偿/变换 v1）。");
    }

    let app = web::router(store);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("流式门谱台 已启动：http://{listen} （数据库 {db_path}）");
    axum::serve(listener, app).await?;
    Ok(())
}
