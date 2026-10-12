//! The service reaches its Edge-provisioned durables through its real connection function under
//! the X stanza of the deployed ACL (XR-021 CONTRACTS.md section R3-01). That stanza lets the
//! identity subscribe only to `_INBOX_x.>`, so a connection without the inbox prefix never
//! receives a `JetStream` reply.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_x::bus::connect_bus;
use ratatoskr_x::deletion_acks;
use ratatoskr_x::extractor_reports;
use support::{ACK_SUBJECT, Enforcing, Keys};
use x_core::config::BusConfig;

mod support;

const COMMANDS_STREAM: &str = "ratatoskr_commands";
const EVENTS_STREAM: &str = "ratatoskr_events";
const COMMANDS_DURABLE: &str = "ratatoskr_x_browser_capture";
const REPORT_SUBJECT: &str = "evt.platform.operation.reported.v1";
/// A durable of another producer on the same stream: the X stanza must not reach it.
const SIBLING_DURABLE: &str = "ratatoskr_chatgpt_deletion_acks";

/// Creates what Edge creates: both streams, the durables the X identity may use and one durable of
/// a sibling producer that it may not.
async fn provision(url: &str, admin_seed: &str) -> jetstream::Context {
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
    for durable in [deletion_acks::DURABLE, SIBLING_DURABLE] {
        events
            .create_consumer(jetstream::consumer::pull::Config {
                durable_name: Some(durable.to_owned()),
                filter_subject: ACK_SUBJECT.to_owned(),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ack_wait: Duration::from_secs(30),
                ..jetstream::consumer::pull::Config::default()
            })
            .await
            .expect("the acknowledgement durable exists");
    }
    context
}

fn bus_config(broker: &Enforcing, seed_path: &std::path::Path) -> BusConfig {
    BusConfig {
        url: broker.url(),
        nkey_seed_path: seed_path.display().to_string(),
        stream_name: COMMANDS_STREAM.to_owned(),
        consumer_name: COMMANDS_DURABLE.to_owned(),
        events_stream_name: EVENTS_STREAM.to_owned(),
    }
}

#[tokio::test]
async fn the_service_completes_jetstream_requests_under_its_inbox_prefix() {
    let keys = Keys::new();
    let broker = Enforcing::start(&keys);
    provision(&broker.url(), &keys.admin.seed().expect("an admin seed")).await;
    let seed_path = broker.write_seed("x.nkey", &keys.x);

    let client = connect_bus(&bus_config(&broker, &seed_path))
        .await
        .expect("the service connects through its own connection function");
    let context = jetstream::new(client);
    x_capture::ensure_browser_consumer(&context, COMMANDS_STREAM, COMMANDS_DURABLE)
        .await
        .expect("the command durable verifies under the X stanza");
    extractor_reports::ensure_reports_consumer(&context, EVENTS_STREAM, extractor_reports::DURABLE)
        .await
        .expect("the report durable verifies under the X stanza");
    deletion_acks::ensure_acks_consumer(&context, EVENTS_STREAM, deletion_acks::DURABLE)
        .await
        .expect("the acknowledgement durable verifies under the X stanza");
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

/// XR-021 CONTRACTS.md R5-11 rule 3: the X stanza reaches its own acknowledgement durable and no
/// sibling producer's.
#[tokio::test]
async fn the_x_stanza_fetches_from_its_own_acknowledgement_durable_and_from_no_sibling() {
    let keys = Keys::new();
    let broker = Enforcing::start(&keys);
    let edge = provision(&broker.url(), &keys.admin.seed().expect("an admin seed")).await;
    let seed_path = broker.write_seed("x.nkey", &keys.x);
    edge.publish(ACK_SUBJECT, "{}".into())
        .await
        .expect("the acknowledgement publishes")
        .await
        .expect("the broker stores the acknowledgement");

    let client = connect_bus(&bus_config(&broker, &seed_path))
        .await
        .expect("the service connects");
    let context = jetstream::new(client);
    let own = context
        .get_consumer_from_stream(deletion_acks::DURABLE, EVENTS_STREAM)
        .await
        .expect("the own durable is visible");
    let mut batch = own
        .fetch()
        .max_messages(1)
        .expires(Duration::from_secs(5))
        .messages()
        .await
        .expect("the own durable can be fetched from");
    let delivered = futures_util::StreamExt::next(&mut batch)
        .await
        .expect("the acknowledgement is delivered")
        .expect("the delivery is intact");
    delivered.ack().await.expect("the delivery is acknowledged");

    let sibling = tokio::time::timeout(
        Duration::from_secs(5),
        context.get_consumer_from_stream::<jetstream::consumer::pull::Config, _, _>(
            SIBLING_DURABLE,
            EVENTS_STREAM,
        ),
    )
    .await;
    assert!(
        !matches!(sibling, Ok(Ok(_))),
        "a sibling producer's durable is out of reach of the X stanza"
    );
}
