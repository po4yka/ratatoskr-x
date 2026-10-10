//! The compiled binary starts against a real database and broker, serves the process-state
//! endpoints, takes SIGTERM and stops cleanly, refuses to start without the public-capture
//! credential, and leaves readiness the moment a bus task stops.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use async_nats::jetstream;
use x_persistence::test_support::TestDatabase;

const HOST: &str = "127.0.0.1";
const REPORTS_DURABLE: &str = "ratatoskr_x_extractor_reports";

/// Grabs an ephemeral port by binding and immediately releasing it.
fn free_port() -> u16 {
    std::net::TcpListener::bind((HOST, 0))
        .expect("a probe listener")
        .local_addr()
        .expect("a local address")
        .port()
}

/// Waits until the port accepts connections, bounded by ten seconds.
fn wait_for_port(port: u16) -> bool {
    for _ in 0..100 {
        if TcpStream::connect((HOST, port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Sends one HTTP request over a fresh connection and returns the raw response text.
fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect((HOST, port)).expect("the service accepts connections");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("the request is written");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("the response is readable");
    response
}

/// The status line of `path`, or `None` while the service is not reachable.
fn try_status(port: u16, path: &str) -> Option<String> {
    let mut stream = TcpStream::connect((HOST, port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    response.lines().next().map(str::to_owned)
}

/// Waits for the child to exit by itself, killing it after `limit`.
fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("the child can be polled") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// What one service instance needs from the test broker.
struct Broker {
    nats_url: String,
    seed_path: PathBuf,
    token_path: PathBuf,
    consumer: String,
}

impl Broker {
    fn files(&self) {
        let seed = nkeys::KeyPair::new_user().seed().expect("a test NKey seed");
        std::fs::write(&self.seed_path, seed).expect("the test NKey seed is written");
        std::fs::write(&self.token_path, "app-only-smoke-token\n")
            .expect("the test bearer token is written");
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.seed_path);
        let _ = std::fs::remove_file(&self.token_path);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the smoke binary chooses its isolated JetStream endpoint"
)]
fn nats_url() -> String {
    std::env::var("X_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:14224".to_owned())
}

/// Provisions what Edge provisions in production: both streams, this instance's command durable
/// and the shared extractor-report durable.
async fn provision(url: &str, consumer: &str) -> jetstream::stream::Stream {
    let context = jetstream::new(
        async_nats::connect(url)
            .await
            .expect("the test broker connects"),
    );
    let commands = context
        .get_or_create_stream(jetstream::stream::Config {
            name: "ratatoskr_commands".to_owned(),
            subjects: vec!["cmd.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the command stream exists");
    commands
        .get_or_create_consumer(
            consumer,
            jetstream::consumer::pull::Config {
                durable_name: Some(consumer.to_owned()),
                filter_subject: "cmd.x.capture.requested.v1".to_owned(),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ..jetstream::consumer::pull::Config::default()
            },
        )
        .await
        .expect("the X command durable exists");
    let events = context
        .get_or_create_stream(jetstream::stream::Config {
            name: "ratatoskr_events".to_owned(),
            subjects: vec!["evt.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the event stream exists");
    events
        .get_or_create_consumer(
            REPORTS_DURABLE,
            jetstream::consumer::pull::Config {
                durable_name: Some(REPORTS_DURABLE.to_owned()),
                filter_subject: "evt.platform.operation.reported.v1".to_owned(),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ack_wait: Duration::from_secs(30),
                ..jetstream::consumer::pull::Config::default()
            },
        )
        .await
        .expect("the extractor-report durable exists");
    commands
}

fn spawn_service(database: &TestDatabase, broker: &Broker, port: u16, with_token: bool) -> Child {
    let base = TestDatabase::admin_url()
        .rsplit_once('/')
        .map(|(base, _)| base.to_owned())
        .expect("the url has a database path");
    let mut command = Command::new(env!("CARGO_BIN_EXE_ratatoskr-x"));
    command
        .env("RATATOSKR__ADMIN__LISTEN_ADDR", format!("{HOST}:{port}"))
        .env(
            "RATATOSKR__DATABASE__URL",
            format!("{base}/{}", database.name()),
        )
        .env("RATATOSKR__TELEMETRY__LOG_FORMAT", "json")
        .env("RATATOSKR__BUS__URL", &broker.nats_url)
        .env("RATATOSKR__BUS__NKEY_SEED_PATH", &broker.seed_path)
        .env("RATATOSKR__BUS__CONSUMER_NAME", &broker.consumer)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if with_token {
        command.env(
            "RATATOSKR__PUBLIC_CAPTURE__BEARER_TOKEN_PATH",
            &broker.token_path,
        );
    }
    command.spawn().expect("the binary launches")
}

fn broker(label: &str) -> Broker {
    let suffix = uuid::Uuid::now_v7().simple();
    Broker {
        nats_url: nats_url(),
        seed_path: std::env::temp_dir().join(format!("ratatoskr-x-smoke-{suffix}.nkey")),
        token_path: std::env::temp_dir().join(format!("ratatoskr-x-smoke-{suffix}.token")),
        consumer: format!("x_browser_capture_{label}_{suffix}"),
    }
}

#[tokio::test]
async fn service_serves_health_endpoints_until_sigterm() {
    let database = TestDatabase::create().await.expect("a disposable database");
    let broker = broker("health");
    provision(&broker.nats_url, &broker.consumer).await;
    broker.files();
    let port = free_port();
    let child = spawn_service(&database, &broker, port, true);

    assert!(wait_for_port(port), "the service starts listening");

    let live = http_get(port, "/health/live");
    assert!(
        live.starts_with("HTTP/1.1 200"),
        "liveness answers 200:\n{live}"
    );
    assert!(
        live.contains("\"live\""),
        "liveness reports its state:\n{live}"
    );
    let ready = http_get(port, "/health/ready");
    assert!(
        ready.starts_with("HTTP/1.1 200") && ready.contains("\"bus\""),
        "readiness names the bus check:\n{ready}"
    );

    let version = http_get(port, "/version");
    assert!(
        version.contains("ratatoskr-x"),
        "version names the service:\n{version}"
    );

    let metrics = http_get(port, "/metrics");
    assert!(
        metrics
            .to_ascii_lowercase()
            .contains("text/plain; version=0.0.4"),
        "metrics serve prometheus exposition:\n{metrics}"
    );

    let pid = child.id().to_string();
    Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("SIGTERM is delivered");
    let output = child.wait_with_output().expect("the process waits");
    assert!(
        output.status.success(),
        "graceful shutdown exits zero: {:?}\nstderr:\n{}\nstdout:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    database
        .cleanup()
        .await
        .expect("cleanup drops the database");
    broker.cleanup();
}

#[tokio::test]
async fn a_bus_without_the_public_capture_credential_exits_with_a_configuration_error() {
    let database = TestDatabase::create().await.expect("a disposable database");
    let broker = broker("refusal");
    provision(&broker.nats_url, &broker.consumer).await;
    broker.files();
    let port = free_port();
    let mut child = spawn_service(&database, &broker, port, false);

    let status = wait_for_exit(&mut child, Duration::from_secs(20));
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let listening = TcpStream::connect((HOST, port)).is_ok();

    database
        .cleanup()
        .await
        .expect("cleanup drops the database");
    broker.cleanup();
    let status = status.expect("the service refuses to start instead of running");
    assert_eq!(status.code(), Some(78), "EX_CONFIG:\n{stderr}");
    assert!(
        stderr.contains("public_capture.bearer_token_path"),
        "the refusal names the missing setting:\n{stderr}"
    );
    assert!(!listening, "readiness was never offered");
}

#[tokio::test]
async fn readiness_goes_false_when_a_bus_task_stops() {
    let database = TestDatabase::create().await.expect("a disposable database");
    let broker = broker("lifecycle");
    let commands = provision(&broker.nats_url, &broker.consumer).await;
    broker.files();
    let port = free_port();
    let mut child = spawn_service(&database, &broker, port, true);

    assert!(wait_for_port(port), "the service starts listening");
    let ready = Instant::now();
    while try_status(port, "/health/ready").as_deref() != Some("HTTP/1.1 200 OK") {
        assert!(
            ready.elapsed() < Duration::from_secs(15),
            "the service becomes ready"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Deleting the Edge-provisioned durable stops the command consumer.
    commands
        .delete_consumer(&broker.consumer)
        .await
        .expect("the durable is deleted");

    let flipped = Instant::now();
    loop {
        match try_status(port, "/health/ready").as_deref() {
            Some("HTTP/1.1 503 Service Unavailable") => break,
            _ => assert!(
                flipped.elapsed() < Duration::from_secs(20),
                "readiness never went false after the consumer stopped"
            ),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = wait_for_exit(&mut child, Duration::from_secs(30))
        .expect("the service exits after a bus task stopped");
    assert!(
        !status.success(),
        "a stopped bus task is a failure exit, got {status:?}"
    );

    database
        .cleanup()
        .await
        .expect("cleanup drops the database");
    broker.cleanup();
}
