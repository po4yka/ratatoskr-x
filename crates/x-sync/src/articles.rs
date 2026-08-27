//! Linked external-article capture and Extractor outcome projection.

use std::collections::HashSet;

use ratatoskr_event_envelope::{EventEnvelope, EventPayload as _};
use ratatoskr_operation_contracts::{OperationReported, OperationStatus};
use sqlx::types::Uuid;
use x_persistence::database::Database;

/// A selected external URL together with its conservative canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedArticleUrl {
    /// Original expanded URL retained as evidence.
    pub original_url: String,
    /// Canonical URL used as the account-scoped deduplication key.
    pub normalized_url: String,
}

/// Persists account-scoped article-capture requests and Extractor outcomes.
#[derive(Debug, Clone)]
pub struct ArticleCaptureService {
    database: Database,
}

impl ArticleCaptureService {
    /// Builds the service over X's owned persistence boundary.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Associates already-expanded external links with one social source.
    ///
    /// # Errors
    /// Returns a persistence error when the association cannot be stored.
    pub async fn capture_expanded_links(
        &self,
        account_id: Uuid,
        social_source_id: Uuid,
        urls: &[String],
    ) -> Result<(), ArticleCaptureError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(ArticleCaptureError::Query)?;
        capture_expanded_links_in_transaction(&mut transaction, account_id, social_source_id, urls)
            .await
            .map_err(ArticleCaptureError::Query)?;
        transaction
            .commit()
            .await
            .map_err(ArticleCaptureError::Query)?;
        Ok(())
    }

    /// Consumes one Extractor operation report for a linked-article capture.
    ///
    /// # Errors
    /// Returns an error when the report cannot be validated or persisted.
    pub async fn consume_extractor_operation_report(
        &self,
        serialized_event: &str,
    ) -> Result<(), ArticleCaptureError> {
        let outcome = decode_extractor_outcome(serialized_event)?;

        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(ArticleCaptureError::Query)?;
        let capture: Option<(Uuid, Uuid)> = sqlx::query_as(
            "select capture.id, account.internal_user_id from x_archive.article_captures capture \
             join x_archive.accounts account on account.id = capture.account_id \
             where capture.correlation_id = $1",
        )
        .bind(&outcome.correlation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(ArticleCaptureError::Query)?;
        let Some((capture_id, owner_id)) = capture else {
            return Err(ArticleCaptureError::InvalidOutcome);
        };
        if !outcome.belongs_to(capture_id, owner_id) {
            return Err(ArticleCaptureError::InvalidOutcome);
        }

        if !claim_outcome_inbox(&mut transaction, &outcome, serialized_event).await? {
            transaction
                .commit()
                .await
                .map_err(ArticleCaptureError::Query)?;
            return Ok(());
        }

        complete_capture(&mut transaction, capture_id, &outcome).await?;

        transaction
            .commit()
            .await
            .map_err(ArticleCaptureError::Query)?;
        Ok(())
    }
}

