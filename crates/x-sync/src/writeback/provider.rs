//! Official-X bookmark mutation adapter boundary.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::payload::CredentialPayload;
use x_persistence::database::Database;

use super::{
    BookmarkMutationProvider, BookmarkProviderError, BookmarkProviderEvidence,
    BookmarkProviderSuccess,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_RESPONSE_SIZE: usize = 64 * 1024;

type CredentialState = (String, Vec<u8>, Vec<String>, Vec<String>);

/// Concrete official-provider adapter with account-bound credential access.
#[derive(Clone)]
pub struct OfficialBookmarkProvider {
    database: Database,
    cipher: TokenCipher,
    base_url: String,
    http: Option<reqwest::Client>,
}

impl std::fmt::Debug for OfficialBookmarkProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OfficialBookmarkProvider")
            .field("http_ready", &self.http.is_some())
            .finish_non_exhaustive()
    }
}

impl OfficialBookmarkProvider {
    /// Builds the add/remove-only adapter around owned persistence and credential decryption.
    #[must_use]
    pub fn new(database: Database, cipher: TokenCipher, base_url: impl Into<String>) -> Self {
        Self::with_timeout(database, cipher, base_url, REQUEST_TIMEOUT)
    }

    /// Builds the adapter with an explicit end-to-end request timeout.
    #[must_use]
    pub fn with_timeout(
        database: Database,
        cipher: TokenCipher,
        base_url: impl Into<String>,
        request_timeout: Duration,
    ) -> Self {
        let base_url = base_url.into();
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .retry(reqwest::retry::never())
            .build()
            .ok();
        Self {
            database,
            cipher,
            base_url: base_url.trim_end_matches('/').to_owned(),
            http,
        }
    }

    async fn credential(
        &self,
        account_id: uuid::Uuid,
    ) -> Result<(String, CredentialPayload), BookmarkProviderError> {
        let row: Option<CredentialState> = sqlx::query_as(
            "select account.provider_user_id, credential.encrypted_payload, \
                    credential.granted_scopes, write_auth.granted_scopes \
             from x_archive.accounts account \
             join x_archive.credentials credential \
               on credential.account_id = account.id and credential.status = 'active' \
             join x_archive.bookmark_write_authorizations write_auth \
               on write_auth.account_id = account.id and write_auth.status = 'active' \
             where account.id = $1 and account.state = 'connected' \
             order by credential.created_at desc, credential.id desc limit 1",
        )
        .bind(account_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(|_| BookmarkProviderError::CredentialUnavailable)?;
        let Some((provider_user_id, encrypted, credential_scopes, authorization_scopes)) = row
        else {
            return Err(BookmarkProviderError::CredentialUnavailable);
        };
        let required = [
            "users.read",
            "tweet.read",
            "bookmark.read",
            "offline.access",
            "bookmark.write",
        ];
        if required.iter().any(|required_scope| {
            !credential_scopes
                .iter()
                .any(|scope| scope == required_scope)
                || !authorization_scopes
                    .iter()
                    .any(|scope| scope == required_scope)
        }) {
            return Err(BookmarkProviderError::CredentialScopeRequired);
        }
        let opened = self
            .cipher
            .open(account_id, Purpose::Credential, &encrypted)
            .map_err(|_| BookmarkProviderError::CredentialInvalid)?;
        let payload = CredentialPayload::decode(&opened)
            .map_err(|_| BookmarkProviderError::CredentialInvalid)?;
        Ok((provider_user_id, payload))
    }

    async fn add(
        &self,
        account_id: uuid::Uuid,
        provider_post_id: &str,
    ) -> Result<BookmarkProviderSuccess, BookmarkProviderError> {
        let (provider_user_id, credential) = self.credential(account_id).await?;
        let url = format!("{}/2/users/{provider_user_id}/bookmarks", self.base_url);
        self.send(
            reqwest::Method::POST,
            url,
            credential,
            Some(provider_post_id),
            true,
        )
        .await
    }

    async fn remove(
        &self,
        account_id: uuid::Uuid,
        provider_post_id: &str,
    ) -> Result<BookmarkProviderSuccess, BookmarkProviderError> {
        let (provider_user_id, credential) = self.credential(account_id).await?;
        let url = format!(
            "{}/2/users/{provider_user_id}/bookmarks/{provider_post_id}",
            self.base_url
        );
        self.send(reqwest::Method::DELETE, url, credential, None, false)
            .await
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: String,
        credential: CredentialPayload,
        provider_post_id: Option<&str>,
        expected_bookmarked: bool,
    ) -> Result<BookmarkProviderSuccess, BookmarkProviderError> {
        let Some(http) = self.http.as_ref() else {
            return Err(BookmarkProviderError::Transient);
        };
        let request = http
            .request(method, url)
            .bearer_auth(&credential.access_token);
        let request = if let Some(provider_post_id) = provider_post_id {
            request.json(&serde_json::json!({ "tweet_id": provider_post_id }))
        } else {
            request
        };
        let mut response = request
            .send()
            .await
            .map_err(|error| classify_transport(&error))?;
        let evidence = response_evidence(&response);
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(BookmarkProviderError::AuthorizationLost { evidence });
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let reset_epoch_seconds = response
                .headers()
                .get("x-rate-limit-reset")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok());
            return Err(BookmarkProviderError::RateLimited {
                reset_epoch_seconds,
                evidence,
            });
        }
        if status.is_client_error() {
            return Err(BookmarkProviderError::DefiniteRefusal {
                status: status.as_u16(),
                evidence,
            });
        }
        if status.is_server_error() || !status.is_success() {
            return Err(BookmarkProviderError::Uncertain {
                status: Some(status.as_u16()),
                evidence,
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES)
        {
            return Err(BookmarkProviderError::Uncertain {
                status: Some(status.as_u16()),
                evidence,
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| classify_transport(&error))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_SIZE {
                return Err(BookmarkProviderError::Uncertain {
                    status: Some(status.as_u16()),
                    evidence,
                });
            }
            body.extend_from_slice(&chunk);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| BookmarkProviderError::Uncertain {
                status: Some(status.as_u16()),
                evidence: evidence.clone(),
            })?;
        if value
            .pointer("/data/bookmarked")
            .and_then(serde_json::Value::as_bool)
            == Some(expected_bookmarked)
        {
            Ok(BookmarkProviderSuccess { evidence })
        } else {
            Err(BookmarkProviderError::Uncertain {
                status: Some(status.as_u16()),
                evidence,
            })
        }
    }
}

impl BookmarkMutationProvider for OfficialBookmarkProvider {
    fn add_bookmark<'a>(
        &'a self,
        account_id: uuid::Uuid,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        Box::pin(self.add(account_id, provider_post_id))
    }

    fn remove_bookmark<'a>(
        &'a self,
        account_id: uuid::Uuid,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        Box::pin(self.remove(account_id, provider_post_id))
    }
}

fn classify_transport(error: &reqwest::Error) -> BookmarkProviderError {
    if error.is_connect() {
        BookmarkProviderError::Transient
    } else {
        BookmarkProviderError::Uncertain {
            status: error.status().map(|status| status.as_u16()),
            evidence: BookmarkProviderEvidence::default(),
        }
    }
}

fn response_evidence(response: &reqwest::Response) -> BookmarkProviderEvidence {
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .and_then(bounded_request_id);
    BookmarkProviderEvidence { request_id }
}

fn bounded_request_id(value: &str) -> Option<String> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        Some(value.to_owned())
    } else {
        None
    }
}
