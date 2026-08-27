//! Bookmark write-back admission against the owned database and isolated write budget.

#![allow(
    clippy::expect_used,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::items_after_statements,
    clippy::panic,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::too_many_lines,
    clippy::type_complexity,
    reason = "end-to-end database scenarios keep their full setup, action, and evidence assertions together"
)]

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use sqlx::Row;
use x_budget::gate::{BudgetClass, BudgetError, BudgetGate, Clock};
use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::payload::CredentialPayload;
use x_persistence::test_support::TestDatabase;
use x_sync::{
    BookmarkAction, BookmarkAdmissionRefusal, BookmarkDryRunOutcome, BookmarkDryRunResult,
    BookmarkMutationProvider, BookmarkPage, BookmarkPageSource, BookmarkProviderError,
    BookmarkProviderSuccess, BookmarkSnapshotService, BookmarkSourceError, BookmarkWriteRequest,
    BookmarkWriteResult, BookmarkWriteStatus, BookmarkWritebackError, BookmarkWritebackService,
    ConsentSurfaceId, OfficialBookmarkProvider, SnapshotOutcome,
};

#[derive(Debug)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[derive(Debug, Default)]
struct FakeBookmarkProvider {
    calls: AtomicUsize,
}

impl BookmarkMutationProvider for FakeBookmarkProvider {
    fn add_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        _provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Ok(BookmarkProviderSuccess {
                evidence: Default::default(),
            })
        })
    }

    fn remove_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        _provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Ok(BookmarkProviderSuccess {
                evidence: Default::default(),
            })
        })
    }
}

#[derive(Debug, Default)]
struct EvidencedBookmarkProvider {
    calls: AtomicUsize,
}

impl BookmarkMutationProvider for EvidencedBookmarkProvider {
    fn add_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            Ok(BookmarkProviderSuccess {
                evidence: x_sync::BookmarkProviderEvidence {
                    request_id: Some(format!("request-add-{provider_post_id}")),
                },
            })
        })
    }

    fn remove_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            Ok(BookmarkProviderSuccess {
                evidence: x_sync::BookmarkProviderEvidence {
                    request_id: Some(format!("request-remove-{provider_post_id}")),
                },
            })
        })
    }
}

#[derive(Debug, Default)]
struct UncertainBookmarkProvider {
    calls: AtomicUsize,
}

impl BookmarkMutationProvider for UncertainBookmarkProvider {
    fn add_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        _provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Err(BookmarkProviderError::Uncertain {
                status: Some(503),
                evidence: x_sync::BookmarkProviderEvidence {
                    request_id: Some("request-uncertain-add".to_owned()),
                },
            })
        })
    }

    fn remove_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        _provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Err(BookmarkProviderError::Uncertain {
                status: Some(503),
                evidence: x_sync::BookmarkProviderEvidence {
                    request_id: Some("request-uncertain-remove".to_owned()),
                },
            })
        })
    }
}

#[derive(Debug)]
struct SequencedBookmarkPages {
    calls: Mutex<Vec<Option<String>>>,
    pages: Mutex<VecDeque<Result<BookmarkPage, BookmarkSourceError>>>,
}

impl SequencedBookmarkPages {
    fn new(pages: Vec<Result<BookmarkPage, BookmarkSourceError>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            pages: Mutex::new(pages.into()),
        }
    }

    fn calls(&self) -> Vec<Option<String>> {
        self.calls
            .lock()
            .expect("snapshot source call recording is not poisoned")
            .clone()
    }
}

impl BookmarkPageSource for SequencedBookmarkPages {
    fn fetch_page<'a>(
        &'a self,
        continuation: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<BookmarkPage, BookmarkSourceError>> + Send + 'a>> {
        self.calls
            .lock()
            .expect("snapshot source call recording is not poisoned")
            .push(continuation.map(ToOwned::to_owned));
        let page = self
            .pages
            .lock()
            .expect("snapshot source response queue is not poisoned")
            .pop_front()
            .expect("the test supplied one response per snapshot request");
        Box::pin(async move { page })
    }
}

fn snapshot_page(provider_post_id: &str, next_token: Option<&str>) -> BookmarkPage {
    let envelope = serde_json::from_str(&format!(
        r#"{{"data":[{{"id":"{provider_post_id}","text":"uncertain target","author_id":"uncertain-author","created_at":"2026-08-01T12:00:00Z"}}],"includes":{{"users":[{{"id":"uncertain-author","name":"Author","username":"author"}}]}}}}"#
    ))
    .expect("the synthetic official bookmark page decodes");
    BookmarkPage::new(envelope, next_token.map(ToOwned::to_owned))
}

