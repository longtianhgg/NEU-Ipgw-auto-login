use clap::Parser;
use neu_login::{
    store::Store,
    web::{AppState, router},
};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "东北大学校园网 Web 管理服务")]
struct Args {
    #[arg(long, env = "NEU_LISTEN", default_value = "0.0.0.0:11099")]
    listen: SocketAddr,
    #[arg(long, env = "NEU_DATA_DIR", default_value = "data")]
    data_dir: PathBuf,
    #[arg(long, env = "NEU_IMAGE_DIR")]
    image_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "neu_login=info".into()),
        )
        .init();
    let args = Args::parse();
    let store = Store::open(&args.data_dir)?;
    let app = AppState::new(store, args.image_dir)?;
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!(listen=%args.listen,"NEU 校园网服务已启动");
    let monitor = app.start_monitor();
    let result = axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown())
        .await;
    monitor.abort();
    result?;
    Ok(())
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("无法监听终止信号");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
}
