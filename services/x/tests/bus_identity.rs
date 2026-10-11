//! The service reaches its Edge-provisioned durables through its real connection function under
//! the X stanza of the deployed ACL (XR-021 CONTRACTS.md section R3-01). That stanza lets the
//! identity subscribe only to `_INBOX_x.>`, so a connection without the inbox prefix never
//! receives a JetStream reply.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_x::bus::connect_bus;
use ratatoskr_x::extractor_reports;
use x_core::config::BusConfig;

const HOST: &str = "127.0.0.1";
const COMMANDS_STREAM: &str = "ratatoskr_commands";
const EVENTS_STREAM: &str = "ratatoskr_events";
const COMMANDS_DURABLE: &str = "ratatoskr_x_browser_capture";
const REPORT_SUBJECT: &str = "evt.platform.operation.reported.v1";

/// A private `nats-server` with `JetStream` whose only users are an administrator standing in for
/// Edge and the X stanza shipped in `deploy/nats/identity.conf`.
struct Enforcing {
    child: Child,
    port: u16,
    directory: PathBuf,
}

impl Enforcing {
    fn start(x_public: &str, admin_public: &str) -> Self {
        let stanza = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/nats/identity.conf"
        ))
        .expect("deploy/nats/identity.conf is shipped");
        let stanza: String = stanza
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("nkey:") {
                    format!("    nkey: {x_public}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let port = std::net::TcpListener::bind((HOST, 0))
            .expect("a probe listener")
            .local_addr()
            .expect("a local address")
            .port();
        let directory = std::env::temp_dir().join(format!(
            "ratatoskr-x-bus-identity-{}",
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&directory).expect("a scratch directory");
        let config = format!(
            "listen: {HOST}:{port}\njetstream {{ store_dir: \"{store}\" }}\nauthorization {{\n  \
             users = [\n{stanza}\n    {{ nkey: {admin_public} }}\n  ]\n}}\n",
            store = directory.join("js").display(),
        );
        let config_path = directory.join("nats.conf");
        std::fs::write(&config_path, config).expect("the broker configuration is written");
        let child = Command::new("nats-server")
            .arg("-c")
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("nats-server is on PATH (CI copies it out of the NATS image)");
        let broker = Self {
            child,
            port,
            directory,
        };
        let listening = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            TcpStream::connect((HOST, port)).is_ok()
        });
        assert!(listening, "the enforcing broker starts listening");
        broker
    }

    fn url(&self) -> String {
        format!("nats://{HOST}:{}", self.port)
    }
}

impl Drop for Enforcing {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Creates what Edge creates: both streams and the two durables the X identity may use.
async fn provision(url: &str, admin_seed: &str) {
    let admin = async_nats::ConnectOptions::with_nkey(admin_seed.to_owned())
        .connect(url)
        .await
        .expect("the provisioning identity connects");
    let context = jetstream::new(admin);
    let commands = context
        .get_or_create_stream(jetstream::stream::Config {
            name: COMMANDS_STREAM.to_owned(),
            subjects: vec!["cmd.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the command stream exists");
    commands
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some(COMMANDS_DURABLE.to_owned()),
            filter_subject: "cmd.x.capture.requested.v1".to_owned(),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ..jetstream::consumer::pull::Config::default()
        })
        .await
        .expect("the command durable exists");
    let events = context
        .get_or_create_stream(jetstream::stream::Config {
            name: EVENTS_STREAM.to_owned(),
            subjects: vec!["evt.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the event stream exists");
    events
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some(extractor_reports::DURABLE.to_owned()),
            filter_subject: REPORT_SUBJECT.to_owned(),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ack_wait: Duration::from_secs(30),
            ..jetstream::consumer::pull::Config::default()
        })
        .await
        .expect("the report durable exists");
}

#[tokio::test]
async fn the_service_completes_jetstream_requests_under_its_inbox_prefix() {
    let x_keys = nkeys::KeyPair::new_user();
    let admin_keys = nkeys::KeyPair::new_user();
    let broker = Enforcing::start(&x_keys.public_key(), &admin_keys.public_key());
    provision(&broker.url(), &admin_keys.seed().expect("an admin seed")).await;

    let seed_path = broker.directory.join("x.nkey");
    std::fs::write(&seed_path, x_keys.seed().expect("an X seed")).expect("the seed is written");
    let config = BusConfig {
        url: broker.url(),
        nkey_seed_path: seed_path.display().to_string(),
        stream_name: COMMANDS_STREAM.to_owned(),
        consumer_name: COMMANDS_DURABLE.to_owned(),
        events_stream_name: EVENTS_STREAM.to_owned(),
    };

    let client = connect_bus(&config)
        .await
        .expect("the service connects through its own connection function");
    let context = jetstream::new(client);
    x_capture::ensure_browser_consumer(&context, COMMANDS_STREAM, COMMANDS_DURABLE)
        .await
        .expect("the command durable verifies under the X stanza");
    extractor_reports::ensure_reports_consumer(&context, EVENTS_STREAM, extractor_reports::DURABLE)
        .await
        .expect("the report durable verifies under the X stanza");
    let ack = context
        .publish(REPORT_SUBJECT, "{}".into())
        .await
        .expect("the report publishes")
        .await
        .expect("the broker acknowledges the report");
    assert_eq!(
        ack.stream, EVENTS_STREAM,
        "the report is stored by Edge's stream"
    );
}
