//! A configured extra CA is added to the platform verifier's roots; it never replaces them
//! (XR-021 CONTRACTS.md section R3-15 rule 1).
//!
//! The platform verifier reads the host trust store, which a test must not change. On targets
//! whose store honours `SSL_CERT_FILE` (Linux, where CI runs) the test starts a child process
//! of this binary whose "host store" is one throwaway CA and gives its resolver a second CA as
//! the extra anchor; both servers must then be trusted. Apple targets read the Security framework
//! and cannot be redirected, so the proof is not compiled there.

#![cfg(not(target_vendor = "apple"))]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::process::Command;
use std::time::Duration;

use x_sync::test_support::TlsFake;
use x_sync::{AppBearerResolver, PublicPostFailure, PublicPostResolver};

const POST_ID: &str = "1900000000000000001";
const BODY: &str = r#"{"data":[{"id":"1900000000000000001","text":"A public post","author_id":"42",
"created_at":"2026-08-16T09:30:00.120Z"}],
"includes":{"users":[{"id":"42","name":"Ada Example","username":"ada"}]}}"#;
const PLATFORM_URL: &str = "X_PROBE_PLATFORM_URL";
const EXTRA_URL: &str = "X_PROBE_EXTRA_URL";
const EXTRA_CA_PEM: &str = "X_PROBE_EXTRA_CA_PEM";
const CHILD: &str = "probe_inside_a_process_whose_host_store_trusts_one_ca";
const MARKER_FILE: &str = "X_PROBE_MARKER_FILE";

fn resolver(base_url: &str, extra_ca_pem: Option<&[u8]>) -> AppBearerResolver {
    AppBearerResolver::with_timeouts(
        base_url,
        "app-only-test-token",
        Duration::from_secs(2),
        Duration::from_secs(5),
        extra_ca_pem,
    )
    .expect("the resolver builds")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_extra_ca_does_not_replace_the_platform_roots() {
    let platform = TlsFake::start(BODY)
        .await
        .expect("the platform-trusted fake");
    let extra = TlsFake::start(BODY).await.expect("the extra-CA fake");
    let directory =
        std::env::temp_dir().join(format!("ratatoskr-x-platform-roots-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    let platform_store = directory.join("host-store.pem");
    let marker = directory.join("probe-passed");
    std::fs::write(&platform_store, platform.ca_pem()).expect("the host store file");

    let output = Command::new(std::env::current_exe().expect("the test binary path"))
        .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env("SSL_CERT_FILE", &platform_store)
        .env_remove("SSL_CERT_DIR")
        .env(PLATFORM_URL, platform.base_url())
        .env(EXTRA_URL, extra.base_url())
        .env(EXTRA_CA_PEM, extra.ca_pem())
        .env(MARKER_FILE, &marker)
        .output()
        .expect("the probe process runs");
    let reached_the_end = marker.is_file();
    let _ = std::fs::remove_dir_all(&directory);

    assert!(
        output.status.success() && reached_the_end,
        "the probe failed (reached the end: {reached_the_end}):\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The body of the probe process; a no-op in an ordinary run, because the environment that makes
/// it meaningful is set only by the test above.
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "the probe reads the environment its parent test sets"
)]
async fn probe_inside_a_process_whose_host_store_trusts_one_ca() {
    let (Ok(platform_url), Ok(extra_url), Ok(extra_ca), Ok(marker)) = (
        std::env::var(PLATFORM_URL),
        std::env::var(EXTRA_URL),
        std::env::var(EXTRA_CA_PEM),
        std::env::var(MARKER_FILE),
    ) else {
        return;
    };

    // Control: without the extra CA the second server is not trusted.
    assert_eq!(
        resolver(&extra_url, None)
            .resolve(POST_ID)
            .await
            .expect_err("the extra CA is unknown to the host store"),
        PublicPostFailure::Transient
    );
    // The host-store CA still works once an extra CA is configured: merged, not replaced.
    let with_extra = Some(extra_ca.as_bytes());
    resolver(&platform_url, with_extra)
        .resolve(POST_ID)
        .await
        .expect("the platform root is still trusted beside the extra CA");
    resolver(&extra_url, with_extra)
        .resolve(POST_ID)
        .await
        .expect("the extra CA is trusted");
    std::fs::write(marker, "passed").expect("the marker is written");
}
