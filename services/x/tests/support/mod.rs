//! A private, authorization-enabled `nats-server` for the tests that need the deployed ACL: the X
//! stanza shipped in `deploy/nats/identity.conf`, an administrator that stands in for Edge, and
//! optionally a Knowledge identity that may publish only the acknowledgement subject.

#![allow(
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "assertions in a test binary; each test binary uses a part of these helpers"
)]

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use uuid::Uuid;

pub(crate) const HOST: &str = "127.0.0.1";
pub(crate) const OWNER: &str = "user:018f0000-0000-7000-8000-000000000005";
pub(crate) const OTHER_OWNER: &str = "user:018f0000-0000-7000-8000-000000000006";
pub(crate) const REMOVAL: &str = "social.source.removed.v1";
pub(crate) const ACK_SUBJECT: &str = "evt.knowledge.deletion.acknowledged.v1";

/// The inbox prefix the test Knowledge identity connects under; its stanza lets it subscribe there
/// and nowhere else.
pub(crate) const KNOWLEDGE_INBOX_PREFIX: &str = "_INBOX_knowledge";

/// The public keys of the three identities a [`Enforcing`] broker knows.
pub(crate) struct Keys {
    pub(crate) x: nkeys::KeyPair,
    pub(crate) admin: nkeys::KeyPair,
    pub(crate) knowledge: nkeys::KeyPair,
}

impl Keys {
    pub(crate) fn new() -> Self {
        Self {
            x: nkeys::KeyPair::new_user(),
            admin: nkeys::KeyPair::new_user(),
            knowledge: nkeys::KeyPair::new_user(),
        }
    }
}

/// A private `nats-server` with `JetStream` whose users are the X stanza as shipped, an
/// administrator and the test Knowledge identity.
pub(crate) struct Enforcing {
    child: Child,
    port: u16,
    pub(crate) directory: PathBuf,
}

impl Enforcing {
    pub(crate) fn start(keys: &Keys) -> Self {
        let stanza = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/nats/identity.conf"
        ))
        .expect("deploy/nats/identity.conf is shipped");
        let stanza: String = stanza
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("nkey:") {
                    format!("    nkey: {}", keys.x.public_key())
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
            "ratatoskr-x-enforcing-{}",
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&directory).expect("a scratch directory");
        let config = format!(
            "listen: {HOST}:{port}\njetstream {{ store_dir: \"{store}\" }}\nauthorization {{\n  \
             users = [\n{stanza}\n    {{ nkey: {admin} }}\n    {{ nkey: {knowledge}, \
             permissions: {{ publish: {{ allow: [\"{ACK_SUBJECT}\"] }}, \
             subscribe: {{ allow: [\"{KNOWLEDGE_INBOX_PREFIX}.>\"] }} }} }}\n  ]\n}}\n",
            store = directory.join("js").display(),
            admin = keys.admin.public_key(),
            knowledge = keys.knowledge.public_key(),
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

    pub(crate) fn url(&self) -> String {
        format!("nats://{HOST}:{}", self.port)
    }

    /// Writes the X seed where the service reads it and returns the path.
    pub(crate) fn write_seed(&self, name: &str, keys: &nkeys::KeyPair) -> PathBuf {
        let path = self.directory.join(name);
        std::fs::write(&path, keys.seed().expect("a seed")).expect("the seed is written");
        path
    }
}

impl Drop for Enforcing {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// A removal envelope for a social source of `OWNER`; the row id is the event id.
pub(crate) fn removal_envelope(id: Uuid) -> String {
    serde_json::json!({
        "event_id": id,
        "event_type": REMOVAL,
        "occurred_at": "2026-10-12T08:00:00Z",
        "producer": "ratatoskr-x",
        "aggregate_id": format!("social_source:{id}"),
        "correlation_id": format!("event:{id}"),
        "tenant_id": OWNER,
        "schema_version": 1,
        "payload": {"social_source_id": id, "reason": "retention_policy"},
    })
    .to_string()
}

/// What Knowledge publishes: the envelope of CONTRACTS.md R5-11 rule 1.
pub(crate) struct Answer {
    pub(crate) acknowledged: Uuid,
    pub(crate) owner: &'static str,
    pub(crate) family: &'static str,
    pub(crate) outcome: &'static str,
    pub(crate) refusal: Option<&'static str>,
    pub(crate) producer: &'static str,
}

impl Answer {
    pub(crate) fn applied(acknowledged: Uuid) -> Self {
        Self {
            acknowledged,
            owner: OWNER,
            family: "social",
            outcome: "applied",
            refusal: None,
            producer: "ratatoskr-knowledge",
        }
    }

    pub(crate) fn refused(acknowledged: Uuid) -> Self {
        Self {
            outcome: "refused",
            refusal: Some("foreign_subject"),
            ..Self::applied(acknowledged)
        }
    }

    pub(crate) fn document(&self) -> String {
        let mut payload = serde_json::json!({
            "acknowledged_event_id": self.acknowledged,
            "family": self.family,
            "owner": self.owner,
            "subject": format!("social_source:{}", self.acknowledged),
            "outcome": self.outcome,
            "acknowledged_at": "2026-10-12T08:00:02Z",
        });
        if let (Some(refusal), Some(object)) = (self.refusal, payload.as_object_mut()) {
            object.insert("refusal".to_owned(), serde_json::Value::from(refusal));
        }
        serde_json::json!({
            "event_id": Uuid::now_v7(),
            "event_type": "knowledge.deletion.acknowledged.v1",
            "occurred_at": "2026-10-12T08:00:02Z",
            "producer": self.producer,
            "aggregate_id": format!("social_source:{}", self.acknowledged),
            "correlation_id": format!("event:{}", self.acknowledged),
            "causation_id": format!("event:{}", self.acknowledged),
            "tenant_id": self.owner,
            "schema_version": 1,
            "payload": payload,
        })
        .to_string()
    }
}
