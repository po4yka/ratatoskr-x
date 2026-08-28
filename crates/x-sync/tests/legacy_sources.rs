//! Credential-free preflight of pinned legacy bookmark source formats.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions and synthetic fixture setup in a test binary"
)]

use std::path::{Path, PathBuf};

use sha2::Digest as _;
use sqlx::Connection as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use x_sync::{
    LegacySourceKind, LegacySourceSelection, LegacySourceVersion, LegacyTransitionError,
    LegacyTransitionService, SourceLimits,
};

const MONOLITH_CSV: &str = include_str!("fixtures/legacy/monolith_bookmark_metadata.csv");
const FIELD_THEORY_JSONL: &str = include_str!("fixtures/legacy/field_theory_preflight_v1.jsonl");

const SQLITE_BOOKMARK_COLUMNS: &[&str] = &[
    "id",
    "tweet_id",
    "url",
    "text",
    "author_handle",
    "author_name",
    "author_profile_image_url",
    "posted_at",
    "bookmarked_at",
    "synced_at",
    "conversation_id",
    "in_reply_to_status_id",
    "quoted_status_id",
    "language",
    "like_count",
    "repost_count",
    "reply_count",
    "quote_count",
    "bookmark_count",
    "view_count",
    "media_count",
    "link_count",
    "links_json",
    "tags_json",
    "ingested_via",
    "categories",
    "primary_category",
    "github_urls",
    "domains",
    "primary_domain",
    "quoted_tweet_json",
    "article_title",
    "article_text",
    "article_site",
    "enriched_at",
    "folder_ids",
    "folder_names",
];

#[derive(Debug)]
struct FixtureDirectory {
    path: PathBuf,
}

impl FixtureDirectory {
    fn create() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "ratatoskr-x-legacy-sources-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self { path })
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

fn sha256(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        for nibble in [byte >> 4, byte & 0x0f] {
            if let Some(character) = char::from_digit(u32::from(nibble), 16) {
                encoded.push(character);
            }
        }
    }
    encoded
}

async fn create_field_theory_sqlite(path: &Path) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("the synthetic SQLite fixture opens");
    sqlx::raw_sql(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); \
         INSERT INTO meta (key, value) VALUES ('schema_version', '6'); \
         CREATE TABLE bookmarks ( \
             id TEXT PRIMARY KEY, tweet_id TEXT NOT NULL, url TEXT NOT NULL, text TEXT NOT NULL, \
             author_handle TEXT, author_name TEXT, author_profile_image_url TEXT, posted_at TEXT, \
             bookmarked_at TEXT, synced_at TEXT NOT NULL, conversation_id TEXT, \
             in_reply_to_status_id TEXT, quoted_status_id TEXT, language TEXT, like_count INTEGER, \
             repost_count INTEGER, reply_count INTEGER, quote_count INTEGER, \
             bookmark_count INTEGER, view_count INTEGER, media_count INTEGER DEFAULT 0, \
             link_count INTEGER DEFAULT 0, links_json TEXT, tags_json TEXT, ingested_via TEXT, \
             categories TEXT, primary_category TEXT, github_urls TEXT, domains TEXT, \
             primary_domain TEXT, quoted_tweet_json TEXT, article_title TEXT, article_text TEXT, \
             article_site TEXT, enriched_at TEXT, folder_ids TEXT, folder_names TEXT \
         ); \
         INSERT INTO bookmarks \
             (id, tweet_id, url, text, author_handle, author_name, posted_at, synced_at, \
              language, like_count, media_count, link_count, links_json, tags_json, ingested_via, \
              categories, primary_category, folder_ids, folder_names) \
         VALUES \
             ('3001', '3001', 'https://x.com/synthetic_c/status/3001', \
              'Synthetic SQLite record', 'synthetic_c', 'Synthetic C', \
              '2026-01-04T12:00:00Z', '2026-02-04T12:00:00Z', 'en', 4, 0, 0, '[]', '[]', \
              'graphql', 'research', 'research', '[\"folder-1\"]', '[\"Reading\"]');",
    )
    .execute(&mut connection)
    .await
    .expect("the exact Field Theory v6 schema and row are created");

    let schema_version: String =
        sqlx::query_scalar("SELECT value FROM meta WHERE key = 'schema_version'")
            .fetch_one(&mut connection)
            .await
            .expect("the SQLite fixture carries schema version 6");
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('bookmarks') ORDER BY cid")
            .fetch_all(&mut connection)
            .await
            .expect("the SQLite fixture column inventory is readable");
    connection
        .close()
        .await
        .expect("the SQLite fixture is durably closed");

    assert_eq!(schema_version, "6", "the SQLite fixture version is pinned");
    assert_eq!(
        columns, SQLITE_BOOKMARK_COLUMNS,
        "the SQLite fixture has the exact 37-column Field Theory projection"
    );
}