/// Persists external capture requests within the caller's source transaction.
pub(crate) async fn capture_expanded_links_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    social_source_id: Uuid,
    urls: &[String],
) -> Result<(), sqlx::Error> {
    let selected = select_external_expanded_urls(urls);
    if selected.is_empty() {
        return Ok(());
    }

    let owner_id: Uuid = sqlx::query_scalar(
        "select account.internal_user_id from x_archive.social_sources source \
             join x_archive.accounts account on account.id = source.account_id \
             where source.social_source_id = $1 and source.account_id = $2",
    )
    .bind(social_source_id)
    .bind(account_id)
    .fetch_one(&mut **transaction)
    .await?;

    for selected_url in selected {
        let capture_id = Uuid::now_v7();
        let correlation_id = format!("article_capture:{capture_id}");
        let capture: (Uuid, String, bool) = sqlx::query_as(
            "with inserted as ( \
                   insert into x_archive.article_captures \
                     (id, account_id, normalized_url, original_url, correlation_id) \
                   values ($1, $2, $3, $4, $5) \
                   on conflict (account_id, normalized_url) do nothing \
                   returning id, correlation_id, true as created \
                 ) \
                 select id, correlation_id, created from inserted \
                 union all \
                 select id, correlation_id, false as created \
                   from x_archive.article_captures \
                  where account_id = $2 and normalized_url = $3 \
                 limit 1",
        )
        .bind(capture_id)
        .bind(account_id)
        .bind(&selected_url.normalized_url)
        .bind(&selected_url.original_url)
        .bind(correlation_id)
        .fetch_one(&mut **transaction)
        .await?;
        let (capture_id, correlation_id, created) = capture;

        sqlx::query(
            "insert into x_archive.post_article_links (social_source_id, article_capture_id) \
                 values ($1, $2) on conflict do nothing",
        )
        .bind(social_source_id)
        .bind(capture_id)
        .execute(&mut **transaction)
        .await?;

        if created {
            let command = serde_json::json!({
                "command_id": Uuid::now_v7(),
                "command_type": "content.capture.requested.v1",
                "requested_at": ratatoskr_identifiers::WireTimestamp::now(),
                "operation_id": capture_id,
                "tenant_id": format!("user:{owner_id}"),
                "correlation_id": correlation_id,
                "idempotency_key": capture_id.to_string(),
                "payload": { "url": selected_url.normalized_url },
            });
            sqlx::query(
                "insert into x_archive.outbox_events \
                     (aggregate, event_type, payload, correlation_id, causation_id) \
                     values ($1, 'content.capture.requested.v1', $2::jsonb, $3, $4)",
            )
            .bind(format!("article_capture:{capture_id}"))
            .bind(command.to_string())
            .bind(&correlation_id)
            .bind(format!("social_source:{social_source_id}"))
            .execute(&mut **transaction)
            .await?;
        }
    }

    Ok(())
}

struct ExtractorOutcome {
    event_id: String,
    correlation_id: String,
    operation_id: String,
    tenant_id: String,
    document_id: String,
    document_ir_blob: serde_json::Value,
}

impl ExtractorOutcome {
    fn belongs_to(&self, capture_id: Uuid, owner_id: Uuid) -> bool {
        self.operation_id == capture_id.to_string() && self.tenant_id == format!("user:{owner_id}")
    }
}

fn decode_extractor_outcome(
    serialized_event: &str,
) -> Result<ExtractorOutcome, ArticleCaptureError> {
    let envelope: EventEnvelope =
        serde_json::from_str(serialized_event).map_err(|_| ArticleCaptureError::InvalidOutcome)?;
    if envelope.producer.to_string() != "ratatoskr-extractor" {
        return Err(ArticleCaptureError::InvalidOutcome);
    }
    let report: OperationReported = envelope
        .payload_as()
        .map_err(|_| ArticleCaptureError::InvalidOutcome)?;
    if report.status != OperationStatus::Succeeded {
        return Err(ArticleCaptureError::InvalidOutcome);
    }
    let result = report
        .results
        .iter()
        .find(|result| result.result_kind.as_str() == "content.document")
        .filter(|result| result.target.kind().as_str() == "document")
        .filter(|result| result.target.as_uuid().is_some())
        .filter(|result| {
            result
                .blob
                .as_ref()
                .is_some_and(|blob| blob.owner_service.as_str() == "ratatoskr-extractor")
        })
        .ok_or(ArticleCaptureError::InvalidOutcome)?;
    let document_id = result
        .target
        .as_uuid()
        .ok_or(ArticleCaptureError::InvalidOutcome)?
        .to_string();
    let document_ir_blob = serde_json::to_value(
        result
            .blob
            .as_ref()
            .ok_or(ArticleCaptureError::InvalidOutcome)?,
    )
    .map_err(|_| ArticleCaptureError::InvalidOutcome)?;
    let tenant_id = envelope
        .tenant_id
        .as_ref()
        .map(ToString::to_string)
        .ok_or(ArticleCaptureError::InvalidOutcome)?;

    Ok(ExtractorOutcome {
        event_id: envelope.event_id.to_string(),
        correlation_id: envelope.correlation_id.to_wire(),
        operation_id: report.operation_id.to_string(),
        tenant_id,
        document_id,
        document_ir_blob,
    })
}

