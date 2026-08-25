//! Telemetry behaves like the `service-bootstrap` spec says it does.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use metrics::{Level, Metadata, Recorder as _};
use tracing_subscriber::fmt::MakeWriter;
use x_core::config::LogFormat;
use x_telemetry::error::TelemetryError;

/// A shared buffer posing as stdout, so JSON emission is assertable.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn snapshot(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = CaptureWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter(self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

struct CaptureWriter<'a>(MutexGuard<'a, Vec<u8>>);

impl std::io::Write for CaptureWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn unparsable_filter_returns_typed_error() {
    let error = x_telemetry::build(LogFormat::Json, "lvl^[[")
        .expect_err("an unparsable filter must fail startup");
    assert!(matches!(error, TelemetryError::Filter(_)), "got: {error:?}");
}

#[test]
fn json_events_render_message_and_level_to_captured_writer() {
    let capture = Capture::default();
    let telemetry =
        x_telemetry::build_with_writer(LogFormat::Json, "info", capture.clone()).expect("build");
    let dispatcher = telemetry.subscriber.clone();
    tracing::subscriber::with_default(dispatcher, || {
        tracing::info!("bootstrap smoke");
    });
    let captured = String::from_utf8(capture.snapshot()).expect("utf8");
    let event: serde_json::Value =
        serde_json::from_str(captured.lines().next().expect("at least one emitted line"))
            .expect("each event is one JSON object");
    assert_eq!(event["fields"]["message"], "bootstrap smoke");
    assert_eq!(event["level"], "INFO");
}

#[test]
fn prometheus_recorder_renders_described_counter() {
    let telemetry = x_telemetry::build(LogFormat::Json, "info").expect("build");
    let key = metrics::Key::from_name("ratatoskr_x_bootstrap_total");
    let metadata = Metadata::new(module_path!(), Level::INFO, None);
    let counter = telemetry.recorder.register_counter(&key, &metadata);
    counter.increment(1);
    let rendered = telemetry.metrics_render();
    assert!(
        rendered.contains("ratatoskr_x_bootstrap_total"),
        "rendered exposition must carry the counter:\n{rendered}"
    );
}
