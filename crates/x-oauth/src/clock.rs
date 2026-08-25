//! The injected time source every timestamp-bearing decision reads.

/// A monotonic-enough wall clock the flows read instead of the system directly.
pub trait Clock: std::fmt::Debug + Send + Sync {
    /// The current instant.
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

/// The production clock reading the system time.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}