#[derive(Debug, Default)]
struct GatedBookmarkProvider {
    calls: AtomicUsize,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl BookmarkMutationProvider for GatedBookmarkProvider {
    fn add_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        _provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(BookmarkProviderSuccess {
                evidence: Default::default(),
            })
        })
    }

    fn remove_bookmark<'a>(
        &'a self,
        _account_id: uuid::Uuid,
        _provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    > {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(BookmarkProviderSuccess {
                evidence: Default::default(),
            })
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DryRunSideEffects {
    write_usage: Option<i32>,
    consumed_consents: i64,
    bookmark_projection: String,
}

async fn dry_run_side_effects(
    test: &TestDatabase,
    account: uuid::Uuid,
    instant: DateTime<Utc>,
) -> DryRunSideEffects {
    let write_usage = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the bookmark-write usage is readable");
    let consumed_consents: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.bookmark_write_consents \
         where account_id = $1 and consumed_at is not null",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the consent consumption count is readable");
    let bookmark_projection: String = sqlx::query_scalar(
        "select coalesce(jsonb_agg(to_jsonb(bookmark) order by bookmark.id)::text, '[]') \
         from x_archive.bookmarks bookmark where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the bookmark projection is readable");
    DryRunSideEffects {
        write_usage,
        consumed_consents,
        bookmark_projection,
    }
}

async fn observe_dry_run(
    test: &TestDatabase,
    account: uuid::Uuid,
    instant: DateTime<Utc>,
    service: &BookmarkWritebackService,
    request: BookmarkWriteRequest<'_>,
    action: BookmarkAction,
) -> (
    Result<BookmarkDryRunResult, BookmarkWritebackError>,
    DryRunSideEffects,
    DryRunSideEffects,
) {
    let before = dry_run_side_effects(test, account, instant).await;
    let result = service.dry_run(request, action).await;
    let after = dry_run_side_effects(test, account, instant).await;
    (result, before, after)
}

fn live_refusal(
    result: &Result<BookmarkWriteResult, BookmarkWritebackError>,
) -> Option<(BookmarkAdmissionRefusal, Option<DateTime<Utc>>)> {
    match result {
        Err(BookmarkWritebackError::OwnershipMismatch) => {
            Some((BookmarkAdmissionRefusal::OwnershipMismatch, None))
        }
        Err(BookmarkWritebackError::ConnectionInactive) => {
            Some((BookmarkAdmissionRefusal::ConnectionInactive, None))
        }
        Err(BookmarkWritebackError::WriteAuthorizationRequired) => {
            Some((BookmarkAdmissionRefusal::WriteAuthorizationRequired, None))
        }
        Err(BookmarkWritebackError::WriteScopeRequired) => {
            Some((BookmarkAdmissionRefusal::WriteScopeRequired, None))
        }
        Err(BookmarkWritebackError::ConsentRequired) => {
            Some((BookmarkAdmissionRefusal::ConsentRequired, None))
        }
        Err(BookmarkWritebackError::Budget(BudgetError::Exhausted { reset_at })) => {
            Some((BookmarkAdmissionRefusal::BudgetExhausted, Some(*reset_at)))
        }
        _ => None,
    }
}

async fn authorized_writeback_fixture() -> (
    TestDatabase,
    uuid::Uuid,
    uuid::Uuid,
    BookmarkWritebackService,
    DateTime<Utc>,
) {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("writeback-consent-binding-account")
        .await
        .expect("the connected account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the account owner is readable");
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    let intent_id = x_persistence::oauth_intents::insert_intent(
        &test.database,
        &x_persistence::oauth_intents::NewIntent {
            internal_user_id: owner,
            account_id: Some(account),
            purpose: "bookmark_write",
            state_hash: "7aad8eae0cca3cc3fbe55862f75893eaa63a20e36b52f26a8ee5a2a33d844708",
            code_verifier_encrypted: b"REDACTED-binding-verifier-envelope",
            nonce: "writeback-consent-binding-nonce",
            redirect_uri: "https://app.example/callback",
            requested_scopes: &scopes,
            created_at: "2026-08-27T11:55:00Z"
                .parse()
                .expect("the creation instant parses"),
            expires_at: "2026-08-27T12:05:00Z"
                .parse()
                .expect("the expiry instant parses"),
        },
    )
    .await
    .expect("the bookmark-write intent seeds");
    x_persistence::credentials::insert_with_status(
        &test.database,
        &x_persistence::credentials::NewCredential {
            account_id: account,
            encrypted_payload: b"REDACTED-binding-write-credential-envelope",
            granted_scopes: &scopes,
            expires_at: None,
        },
        "active",
    )
    .await
    .expect("the active write credential seeds");
    let instant: DateTime<Utc> = "2026-08-27T12:00:00Z"
        .parse()
        .expect("the fixture instant parses");
    sqlx::query(
        "insert into x_archive.bookmark_write_authorizations \
         (account_id, oauth_intent_id, granted_scopes, status, authorized_at) \
         values ($1, $2, $3, 'active', $4)",
    )
    .bind(account)
    .bind(intent_id)
    .bind(&scopes)
    .bind(instant)
    .execute(test.database.pool())
    .await
    .expect("the local write authorization seeds");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(instant));
    let budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::BookmarkWrite,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the isolated bookmark-write budget constructs");
    let service = BookmarkWritebackService::new(test.database.clone(), budget, clock, 300)
        .expect("the consent lifetime is valid");
    (test, account, owner, service, instant)
}

#[path = "bookmark_writeback/budget_projection.rs"]
mod budget_projection;
#[path = "bookmark_writeback/consent_idempotency.rs"]
mod consent_idempotency;
#[path = "bookmark_writeback/dry_run.rs"]
mod dry_run;
#[path = "bookmark_writeback/secrecy.rs"]
mod secrecy;
#[path = "bookmark_writeback/uncertainty_audit.rs"]
mod uncertainty_audit;