async fn add_forbidden_sqlite_column(path: &Path) {
    let options = SqliteConnectOptions::new().filename(path);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .expect("the synthetic SQLite fixture reopens for setup");
    sqlx::raw_sql("ALTER TABLE bookmarks ADD COLUMN session_cookie TEXT")
        .execute(&mut connection)
        .await
        .expect("the synthetic forbidden column is fixture setup only");
    connection
        .close()
        .await
        .expect("the forbidden SQLite fixture is durably closed");
}

#[tokio::test]
async fn preflight_accepts_pinned_monolith_csv_field_theory_jsonl_and_sqlite_v6() {
    let fixtures = FixtureDirectory::create().expect("a private synthetic fixture directory");
    let monolith_path = fixtures.join("x_bookmark_metadata.csv");
    let jsonl_path = fixtures.join("bookmarks.jsonl");
    let sqlite_path = fixtures.join("bookmarks.db");
    std::fs::write(&monolith_path, MONOLITH_CSV).expect("the monolith fixture is written");
    std::fs::write(&jsonl_path, FIELD_THEORY_JSONL).expect("the JSONL fixture is written");
    create_field_theory_sqlite(&sqlite_path).await;

    let limits = SourceLimits {
        max_bytes: 1_000_000,
        max_rows: 10,
        max_text_bytes: 10_000,
        max_json_depth: 10,
    };
    let monolith_result = LegacyTransitionService::preflight_source(
        LegacySourceSelection::monolith_csv(&monolith_path),
        limits,
    )
    .await;
    let jsonl_result = LegacyTransitionService::preflight_source(
        LegacySourceSelection::field_theory(Some(jsonl_path.clone()), None),
        limits,
    )
    .await;
    let sqlite_result = LegacyTransitionService::preflight_source(
        LegacySourceSelection::field_theory(None, Some(sqlite_path.clone())),
        limits,
    )
    .await;
    let preferred_result = LegacyTransitionService::preflight_source(
        LegacySourceSelection::field_theory(Some(jsonl_path.clone()), Some(sqlite_path.clone())),
        limits,
    )
    .await;

    let unimplemented: Vec<&str> = [
        ("monolith CSV", &monolith_result),
        ("Field Theory JSONL", &jsonl_result),
        ("Field Theory SQLite", &sqlite_result),
        ("Field Theory JSONL precedence", &preferred_result),
    ]
    .into_iter()
    .filter_map(|(source, result)| {
        matches!(result, Err(LegacyTransitionError::NotImplemented)).then_some(source)
    })
    .collect();
    assert!(
        unimplemented.is_empty(),
        "LegacyTransitionError::NotImplemented remains for {unimplemented:?}"
    );

    let monolith = monolith_result.expect("the pinned monolith CSV preflights");
    let jsonl = jsonl_result.expect("the pinned Field Theory JSONL preflights");
    let sqlite = sqlite_result.expect("the pinned Field Theory SQLite preflights");
    let preferred = preferred_result.expect("JSONL wins when both Field Theory artifacts are set");

    assert_eq!(
        monolith.source_kind(),
        LegacySourceKind::MonolithBookmarkMetadataCsv
    );
    assert_eq!(
        monolith.source_version(),
        LegacySourceVersion::MonolithBookmarkMetadata
    );
    assert_eq!(monolith.row_count(), 1);
    assert_eq!(monolith.source_digest(), sha256(MONOLITH_CSV.as_bytes()));

    assert_eq!(jsonl.source_kind(), LegacySourceKind::FieldTheoryJsonl);
    assert_eq!(
        jsonl.source_version(),
        LegacySourceVersion::FieldTheoryJsonl1
    );
    assert_eq!(jsonl.row_count(), 2);
    assert_eq!(jsonl.source_digest(), sha256(FIELD_THEORY_JSONL.as_bytes()));

    assert_eq!(sqlite.source_kind(), LegacySourceKind::FieldTheorySqlite);
    assert_eq!(
        sqlite.source_version(),
        LegacySourceVersion::FieldTheorySqlite6
    );
    assert_eq!(sqlite.row_count(), 1);
    assert_eq!(
        sqlite.source_digest(),
        sha256(&std::fs::read(&sqlite_path).expect("the SQLite bytes are readable"))
    );

    assert_eq!(preferred.source_kind(), LegacySourceKind::FieldTheoryJsonl);
    assert_eq!(
        preferred.source_version(),
        LegacySourceVersion::FieldTheoryJsonl1
    );
    assert_eq!(preferred.row_count(), jsonl.row_count());
    assert_eq!(preferred.source_digest(), jsonl.source_digest());
}

