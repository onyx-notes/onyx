//! The Onyx sync server binary.
//!
//! Usage: `onyx-server [--data-dir DIR] [--listen ADDR]`
//! TLS is the reverse proxy's job in the recommended docker-compose;
//! built-in rustls is on the roadmap for true single-container setups.

use std::net::SocketAddr;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut data_dir = PathBuf::from("./data");
    let mut listen: SocketAddr = "0.0.0.0:7677".parse().expect("static address parses");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                data_dir = PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--data-dir requires a value");
                    std::process::exit(2);
                }));
            }
            "--listen" => {
                let value = args.next().unwrap_or_default();
                listen = value.parse().unwrap_or_else(|_| {
                    eprintln!("invalid --listen address: {value}");
                    std::process::exit(2);
                });
            }
            other => {
                eprintln!("unknown argument: {other}");
                eprintln!("usage: onyx-server [--data-dir DIR] [--listen ADDR]");
                std::process::exit(2);
            }
        }
    }

    let state = onyx_server::state(&data_dir).unwrap_or_else(|error| {
        eprintln!("failed to open database: {error}");
        std::process::exit(1);
    });
    let app = onyx_server::app(state);

    tracing::info!(%listen, data_dir = %data_dir.display(), "onyx-server starting");
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .unwrap_or_else(|error| {
            eprintln!("failed to bind {listen}: {error}");
            std::process::exit(1);
        });
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server run");
}