async fn claim_outcome_inbox(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    outcome: &ExtractorOutcome,
    serialized_event: &str,
) -> Result<bool, ArticleCaptureError> {
    let applied: Option<Uuid> = sqlx::query_scalar(
        "insert into x_archive.inbox_events (source, event_type, event_id, payload, consumed_at) \
         values ('ratatoskr-extractor', $1, $2, $3::jsonb, now()) \
         on conflict (event_id) do nothing returning id",
    )
    .bind(OperationReported::EVENT_TYPE)
    .bind(&outcome.event_id)
    .bind(serialized_event)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(ArticleCaptureError::Query)?;
    Ok(applied.is_some())
}

async fn complete_capture(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture_id: Uuid,
    outcome: &ExtractorOutcome,
) -> Result<(), ArticleCaptureError> {
    let updated = sqlx::query(
        "update x_archive.article_captures set state = 'completed', document_id = $2, \
         document_ir_blob = $3::jsonb, completed_at = now(), updated_at = now() \
         where id = $1 and state = 'requested'",
    )
    .bind(capture_id)
    .bind(&outcome.document_id)
    .bind(outcome.document_ir_blob.to_string())
    .execute(&mut **transaction)
    .await
    .map_err(ArticleCaptureError::Query)?;
    if updated.rows_affected() != 1 {
        return Err(ArticleCaptureError::InvalidOutcome);
    }
    Ok(())
}

/// Linked-article capture or outcome projection failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArticleCaptureError {
    /// The inbound event is not a validated Extractor document result for this capture.
    #[error("the extractor outcome is invalid for this article capture")]
    InvalidOutcome,
    /// The source does not belong to the account or is not available.
    #[error("the social source is unavailable for this account")]
    SourceUnavailable,
    /// A database query failed.
    #[error("the article capture query failed")]
    Query(#[source] sqlx::Error),
}

/// Selects unique external HTTP(S) links from already expanded provider entities.
#[must_use]
pub fn select_external_expanded_urls(urls: &[String]) -> Vec<SelectedArticleUrl> {
    let mut seen = HashSet::new();
    let mut selected = Vec::new();

    for original_url in urls {
        let Some(normalized_url) = normalize_external_url(original_url) else {
            continue;
        };
        if seen.insert(normalized_url.clone()) {
            selected.push(SelectedArticleUrl {
                original_url: original_url.clone(),
                normalized_url,
            });
        }
    }

    selected
}

fn normalize_external_url(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return None;
    }
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest.get(..authority_end)?;
    if authority.is_empty() || authority.contains('@') || authority.starts_with('[') {
        return None;
    }
    let (host, port) = split_host_and_port(authority)?;
    if is_x_host(&host) {
        return None;
    }

    let path_and_query = rest.get(authority_end..)?;
    let without_fragment = path_and_query
        .split_once('#')
        .map_or(path_and_query, |(prefix, _)| prefix);
    let (path, query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, None), |(path, query)| {
            (path, Some(query))
        });
    let path = if path.is_empty() { "/" } else { path };
    let query = canonical_query(query);

    let default_port =
        (scheme == "http" && port == Some(80)) || (scheme == "https" && port == Some(443));
    let authority = match (port, default_port) {
        (Some(port), false) => format!("{host}:{port}"),
        _ => host,
    };
    let suffix = query.map_or_else(String::new, |query| format!("?{query}"));
    Some(format!("{scheme}://{authority}{path}{suffix}"))
}

fn split_host_and_port(authority: &str) -> Option<(String, Option<u16>)> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, raw_port)) if !raw_port.is_empty() => {
            let port = raw_port.parse().ok()?;
            (host, Some(port))
        }
        Some(_) => return None,
        None => (authority, None),
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return None;
    }
    Some((host.to_ascii_lowercase(), port))
}

fn is_x_host(host: &str) -> bool {
    ["x.com", "twitter.com", "t.co"].iter().any(|suffix| {
        host == *suffix
            || host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn canonical_query(query: Option<&str>) -> Option<String> {
    let mut retained: Vec<&str> = query?
        .split('&')
        .filter(|component| !component.is_empty())
        .filter(|component| {
            let key = component.split_once('=').map_or(*component, |(key, _)| key);
            !is_tracking_key(key)
        })
        .collect();
    retained.sort_unstable();
    (!retained.is_empty()).then(|| retained.join("&"))
}

fn is_tracking_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_") || matches!(key.as_str(), "fbclid" | "gclid")
}
