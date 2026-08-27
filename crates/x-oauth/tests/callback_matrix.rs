//! The callback state matrix: acceptance exactly once, expiry, replay, and an
//! unmatchable state stay four distinct outcomes of one resolver seam.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::Mutex;

use x_oauth::callback::{self, CallbackResolution};
use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::clock::Clock;
use x_persistence::oauth_intents::NewIntent;
use x_persistence::test_support::TestDatabase;

/// A deterministic key so sealed verifier envelopes open the same way everywhere.
fn test_cipher() -> TokenCipher {
    let key = [21u8; 32];
    TokenCipher::new(&key).expect("a valid 32-byte test key")
}

/// A test clock the suite moves by hand.
#[derive(Debug)]
struct MutableClock {
    current: Mutex<chrono::DateTime<chrono::Utc>>,
}

impl MutableClock {
    fn at(instant: &str) -> Self {
        let parsed = chrono::DateTime::parse_from_rfc3339(instant)
            .expect("a fixed RFC 3339 instant")
            .with_timezone(&chrono::Utc);
        Self {
            current: Mutex::new(parsed),
        }
    }
}

impl Clock for MutableClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        *self.current.lock().expect("an uncontended test clock")
    }
}

/// Seeds one intent for `user` with `state` live from `created` to `expires`.
async fn seed_intent(
    db: &x_persistence::database::Database,
    cipher: &TokenCipher,
    user: uuid::Uuid,
    state: &str,
    created: &str,
    expires: &str,
) -> uuid::Uuid {
    let verifier = "seeded-pkce-verifier-value";
    let sealed = cipher.seal(user, Purpose::IntentVerifier, verifier.as_bytes());
    let parse = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value)
            .expect("a fixed instant")
            .with_timezone(&chrono::Utc)
    };
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
    ];
    let state_hash = callback::state_digest(state);
    let intent = NewIntent {
        internal_user_id: user,
        account_id: None,
        purpose: "read_connection",
        state_hash: &state_hash,
        code_verifier_encrypted: Box::leak(sealed.into_boxed_slice()),
        nonce: "intent-nonce-observation",
        redirect_uri: "https://app.example/callback",
        requested_scopes: &scopes,
        created_at: parse(created),
        expires_at: parse(expires),
    };
    x_persistence::oauth_intents::insert_intent(db, &intent)
        .await
        .expect("seeding an intent row succeeds")
}

const CREATED: &str = "2026-08-26T12:00:00Z";
const EXPIRES: &str = "2026-08-26T12:10:00Z";
const LIVE_NOW: &str = "2026-08-26T12:01:00Z";

#[tokio::test]
async fn valid_callback_is_accepted_once_exposing_binding_and_verifier() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let cipher = test_cipher();
    let clock = MutableClock::at(LIVE_NOW);
    let user = uuid::Uuid::from_u128(0x5150);
    let state = "live-callback-state";
    seed_intent(&test.database, &cipher, user, state, CREATED, EXPIRES).await;

    let resolution = callback::resolve_callback(&test.database, &cipher, &clock, state)
        .await
        .expect("resolution queries succeed");

    match resolution {
        CallbackResolution::Accepted(accepted) => {
            assert_ne!(
                accepted.intent_id,
                uuid::Uuid::nil(),
                "the persisted intent id survives"
            );
            assert_eq!(
                accepted.internal_user_id, user,
                "the acceptance exposes the internal-user binding"
            );
            assert_eq!(
                accepted.code_verifier, "seeded-pkce-verifier-value",
                "the acceptance exposes the decrypted verifier"
            );
        }
        other => panic!("a valid callback must be accepted once, got {other:?}"),
    }
}

#[tokio::test]
async fn replayed_state_is_rejected_after_consumption() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let cipher = test_cipher();
    let clock = MutableClock::at(LIVE_NOW);
    let user = uuid::Uuid::from_u128(0x5151);
    let state = "replayed-callback-state";
    seed_intent(&test.database, &cipher, user, state, CREATED, EXPIRES).await;

    let first = callback::resolve_callback(&test.database, &cipher, &clock, state)
        .await
        .expect("the first presentation resolves");
    assert!(
        matches!(first, CallbackResolution::Accepted(_)),
        "the first presentation is accepted: {first:?}"
    );

    let second = callback::resolve_callback(&test.database, &cipher, &clock, state)
        .await
        .expect("the second presentation resolves");
    assert!(
        matches!(second, CallbackResolution::Replayed),
        "the second presentation must be rejected as a replay, got {second:?}"
    );
}

#[tokio::test]
async fn expired_intent_is_rejected_and_never_consumed() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let cipher = test_cipher();
    let clock = MutableClock::at("2026-08-26T12:11:00Z");
    let user = uuid::Uuid::from_u128(0x5152);
    let state = "expired-callback-state";
    seed_intent(&test.database, &cipher, user, state, CREATED, EXPIRES).await;

    let resolution = callback::resolve_callback(&test.database, &cipher, &clock, state)
        .await
        .expect("resolution queries succeed");
    assert!(
        matches!(resolution, CallbackResolution::Expired),
        "an expired intent must be refused as expired, got {resolution:?}"
    );

    // The expired rejection consumed nothing: rewound into the validity window,
    // the same intent still accepts exactly once.
    let rewound = MutableClock::at(LIVE_NOW);
    let later = callback::resolve_callback(&test.database, &cipher, &rewound, state)
        .await
        .expect("the intent stays usable after an expired rejection");
    assert!(
        matches!(later, CallbackResolution::Accepted(_)),
        "an expired rejection must never consume the intent, got {later:?}"
    );
}

#[tokio::test]
async fn unknown_state_is_unmatchable_without_side_effects() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let cipher = test_cipher();
    let clock = MutableClock::at(LIVE_NOW);
    let user = uuid::Uuid::from_u128(0x5153);
    let live_state = "live-state-beside-the-unknown-one";
    seed_intent(&test.database, &cipher, user, live_state, CREATED, EXPIRES).await;

    let unknown = callback::resolve_callback(&test.database, &cipher, &clock, "no-such-state")
        .await
        .expect("resolution queries succeed");
    assert!(
        matches!(unknown, CallbackResolution::Unmatched),
        "an unknown state must be unmatchable, got {unknown:?}"
    );

    // The stub collapses every presentation into one refusal; a live intent must still
    // resolve on its own merits, which is how outcomes stay distinguishable.
    let live = callback::resolve_callback(&test.database, &cipher, &clock, live_state)
        .await
        .expect("the neighbouring live intent still resolves");
    assert!(
        matches!(live, CallbackResolution::Accepted(_)),
        "an unmatched presentation must not disturb the matching machinery, got {live:?}"
    );
}
