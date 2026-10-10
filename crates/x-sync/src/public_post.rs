//! Public, app-only resolution of one X post (XR-021 CONTRACTS.md S10 CD5).
//!
//! The call carries only an app-only bearer token: no user token, no cookie, nothing that makes the
//! answer depend on who asked. A protected post is therefore reported honestly as inaccessible
//! instead of being served from whatever another account's sync happened to store.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use x_normalize::dto::Envelope;
use x_normalize::normalize::normalize;

/// The request parameters of the lookup (CONTRACTS.md S10 CD5), one list per query member.
const EXPANSIONS: &str = "author_id,attachments.media_keys,referenced_tweets.id";
const TWEET_FIELDS: &str = "created_at,text,entities,note_tweet,lang,conversation_id,public_metrics,referenced_tweets,attachments";
const USER_FIELDS: &str = "username,name";
const MEDIA_FIELDS: &str = "type,url,preview_image_url,width,height,alt_text,duration_ms";

/// The response body is capped so a hostile or broken endpoint cannot exhaust memory.
const MAX_BODY_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// The author of a publicly resolved post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicAuthor {
    /// Provider identity of the author.
    pub id: String,
    /// Handle at resolution time.
    pub username: Option<String>,
    /// Display name at resolution time.
    pub name: Option<String>,
}

/// What the public app-only call returned for one post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicPost {
    /// Provider identity of the post.
    pub provider_id: String,
    /// The post author.
    pub author: PublicAuthor,
    /// Canonical short text.
    pub text: String,
    /// Long-form note body when the provider supplies one.
    pub long_text: Option<String>,
    /// Publication time.
    pub published_at: Option<DateTime<Utc>>,
    /// Provider-resolved URL entities.
    pub expanded_urls: Vec<String>,
}

/// Why a post could not be resolved publicly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PublicPostFailure {
    /// The provider says the post was deleted.
    #[error("the post was deleted")]
    Deleted,
    /// The post is protected, unavailable or unreadable with an app-only credential.
    #[error("the post cannot be retrieved publicly")]
    Inaccessible,
    /// The provider could not be reached, throttled the request, or rejected the credential.
    #[error("the provider could not resolve the post now")]
    Transient,
}

/// A source of publicly resolvable posts.
pub trait PublicPostResolver: Send + Sync {
    /// Resolves one post by its provider id.
    fn resolve<'a>(
        &'a self,
        provider_post_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PublicPost, PublicPostFailure>> + Send + 'a>>;
}

/// Why the resolver could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublicPostConfigError {
    /// The API base URL is not a URL.
    #[error("the public post API base URL is invalid")]
    BaseUrl,
    /// The HTTP client could not be built.
    #[error("the public post HTTP client could not be built")]
    Client(#[source] reqwest::Error),
}

/// Resolver over the official app-only X API.
#[derive(Clone)]
pub struct AppBearerResolver {
    http: reqwest::Client,
    endpoint: String,
    bearer_token: String,
}

