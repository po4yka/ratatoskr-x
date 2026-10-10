//! The public app-only post resolver against recorded X API responses served by `WireMock`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::time::Duration;

use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use x_sync::{AppBearerResolver, PublicPostFailure, PublicPostResolver};

const POST_ID: &str = "1900000000000000001";
const TOKEN: &str = "app-only-test-token-0123456789";

fn resolver(server: &MockServer) -> AppBearerResolver {
    AppBearerResolver::with_timeouts(
        &server.uri(),
        TOKEN,
        Duration::from_millis(500),
        Duration::from_millis(500),
    )
    .expect("the resolver builds")
}

fn post_envelope() -> serde_json::Value {
    json!({
        "data": [{
            "id": POST_ID,
            "text": "A public post https://t.co/abc",
            "author_id": "42",
            "created_at": "2026-08-16T09:30:00.120Z",
            "lang": "en",
            "entities": {"urls": [{
                "url": "https://t.co/abc",
                "expanded_url": "https://example.test/article",
                "display_url": "example.test/article"
            }]},
            "note_tweet": {"text": "A public post with the full long-form body."}
        }],
        "includes": {"users": [{"id": "42", "name": "Ada Example", "username": "ada"}]}
    })
}

async fn serve(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/2/tweets"))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn failure_for(response: ResponseTemplate) -> PublicPostFailure {
    let server = MockServer::start().await;
    serve(&server, response).await;
    resolver(&server)
        .resolve(POST_ID)
        .await
        .expect_err("the response is not a post")
}

#[tokio::test]
async fn a_public_post_resolves_with_the_documented_app_only_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2/tweets"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(query_param("ids", POST_ID))
        .and(query_param(
            "expansions",
            "author_id,attachments.media_keys,referenced_tweets.id",
        ))
        .and(query_param(
            "tweet.fields",
            "created_at,text,entities,note_tweet,lang,conversation_id,public_metrics,referenced_tweets,attachments",
        ))
        .and(query_param("user.fields", "username,name"))
        .and(query_param(
            "media.fields",
            "type,url,preview_image_url,width,height,alt_text,duration_ms",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(post_envelope()))
        .expect(1)
        .mount(&server)
        .await;

    let post = resolver(&server)
        .resolve(POST_ID)
        .await
        .expect("the public post resolves");

    assert_eq!(post.provider_id, POST_ID);
    assert_eq!(post.author.id, "42");
    assert_eq!(post.author.username.as_deref(), Some("ada"));
    assert_eq!(post.author.name.as_deref(), Some("Ada Example"));
    assert_eq!(post.text, "A public post https://t.co/abc");
    assert_eq!(
        post.long_text.as_deref(),
        Some("A public post with the full long-form body.")
    );
    assert_eq!(
        post.published_at.map(|at| at.to_rfc3339()),
        Some("2026-08-16T09:30:00.120+00:00".to_owned())
    );
    assert_eq!(post.expanded_urls, vec!["https://example.test/article"]);

    let requests = server
        .received_requests()
        .await
        .expect("requests are recorded");
    assert_eq!(requests.len(), 1);
    for (name, _) in &requests.first().expect("the element exists").headers {
        let name = name.as_str();
        assert!(
            !name.contains("cookie") && !name.starts_with("x-") && name != "proxy-authorization",
            "no user session material may accompany the app-only call: {name}"
        );
    }
}

#[tokio::test]
async fn error_types_in_a_200_response_map_to_typed_failures() {
    for (problem, expected) in [
        ("resource-not-found", PublicPostFailure::Deleted),
        (
            "not-authorized-for-resource",
            PublicPostFailure::Inaccessible,
        ),
        ("resource-unavailable", PublicPostFailure::Inaccessible),
        ("client-forbidden", PublicPostFailure::Inaccessible),
    ] {
        let body = json!({"errors": [{
            "value": POST_ID,
            "detail": "Detail text is never interpreted.",
            "title": "Problem",
            "resource_type": "tweet",
            "parameter": "ids",
            "resource_id": POST_ID,
            "type": format!("https://api.twitter.com/2/problems/{problem}")
        }]});
        let failure = failure_for(ResponseTemplate::new(200).set_body_json(body)).await;
        assert_eq!(failure, expected, "{problem}");
    }
}

#[tokio::test]
async fn throttling_and_server_errors_are_transient() {
    for status in [429_u16, 500, 503] {
        let failure = failure_for(ResponseTemplate::new(status)).await;
        assert_eq!(failure, PublicPostFailure::Transient, "{status}");
    }
}

#[tokio::test]
async fn a_slow_provider_is_transient() {
    let failure = failure_for(
        ResponseTemplate::new(200)
            .set_body_json(post_envelope())
            .set_delay(Duration::from_secs(3)),
    )
    .await;
    assert_eq!(failure, PublicPostFailure::Transient);
}

#[tokio::test]
async fn a_refused_connection_is_transient() {
    // Bind and release a port: nothing listens there afterwards.
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a probe listener")
        .local_addr()
        .expect("a local address");
    let resolver = AppBearerResolver::with_timeouts(
        &format!("http://{address}"),
        TOKEN,
        Duration::from_millis(500),
        Duration::from_millis(500),
    )
    .expect("the resolver builds");
    let failure = resolver
        .resolve(POST_ID)
        .await
        .expect_err("nothing is listening");
    assert_eq!(failure, PublicPostFailure::Transient);
}

#[tokio::test]
async fn a_rejected_credential_is_transient_and_counted() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _guard = metrics::set_default_local_recorder(&recorder);
    for status in [401_u16, 403] {
        let failure = failure_for(ResponseTemplate::new(status)).await;
        assert_eq!(
            failure,
            PublicPostFailure::Transient,
            "{status} is an operator problem, not a property of the post"
        );
    }
    let exposition = handle.render();
    assert!(
        exposition.contains("x_public_capture_credential_rejected_total 2"),
        "{exposition}"
    );
}

#[tokio::test]
async fn a_malformed_response_is_inaccessible() {
    for response in [
        ResponseTemplate::new(200).set_body_string("this is not json"),
        ResponseTemplate::new(200).set_body_json(json!({})),
        ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": POST_ID}]})),
        ResponseTemplate::new(200).set_body_string("x".repeat(2 * 1024 * 1024)),
    ] {
        let failure = failure_for(response).await;
        assert_eq!(failure, PublicPostFailure::Inaccessible);
    }
}

#[tokio::test]
async fn redirects_are_never_followed() {
    let server = MockServer::start().await;
    serve(
        &server,
        ResponseTemplate::new(302).insert_header("location", format!("{}/elsewhere", server.uri())),
    )
    .await;
    let elsewhere = Mock::given(method("GET"))
        .and(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(200).set_body_json(post_envelope()))
        .expect(0)
        .mount_as_scoped(&server)
        .await;

    let failure = resolver(&server)
        .resolve(POST_ID)
        .await
        .expect_err("a redirect is not a post");

    assert_eq!(failure, PublicPostFailure::Transient);
    drop(elsewhere);
}

#[tokio::test]
async fn the_token_never_appears_in_debug_output_or_errors() {
    let server = MockServer::start().await;
    let resolver = resolver(&server);
    let rendered = format!("{resolver:?}");
    assert!(!rendered.contains(TOKEN), "{rendered}");
    assert!(rendered.contains("redacted"), "{rendered}");

    serve(&server, ResponseTemplate::new(401)).await;
    let failure = resolver
        .resolve(POST_ID)
        .await
        .expect_err("the credential is rejected");
    let failure = format!("{failure} {failure:?}");
    assert!(!failure.contains(TOKEN), "{failure}");
}
