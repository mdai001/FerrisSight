use clap::{Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "ferrissight",
    version,
    about = "Rust camera gateway and lightweight NVR"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(
            long,
            env = "FERRISSIGHT_BIND",
            hide_env_values = true,
            default_value = "127.0.0.1:8080"
        )]
        bind: SocketAddr,
        /// Root containing UTC recordings and the local storage control database.
        #[arg(
            long,
            env = "FERRISSIGHT_DATA_DIR",
            hide_env_values = true,
            default_value = "recordings"
        )]
        data_dir: PathBuf,
        #[arg(long, default_value_t = 72)]
        retention_hours: u32,
        #[arg(long, default_value_t=100 * 1024 * 1024 * 1024_u64)]
        max_storage_bytes: u64,
        /// Explicitly allow dropping pending/failed recordings under capacity pressure.
        #[arg(long)]
        allow_unuploaded_eviction: bool,
        /// Reserved for a future uploader; does not initiate cloud traffic.
        #[arg(long)]
        upload_enabled: bool,
        #[arg(
            long,
            env = "FERRISSIGHT_LOG_LEVEL",
            hide_env_values = true,
            default_value = "info"
        )]
        log_level: tracing::level_filters::LevelFilter,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // clap's normal parse errors echo input. Keep invalid configuration errors fixed.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return std::process::ExitCode::SUCCESS;
        }
        Err(_) => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "gateway invalid_configuration; use --help"
            );
            return std::process::ExitCode::from(2);
        }
    };
    let Command::Serve {
        bind,
        data_dir,
        retention_hours,
        max_storage_bytes,
        allow_unuploaded_eviction,
        upload_enabled,
        log_level,
    } = cli.command;
    let storage_config = ferrissight::storage::local::StorageConfig {
        retention_hours,
        max_storage_bytes,
        protect_unuploaded: !allow_unuploaded_eviction,
        upload_enabled,
    };
    if storage_config.validate().is_err() {
        eprintln!("gateway invalid_configuration; use --help");
        return std::process::ExitCode::from(2);
    }
    // Filter dependencies out: their future transport logs may include network data.
    let filter = tracing_subscriber::filter::Targets::new().with_target("ferrissight", log_level);
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .without_time()
                .with_target(false),
        )
        .with(filter)
        .init();
    let shutdown = match shutdown_signal() {
        Ok(shutdown) => shutdown,
        Err(_) => {
            tracing::error!(service = "gateway", event = "signal_setup_failed");
            return std::process::ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(listener) => listener,
        Err(_) => {
            tracing::error!(service = "gateway", event = "startup_failed");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (storage_stop, stopped) = tokio::sync::watch::channel(false);
    let (storage, storage_task) =
        ferrissight::storage::service::StorageService::start(data_dir, storage_config, stopped);
    tracing::info!(service = "gateway", event = "started");
    let result = ferrissight::server::serve_with_storage(listener, shutdown, storage).await;
    let _ = storage_stop.send(true);
    let _ = storage_task.await;
    if result.is_err() {
        tracing::error!(service = "gateway", event = "server_failed");
        return std::process::ExitCode::FAILURE;
    }
    tracing::info!(service = "gateway", event = "stopped");
    std::process::ExitCode::SUCCESS
}
fn shutdown_signal() -> std::io::Result<impl std::future::Future<Output = ()> + Send> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    Ok(async move {
        #[cfg(unix)]
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    })
}