#[tokio::test]
async fn credential_bearing_or_writable_source_is_rejected_without_mutation() {
    let fixtures = FixtureDirectory::create().expect("a private synthetic fixture directory");
    let jsonl_path = fixtures.join("credential-bearing.jsonl");
    let sqlite_path = fixtures.join("credential-bearing.db");
    let secret_value = "synthetic-secret-must-never-be-surfaced";
    let jsonl = format!(
        "{{\"id\":\"4001\",\"tweetId\":\"4001\",\"url\":\"https://x.com/synthetic/status/4001\",\"text\":\"Synthetic\",\"syncedAt\":\"2026-02-05T12:00:00Z\",\"accessToken\":\"{secret_value}\"}}\n"
    );
    std::fs::write(&jsonl_path, &jsonl).expect("the credential-shaped JSONL fixture is written");
    create_field_theory_sqlite(&sqlite_path).await;
    add_forbidden_sqlite_column(&sqlite_path).await;
    let jsonl_before = std::fs::read(&jsonl_path).expect("the JSONL fixture bytes are readable");
    let sqlite_before = std::fs::read(&sqlite_path).expect("the SQLite fixture bytes are readable");

    let jsonl_error = LegacyTransitionService::preflight_source(
        LegacySourceSelection::field_theory(Some(jsonl_path.clone()), None),
        SourceLimits::default(),
    )
    .await
    .expect_err("a credential-bearing JSONL source is rejected");
    let sqlite_error = LegacyTransitionService::preflight_source(
        LegacySourceSelection::field_theory(None, Some(sqlite_path.clone())),
        SourceLimits::default(),
    )
    .await
    .expect_err("a credential-bearing SQLite shape is rejected before row reads");

    assert!(
        matches!(
            jsonl_error,
            LegacyTransitionError::ForbiddenField { ref field } if field == "accessToken"
        ),
        "only the rejected JSONL field name is classified: {jsonl_error}"
    );
    assert!(
        matches!(
            sqlite_error,
            LegacyTransitionError::ForbiddenField { ref field } if field == "session_cookie"
        ),
        "only the rejected SQLite field name is classified: {sqlite_error}"
    );
    assert!(!jsonl_error.to_string().contains(secret_value));
    assert!(!format!("{jsonl_error:?}").contains(secret_value));
    assert!(!sqlite_error.to_string().contains(secret_value));
    assert!(!format!("{sqlite_error:?}").contains(secret_value));
    assert_eq!(
        std::fs::read(&jsonl_path).expect("the JSONL fixture remains readable"),
        jsonl_before,
        "preflight never mutates the raw JSONL source"
    );
    assert_eq!(
        std::fs::read(&sqlite_path).expect("the SQLite fixture remains readable"),
        sqlite_before,
        "the SQLite connection is immutable and cannot journal or mutate the source"
    );
}

#[tokio::test]
async fn field_theory_jsonl_rejects_unknown_nested_shape() {
    let fixtures = FixtureDirectory::create().expect("a private synthetic fixture directory");
    let jsonl_path = fixtures.join("unknown-nested-field.jsonl");
    std::fs::write(
        &jsonl_path,
        "{\"id\":\"5001\",\"tweetId\":\"5001\",\"url\":\"https://x.com/synthetic/status/5001\",\"text\":\"Synthetic\",\"syncedAt\":\"2026-02-05T12:00:00Z\",\"author\":{\"handle\":\"synthetic\",\"unreviewedProjection\":true}}\n",
    )
    .expect("the unknown nested shape fixture is written");

    let error = LegacyTransitionService::preflight_source(
        LegacySourceSelection::field_theory(Some(jsonl_path), None),
        SourceLimits::default(),
    )
    .await
    .expect_err("a nested field outside the pinned schema is rejected");
    assert!(matches!(
        error,
        LegacyTransitionError::UnsupportedField { ref field }
            if field == "unreviewedProjection"
    ));
}
