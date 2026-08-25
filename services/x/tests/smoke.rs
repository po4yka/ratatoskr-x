//! The compiled binary starts against a real database, serves the process-state endpoints,
//! takes SIGTERM, and stops cleanly with success.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::Duration;

use x_persistence::test_support::TestDatabase;

const HOST: &str = "127.0.0.1";

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

#[tokio::test]
async fn service_serves_health_endpoints_until_sigterm() {
    let database = TestDatabase::create().await.expect("a disposable database");
    let name = database.name().to_owned();
    let base = TestDatabase::admin_url()
        .rsplit_once('/')
        .map(|(base, _)| base.to_owned())
        .expect("the url has a database path");
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_ratatoskr-x"))
        .env("RATATOSKR__ADMIN__LISTEN_ADDR", format!("{HOST}:{port}"))
        .env("RATATOSKR__DATABASE__URL", format!("{base}/{name}"))
        .env("RATATOSKR__TELEMETRY__LOG_FORMAT", "json")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary launches");

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
}