impl std::fmt::Debug for AppBearerResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppBearerResolver")
            .field("endpoint", &self.endpoint)
            .field("bearer_token", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl AppBearerResolver {
    /// Builds the resolver with the production timeouts: 3 s to connect, 10 s in total.
    ///
    /// # Errors
    /// When the base URL is not a URL or the HTTP client cannot be built.
    pub fn new(api_base_url: &str, bearer_token: &str) -> Result<Self, PublicPostConfigError> {
        Self::with_timeouts(api_base_url, bearer_token, CONNECT_TIMEOUT, TOTAL_TIMEOUT)
    }

    /// Builds the resolver with explicit connect and total timeouts.
    ///
    /// Redirects are never followed and requests are never retried: a retry is the worker's
    /// decision, taken against its attempt budget.
    ///
    /// # Errors
    /// When the base URL is not a URL or the HTTP client cannot be built.
    pub fn with_timeouts(
        api_base_url: &str,
        bearer_token: &str,
        connect_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<Self, PublicPostConfigError> {
        let endpoint = format!("{}/2/tweets", api_base_url.trim_end_matches('/'));
        reqwest::Url::parse(&endpoint).map_err(|_| PublicPostConfigError::BaseUrl)?;
        let http = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(total_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(PublicPostConfigError::Client)?;
        Ok(Self {
            http,
            endpoint,
            bearer_token: bearer_token.to_owned(),
        })
    }

    async fn lookup(&self, provider_post_id: &str) -> Result<PublicPost, PublicPostFailure> {
        let url = reqwest::Url::parse_with_params(
            &self.endpoint,
            [
                ("ids", provider_post_id),
                ("expansions", EXPANSIONS),
                ("tweet.fields", TWEET_FIELDS),
                ("user.fields", USER_FIELDS),
                ("media.fields", MEDIA_FIELDS),
            ],
        )
        .map_err(|_| PublicPostFailure::Transient)?;
        let mut response = self
            .http
            .get(url)
            .bearer_auth(&self.bearer_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                // Content-free: the class only. The error text can carry the request URL.
                tracing::warn!(
                    class = transport_class(&error),
                    "the public post request failed before a response"
                );
                PublicPostFailure::Transient
            })?;
        classify_status(response.status())?;
        let body = read_capped(&mut response).await?;
        parse_post(&body)
    }
}

impl PublicPostResolver for AppBearerResolver {
    fn resolve<'a>(
        &'a self,
        provider_post_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PublicPost, PublicPostFailure>> + Send + 'a>> {
        Box::pin(self.lookup(provider_post_id))
    }
}

/// Maps the HTTP status of the whole request; success falls through to the body.
fn classify_status(status: reqwest::StatusCode) -> Result<(), PublicPostFailure> {
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        // A rejected app credential says nothing about the post: it is an operator problem, so
        // the capture is retried and the operator is told.
        metrics::counter!("x_public_capture_credential_rejected_total").increment(1);
        tracing::error!(
            status = status.as_u16(),
            "the X API rejected the app-only credential used for public capture"
        );
        return Err(PublicPostFailure::Transient);
    }
    if status == reqwest::StatusCode::PAYMENT_REQUIRED {
        // The plan or its quota, not the post: an operator problem as well.
        tracing::error!(
            status = status.as_u16(),
            "the X API refused public capture requests for the current plan"
        );
        return Err(PublicPostFailure::Transient);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
        || status.is_redirection()
    {
        return Err(PublicPostFailure::Transient);
    }
    Err(PublicPostFailure::Inaccessible)
}

/// A fixed, content-free class of a transport error.
fn transport_class(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_builder() {
        "request_build"
    } else {
        "other"
    }
}

/// Reads the body, refusing anything longer than [`MAX_BODY_BYTES`].
async fn read_capped(response: &mut reqwest::Response) -> Result<Vec<u8>, PublicPostFailure> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(PublicPostFailure::Inaccessible);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| PublicPostFailure::Transient)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(PublicPostFailure::Inaccessible);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Turns a 200 body into the post, or into the typed reason it is not one.
fn parse_post(body: &[u8]) -> Result<PublicPost, PublicPostFailure> {
    let envelope: Envelope =
        serde_json::from_slice(body).map_err(|_| PublicPostFailure::Inaccessible)?;
    if envelope.posts().is_empty() {
        return Err(classify_problems(&envelope));
    }
    envelope
        .validate()
        .map_err(|_| PublicPostFailure::Inaccessible)?;
    let batch = normalize(&envelope).map_err(|_| PublicPostFailure::Inaccessible)?;
    let post = batch
        .posts()
        .first()
        .ok_or(PublicPostFailure::Inaccessible)?;
    let author = batch
        .users()
        .iter()
        .find(|user| user.provider_id == post.author_provider_id);
    Ok(PublicPost {
        provider_id: post.provider_id.clone(),
        author: PublicAuthor {
            id: post.author_provider_id.clone(),
            username: author.and_then(|user| user.username.clone()),
            name: author.and_then(|user| user.display_name.clone()),
        },
        text: post.text.clone(),
        long_text: post.long_text.clone(),
        published_at: post.published_at,
        expanded_urls: post.expanded_urls.clone(),
    })
}

/// Reads the `errors[].type` problem URIs of a response that carries no post.
fn classify_problems(envelope: &Envelope) -> PublicPostFailure {
    let problems: Vec<&str> = envelope
        .extension
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|problem| problem.get("type")?.as_str())
        .collect();
    if problems
        .iter()
        .any(|problem| problem.ends_with("resource-not-found"))
    {
        return PublicPostFailure::Deleted;
    }
    // `not-authorized-for-resource`, `resource-unavailable`, `client-forbidden` and anything
    // unrecognised: the post cannot be read publicly.
    PublicPostFailure::Inaccessible
}
