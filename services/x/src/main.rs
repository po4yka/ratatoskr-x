//! The `ratatoskr-x` service binary: configuration, telemetry, database, bus workers, admin
//! listener.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_x::bootstrap::BootstrapError;
use ratatoskr_x::extractor_reports;
use tokio::sync::watch;
use tokio::task::JoinSet;
use x_budget::gate::SystemClock;
use x_core::config::XConfig;
use x_http::admin::{RuntimeState, admin_router};
use x_persistence::database::Database;
use x_sync::{AppBearerResolver, CapturePolicy, PublicCaptureWorker};

/// How long the admin listener keeps answering `503` after a bus task stopped, so a readiness
/// probe sees the failure before the process leaves.
const FAILURE_GRACE: Duration = Duration::from_secs(2);

/// How long the remaining bus tasks get to finish after shutdown started.
const TASK_DRAIN: Duration = Duration::from_secs(10);

/// A bus task's name and the reason it stopped, if it stopped by failing.
type TaskExit = (&'static str, Result<(), String>);

/// Reads the app-only bearer token file; the token is never logged.
fn read_bearer_token(path: &str) -> Result<String, BootstrapError> {
    let token = std::fs::read_to_string(path).map_err(BootstrapError::BearerToken)?;
    let token = token.trim();
    if token.is_empty() {
        return Err(BootstrapError::BearerTokenEmpty);
    }
    Ok(token.to_owned())
}

/// Loads configuration, starts telemetry and the database, serves the admin router, and drains
/// on SIGINT, SIGTERM or the first stopped bus task. Every failure returns through one typed path.
///
/// # Errors
/// Returns the failing subsystem's typed error; the caller maps it to an exit status.
async fn run() -> Result<(), BootstrapError> {
    let config = x_core::config::load()?;
    // The service always runs a broker consumer, so it must also be able to finish the work: no
    // credential, no start (CONTRACTS.md S02 rule 5).
    let bearer_token = read_bearer_token(config.require_public_capture_token_path()?)?;

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
    let context = jetstream::new(nats_client.clone());
    verify_durables(&config, &context).await?;

    let state = Arc::new(RuntimeState::new());
    state.mark_database_ready();
    let (stop_tx, stop_rx) = watch::channel(false);
    let tasks = spawn_bus_tasks(&config, &database, &context, &bearer_token, &stop_rx)?;
    state.set_bus_ready(true);

    let listener = tokio::net::TcpListener::bind(&config.admin.listen_addr)
        .await
        .map_err(BootstrapError::Listener)?;
    state.set_ready();
    tracing::info!(addr = %config.admin.listen_addr, "the admin listener is bound");

    let app = admin_router(Arc::clone(&state), move || render_handle.render());
    let tasks = Arc::new(tokio::sync::Mutex::new(tasks));
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let shutdown = {
        let state = Arc::clone(&state);
        let tasks = Arc::clone(&tasks);
        let failure = Arc::clone(&failure);
        async move {
            tokio::select! {
                () = wait_for_signal() => {}
                exit = next_exit(&tasks) => {
                    // Any bus task returning before shutdown is a fault: never stay ready.
                    state.set_bus_ready(false);
                    let reason = describe_exit(exit);
                    tracing::error!(%reason, "a bus task stopped; shutting down");
                    if let Ok(mut slot) = failure.lock() {
                        *slot = Some(reason);
                    }
                    tokio::time::sleep(FAILURE_GRACE).await;
                }
            }
            state.begin_drain();
            let _ = stop_tx.send(true);
            tracing::info!("shutdown started");
        }
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(BootstrapError::Listener)?;

    drain_tasks(&tasks).await;
    drop(nats_client);
    guard.shutdown();
    tracing::info!("shutdown complete");
    let stopped = failure.lock().ok().and_then(|slot| slot.clone());
    stopped.map_or(Ok(()), |reason| Err(BootstrapError::BusTask(reason)))
}

/// Verifies the Edge-provisioned durables before the service reports anything.
async fn verify_durables(
    config: &XConfig,
    context: &jetstream::Context,
) -> Result<(), BootstrapError> {
    x_capture::ensure_browser_consumer(context, &config.bus.stream_name, &config.bus.consumer_name)
        .await
        .map_err(|error| BootstrapError::Nats(error.to_string()))?;
    extractor_reports::ensure_reports_consumer(
        context,
        &config.bus.events_stream_name,
        extractor_reports::DURABLE,
    )
    .await
    .map_err(|error| BootstrapError::Nats(error.to_string()))
}

/// Starts the consumers, the public capture worker and the outbox relay.
fn spawn_bus_tasks(
    config: &XConfig,
    database: &Database,
    context: &jetstream::Context,
    bearer_token: &str,
    stop: &watch::Receiver<bool>,
) -> Result<JoinSet<TaskExit>, BootstrapError> {
    let resolver = AppBearerResolver::new(&config.public_capture.api_base_url, bearer_token)
        .map_err(|error| BootstrapError::PublicCapture(error.to_string()))?;
    let worker = PublicCaptureWorker::new(
        database.clone(),
        Arc::new(resolver),
        Arc::new(SystemClock),
        CapturePolicy::new(
            config.public_capture.max_attempts,
            config.public_capture.batch_size,
        ),
    );
    let relay = x_capture::relay::OutboxRelay::new(
        database.clone(),
        Arc::new(x_capture::relay::JetStreamPublisher::new(context.clone())),
    );
    let poll_interval = Duration::from_secs(config.public_capture.poll_interval_seconds);
    let mut tasks = JoinSet::new();

    let (commands, stream, durable) = (
        context.clone(),
        config.bus.stream_name.clone(),
        config.bus.consumer_name.clone(),
    );
    let (db, signal) = (database.clone(), stopped(stop));
    tasks.spawn(async move {
        let result =
            x_capture::consume_browser_commands(&commands, &db, &stream, &durable, signal).await;
        (
            "browser_capture_consumer",
            result.map(drop).map_err(|e| e.to_string()),
        )
    });

    let (reports, stream) = (context.clone(), config.bus.events_stream_name.clone());
    let (db, signal) = (database.clone(), stopped(stop));
    tasks.spawn(async move {
        let result = extractor_reports::consume_extractor_reports(
            &reports,
            &db,
            &stream,
            extractor_reports::DURABLE,
            signal,
        )
        .await;
        (
            "extractor_report_consumer",
            result.map(drop).map_err(|e| e.to_string()),
        )
    });

    let signal = stopped(stop);
    tasks.spawn(async move {
        let mut signal = Box::pin(signal);
        let result = loop {
            if let Err(error) = worker.run_due_once().await {
                break Err(error.to_string());
            }
            tokio::select! {
                biased;
                () = &mut signal => break Ok(()),
                () = tokio::time::sleep(poll_interval) => {}
            }
        };
        ("public_capture_worker", result)
    });

    let signal = stopped(stop);
    tasks.spawn(async move {
        (
            "outbox_relay",
            relay.run(signal).await.map_err(|e| e.to_string()),
        )
    });
    Ok(tasks)
}

/// Resolves once shutdown was requested.
fn stopped(stop: &watch::Receiver<bool>) -> impl std::future::Future<Output = ()> + Send + 'static {
    let mut stop = stop.clone();
    async move {
        let _ = stop.wait_for(|requested| *requested).await;
    }
}

/// Waits for the next bus task to return; pends forever when none is left.
async fn next_exit(
    tasks: &tokio::sync::Mutex<JoinSet<TaskExit>>,
) -> Result<TaskExit, tokio::task::JoinError> {
    match tasks.lock().await.join_next().await {
        Some(exit) => exit,
        None => std::future::pending().await,
    }
}

fn describe_exit(exit: Result<TaskExit, tokio::task::JoinError>) -> String {
    match exit {
        Ok((name, Ok(()))) => format!("{name} returned"),
        Ok((name, Err(error))) => format!("{name} failed: {error}"),
        Err(error) => format!("a bus task panicked or was cancelled: {error}"),
    }
}

/// Gives the remaining bus tasks a bounded time to finish after shutdown started.
async fn drain_tasks(tasks: &tokio::sync::Mutex<JoinSet<TaskExit>>) {
    let mut tasks = tasks.lock().await;
    let finished = tokio::time::timeout(TASK_DRAIN, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    if finished.is_err() {
        tracing::warn!("bus tasks did not stop in time; aborting them");
        tasks.abort_all();
    }
}

/// Resolves on SIGINT or SIGTERM.
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            () = terminate() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
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
