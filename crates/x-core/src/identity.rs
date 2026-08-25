//! Build identity reported by the version endpoint.

/// The canonical service name.
pub const SERVICE_NAME: &str = "ratatoskr-x";

/// The crate version this binary was built from.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The source revision, when the build environment provides `RATATOSKR_GIT_SHA`.
pub const GIT_SHA: &str = match option_env!("RATATOSKR_GIT_SHA") {
    Some(sha) => sha,
    None => "unknown",
};

/// The compiler version this binary was built with.
pub const RUST_VERSION: &str = env!("CARGO_PKG_RUST_VERSION");
