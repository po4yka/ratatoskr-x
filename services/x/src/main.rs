//! The `ratatoskr-x` service binary: configuration, telemetry, database, admin listener.

use std::process::ExitCode;
use std::sync::Arc;

use ratatoskr_x::bootstrap::BootstrapError;
use x_http::admin::{RuntimeState, admin_router};
use x_persistence::database::Database;

/// Loads configuration, starts telemetry and the database, serves the admin router, and drains
/// on SIGINT or SIGTERM. Every failure returns through one typed path.
///
/// # Errors
/// Returns the failing subsystem's typed error; the caller maps it to an exit status.
async fn run() -> Result<(), BootstrapError> {
    let config = x_core::config::load()?;

    let telemetry = x_telemetry::build(config.telemetry.log_format, &config.telemetry.log_filter)?;
    let render_handle = telemetry.metrics_handle.clone();
    let guard = telemetry.install()?;
    tracing::info!(
        service = x_core::identity::SERVICE_NAME,
        version = x_core::identity::VERSION,
        git_sha = x_core::identity::GIT_SHA,
        "starting"
    );

    let database = Database::connect(&config.database.url, config.database.max_connections).await?;
    database.apply_schema().await?;

    let nkey_seed =
        std::fs::read_to_string(&config.bus.nkey_seed_path).map_err(BootstrapError::NatsSeed)?;
    let nats_client = async_nats::ConnectOptions::with_nkey(nkey_seed.trim().to_owned())
        .connect(&config.bus.url)
        .await
        .map_err(|error| BootstrapError::Nats(error.to_string()))?;
    let jetstream = async_nats::jetstream::new(nats_client.clone());
    x_capture::ensure_browser_consumer(
        &jetstream,
        &config.bus.stream_name,
        &config.bus.consumer_name,
    )
    .await
    .map_err(|error| BootstrapError::Nats(error.to_string()))?;
    let consumer_database = database.clone();
    let consumer_stream = config.bus.stream_name.clone();
    let consumer_name = config.bus.consumer_name.clone();
    let consumer = tokio::spawn(async move {
        if let Err(error) = x_capture::consume_browser_commands(
            &jetstream,
            &consumer_database,
            &consumer_stream,
            &consumer_name,
            std::future::pending(),
        )
        .await
        {
            tracing::error!(%error, "the X browser-capture consumer stopped");
        }
    });

    let state = Arc::new(RuntimeState::new());
    state.mark_database_ready();

    let listener = tokio::net::TcpListener::bind(&config.admin.listen_addr)
        .await
        .map_err(BootstrapError::Listener)?;
    state.set_ready();
    let bound = config.admin.listen_addr.clone();
    tracing::info!(addr = %bound, "the admin listener is bound");

    let app = admin_router(Arc::clone(&state), move || render_handle.render());

    #[cfg(not(unix))]
    let shutdown = {
        let state = Arc::clone(&state);
        async move {
            let _ = tokio::signal::ctrl_c().await;
            state.begin_drain();
            tracing::info!("shutdown started");
        }
    };
    #[cfg(unix)]
    let shutdown = {
        let state = Arc::clone(&state);
        async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                () = terminate() => {},
            }
            state.begin_drain();
            tracing::info!("shutdown started");
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(BootstrapError::Listener)?;
    consumer.abort();
    let _ = consumer.await;
    drop(nats_client);
    guard.shutdown();
    tracing::info!("shutdown complete");
    Ok(())
}

/// Resolves when SIGTERM arrives.
#[cfg(unix)]
async fn terminate() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}

/// Runs the async runtime and maps the outcome onto a process exit status.
fn main() -> ExitCode {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Err(_) => ExitCode::FAILURE,
        Ok(runtime) => match runtime.block_on(run()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("ratatoskr-x: {}", error.report());
                ExitCode::from(error.exit_code())
            }
        },
    }
}
