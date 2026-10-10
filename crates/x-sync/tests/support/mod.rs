//! Shared fixtures: publish a source through the bookmark path, the account-scoped path that
//! remains after the shared-row explicit capture was removed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use x_budget::gate::{BudgetClass, BudgetGate, Clock};
use x_persistence::database::Database;
use x_sync::{BookmarkPage, BookmarkPageSource, BookmarkSnapshotService, BookmarkSourceError};

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        "2026-08-20T12:00:00Z"
            .parse()
            .expect("the fixture instant parses")
    }
}

/// A one-page bookmark source holding exactly one post.
#[derive(Debug, Clone)]
pub(crate) struct OnePost {
    /// Provider identity of the post.
    pub(crate) provider_id: String,
    /// Provider identity of its author.
    pub(crate) author_id: String,
    /// The post text.
    pub(crate) text: String,
}

impl BookmarkPageSource for OnePost {
    fn fetch_page<'a>(
        &'a self,
        _continuation: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<BookmarkPage, BookmarkSourceError>> + Send + 'a>> {
        let payload = serde_json::json!({
            "data": [{
                "id": self.provider_id,
                "text": self.text,
                "author_id": self.author_id,
                "created_at": "2026-08-16T09:30:00Z"
            }],
            "includes": {"users": [{
                "id": self.author_id,
                "name": "Fixture Author",
                "username": "fixture_author"
            }]}
        });
        let envelope = serde_json::from_value(payload).expect("the fixture decodes");
        Box::pin(async move { Ok(BookmarkPage::new(envelope, None)) })
    }
}

/// A snapshot service over a fixed clock and a generous read budget.
pub(crate) fn snapshot_service(database: Database) -> BookmarkSnapshotService {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock);
    let budget = BudgetGate::with_clock(
        database.clone(),
        BudgetClass::Read,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the fixture budget configuration is valid");
    BookmarkSnapshotService::new(database, budget, clock)
}
