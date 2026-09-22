use axum::serve;
use flow_gate_bench::{bootstrap, web};
use std::{net::SocketAddr, sync::Arc};
use tokio::net::TcpListener;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut listen = "127.0.0.1:5582".to_string();
    let mut db_path = "flow_gate_bench.sqlite".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                i += 1;
                listen = args.get(i).cloned().expect("--listen 需要地址");
            }
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned().expect("--db 需要路径");
            }
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let db = Arc::new(bootstrap(&db_path).unwrap_or_else(|e| {
        eprintln!("初始化失败: {e}");
        std::process::exit(1);
    }));
    let addr: SocketAddr = listen.parse().unwrap_or_else(|e| {
        eprintln!("无法解析监听地址 {listen}: {e}");
        std::process::exit(2);
    });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async move {
        let listener = TcpListener::bind(addr).await.expect("绑定端口失败");
        println!("流式门谱台已启动: http://{addr}");
        let app = web::router(db);
        serve(listener, app.into_make_service())
            .await
            .expect("服务器错误");
    });
}
