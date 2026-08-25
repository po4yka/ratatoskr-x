# Design: oauth-pkce-connection

## Context

The scaffold provides a five-member workspace with exactly-pinned dependencies, figment configuration under `RATATOSKR__`, thiserror error types, SQLx persistence owning `schema.sql` application under an advisory lock, and integration tests against disposable PostgreSQL databases. No HTTP client, crypto, base64, or JSON-web dependencies exist yet; everything OAuth needs arrives in this change. The legacy Field Theory connector used browser-session cookies through Playwright; this change replaces that mechanism with the official provider flow and must keep tokens inside the service boundary, encrypted at rest.

## Goals / Non-Goals

**Goals:**

- Every spec scenario backed by a named failing-test-first test, including the callback state matrix, encryption round trip, rotation/reuse rejection, and budget-gate blocking.
- Provider HTTP interaction exercised through recorded synthetic fixtures so tests never touch the live API.
- Secrets (tokens, verifier, key) absent from logs, error renderings, fixtures, and the database in plaintext form.
- Budget enforcement decided before any provider call, durable across restarts.

**Non-Goals:** connect/callback HTTP routes (arrive with Platform wiring), NATS events, write scopes or bookmark mutations, sync workers, key rotation tooling beyond the version byte, OIDC ID-token handling.

## Decisions

### D1. Crate placement

New `crates/x-oauth` holds PKCE derivation, the cipher envelope, the provider token adapter, scope auditing, and flow orchestration. New `crates/x-budget` holds the window gate, independent of OAuth so later sync crates depend on it without pulling crypto or an HTTP client. Both delegate storage to new repository modules in `crates/x-persistence` (`oauth_intents.rs`, `credentials.rs`, `budget_windows.rs`), keeping every SQL statement in the crate that owns schema application, matching how the scaffold isolates database knowledge. Configuration grows in `x-core::config`; `services/x` is untouched because nothing wires these subsystems into the binary until routes/events exist. Alternative considered: repositories as traits implemented beside domain logic — rejected as indirection without a second backend.

### D2. Cipher envelope

AES-256-GCM with the byte-exact envelope layout `0x76 0x01 || 12-byte nonce || ciphertext||tag` (`0x76` is ASCII `v`, `0x01` the format version; asserted by a dedicated test so the layout is pinned, not prose). Each envelope binds one owner plus one purpose inside the authenticated ciphertext body: the body is `purpose-label || owner-UUID-bytes || plaintext`, where credential envelopes carry the account UUID under the label `ratatoskr/x/credential/v1` and authorization-intent verifier envelopes carry the requesting internal-user UUID under `ratatoskr/x/intent-verifier/v1` — an intent precedes any account's existence, so it cannot bind an account. The opener always supplies the expected binding from trusted context (the row's owner), never from the envelope. Binding lives inside the authenticated region rather than as GCM additional data because the typed error contract requires distinguishing tampering (`Auth`: decryption fails) from relocation or purpose confusion (`Binding`: decryption succeeds but the recovered binding disagrees) — a pure-AAD scheme collapses both into one undecryptable failure and cannot emit both variants; the security property is identical, since both mechanisms make the binding tamper-evident and unusable under a different owner or purpose. The key is 32 raw bytes decoded from the base64url value of `RATATOSKR__SECURITY__TOKEN_ENCRYPTION_KEY`; it is typed as a redacting newtype in `x-core` whose `Debug` renders `[redacted]` and whose decrypted payload types likewise never derive secret-revealing `Debug`, is absent from every fixture, and is required at first use — missing/malformed keys yield typed errors, never a generated fallback. An unknown leading format byte yields a typed version error rather than misinterpretation. The leading format pair makes future re-encryption explicit. Alternatives: ChaCha20-Poly1305 (equivalent security; AES-GCM chosen for hardware acceleration on deployment hosts), age/ring (heavier dependency surface than one AEAD primitive requires).

### D3. Intent storage and state handling

An intent row stores `sha256(state)` hex as its unique lookup key — a database leak yields nothing directly usable — plus the encrypted verifier, internal-user binding, redirect URI, requested scope list, nonce, creation/expiry timestamps, and a nullable consumed marker. State and verifier come from `getrandom::fill` (rand_core 0.10 no longer ships an OsRng): 32 random bytes for state and 64 for the verifier, both base64url-no-pad encoded (verifier length 86 chars, within RFC 7636's 43–128 range). Expiry defaults to 600 seconds via `oauth.intent_ttl_seconds`. Lookup by digest makes unknown/expired/consumed three distinct query outcomes feeding the matrix. Consumption is a conditional update `SET consumed_at = $now WHERE id = $1 AND consumed_at IS NULL` inside the acceptance transaction: zero affected rows means replay. Every timestamp the flow later compares against (`created_at`, `expires_at`) is written as a bind parameter derived from the injected clock — never from a `DEFAULT now()` — so expiry decisions are deterministic in tests.

### D4. Provider adapter and fixtures

`reqwest` pinned with `default-features = false`, features `json`,`rustls` (reqwest 0.13 renamed the rustls feature; its default provider is aws-lc-rs, coexisting harmlessly with sqlx's ring stack). Token exchange posts the RFC 6749 form fields; refresh posts `grant_type=refresh_token`; revocation posts the RFC 7009 `token` parameter to the configured revocation URL. Client authentication sends HTTP Basic credentials when `client_secret` is configured, otherwise public-client mode. Per RFC 6749 §4.1.4 a token response that omits the `scope` parameter means granted-equals-requested; an absent scope is therefore recorded as the requested set and never misfires the downgrade check. Base URLs are configuration (`authorize_url`, `token_url`, `revocation_url`) defaulting to the documented X endpoints, so tests point them at a local `wiremock` server. Synthetic response bodies live in committed files under `fixtures/oauth/` (`token_success.json`, `token_scope_omitted.json`, `token_invalid_grant.json`, `token_downgraded_scope.json`, `refresh_rotated.json`, `revocation_ok.json`), shaped after the official documentation with placeholder secrets, loaded by the stubs. This satisfies "recorded fixtures" honestly: they are reviewed recordings of response shapes, not live captures containing real material. Hand-written fakes are unnecessary at this seam because the configurable base URL is the seam; pure decision logic (scope diffing, reuse classification) is tested as functions. Transport-failure tests use connection-refused (a dropped or never-bound endpoint), never timeout-based mocks, which are slow and flaky.

### D5. Rotation, reuse detection, and failure classes

Refresh is serialized per account: the operation opens one transaction, takes `SELECT … FROM credentials WHERE account_id = $1 FOR UPDATE` on the account's active credential row, and holds that row lock across classification, the provider call, and the final update before committing. Holding a row lock across an HTTP call is accepted deliberately here — refreshes are rare, provider latency is seconds, and the lock makes two failure modes impossible that optimistic concurrency would otherwise invite: (a) both callers hitting the provider with the same token, where X answers the loser `invalid_grant`; (b) the loser's post-loss reclassification landing on the retired-hash branch — after the winner commits, the loser's just-presented token *is* the stored superseded hash, so naive restart classification would revoke a healthy family. With serialization, a concurrent refresher simply waits, re-reads under the lock, and presents the now-current token; upstream `invalid_grant` regains its unambiguous meaning.

Under the lock: decrypt the payload, compare the presented refresh token against the decrypted current token and the stored `superseded_refresh_hash` (SHA-256 hex of the retired token). Current match → call the provider → CAS-update the row guarded by `WHERE id = $1 AND status = 'active' AND superseded_refresh_hash IS NOT DISTINCT FROM $2`, writing the new envelope and moving the old hash into `superseded_refresh_hash` (the predicate is belt-and-braces under the lock). Retired-hash match → reuse: scrub payload bytes, set status `revoked`, set account state `reauth_required`. Neither match → stale refusal leaving state untouched. A refresh attempt against a credential whose status is not `active` is refused locally with zero provider contact. Upstream `invalid_grant` marks the credential `expired` and the account `reauth_required` while keeping the (now useless) evidence payload; transport errors return a transient variant touching nothing. This implements ARCHITECTURE §5.4's requirement that revocation, refresh failure, and staleness stay distinguishable.

### D6. Budget windows

One row per account per window in `x_archive.api_budget_windows(account_id, window_start, window_seconds, request_cap, used_requests)` keyed `(account_id, window_start)`; historical rows are immutable once superseded. The gate never selects a "newest row": it computes the target window deterministically from the injected clock as `window_start = floor(epoch_seconds / window_seconds) * window_seconds` and runs, inside one transaction on READ COMMITTED:

```sql
INSERT INTO x_archive.api_budget_windows
       (account_id, window_start, window_seconds, request_cap, used_requests)
VALUES ($1, $2, $3, $4, 0)
ON CONFLICT (account_id, window_start) DO NOTHING;

SELECT window_seconds, request_cap, used_requests
  FROM x_archive.api_budget_windows
 WHERE account_id = $1 AND window_start = $2
   FOR UPDATE;
-- refuse in Rust when used + cost > cap (reset = window_start + window_seconds), else:
UPDATE x_archive.api_budget_windows
   SET used_requests = used_requests + $3
 WHERE account_id = $1 AND window_start = $2;
COMMIT;
```

Every racer at a given instant computes the same `window_start`; `ON CONFLICT DO NOTHING` absorbs the create race (the loser briefly waits on the winner's speculative insertion and no-ops); `FOR UPDATE` serializes spending; each transaction touches exactly one row, so a deadlock cycle is impossible. Refusal charges nothing and carries the reset time. The commit precedes any provider call — callers then make their request. `refund(account, window_start, cost)` is one atomic statement clamping in SQL: `SET used_requests = GREATEST(used_requests - $3, 0)`, releasing cost for failed calls without read-modify-write. Cap and window length are `budgets.request_cap_per_window` and `budgets.window_seconds` with documented defaults; the pool must be sized at least as large as the widest concurrency test so racers contend on the row lock, not the connection pool. Alternative: sliding-window counters — rejected as more state for no stated requirement; fixed windows satisfy "requests per window persisted".

### D7. Schema edits in place

`schema.sql` gains: `accounts.state text NOT NULL DEFAULT 'connected'` with the six-value CHECK; `credentials.superseded_refresh_hash text`; table `oauth_intents` (id, internal_user_id, state_hash unique, code_verifier_encrypted bytea, nonce, redirect_uri, requested_scopes text[], created_at, expires_at, consumed_at); table `api_budget_windows` as in D6. Development-status rules hold: edited in place, no migration machinery. The existing inventory test's expected table set grows from fifteen to seventeen names, which is also the RED step proving the delta landed.

### D8. Configuration model

`XConfig` gains sections following existing conventions (`#[serde(deny_unknown_fields)]`, hand-written `Default`, semantic validation): `security.token_encryption_key: Option<SecretKey>` where `SecretKey` is a redacting newtype; `oauth` (`client_id`, `client_secret`, `redirect_uri` all optional until provisioned, `authorize_url`, `token_url`, `revocation_url`, `read_scopes: Vec<String>` defaulted to the four read scopes, `intent_ttl_seconds` defaulted 600); `budgets` (`request_cap_per_window` defaulted 1000, `window_seconds` defaulted 900). Validation refuses keys that are present but not 32 bytes of valid base64url, empty URL values, empty scope lists, zero TTL/budgets, and empty-string client id or redirect URI when set — while absent credentials stay legal so the service still boots before any account is provisioned; flow construction re-enforces presence at point of use. This keeps existing bootstrap/smoke tests green without weakening the cipher's key requirement.

### D9. New dependencies (exact pins finalized in tasks)

`aes-gcm` (AEAD; RustCrypto, Apache-2.0/MIT), `getrandom` 0.4 as the OS CSPRNG (`rand_core` 0.10 no longer ships `OsRng`, so the rand family is not needed at all), `sha2` (challenge/state hashing), `base64` (already in-tree transitively; promoted to direct pin), `chrono` enabled through sqlx's `chrono` feature for timestamptz mapping, `reqwest` (Rustls), and dev-only `wiremock` for provider stubs. All pass the existing `deny.toml` license allowlist; versions are pinned `=` and locked in the same commit that introduces them.

## Risks / Trade-offs

- [Refresh holds a row lock across the provider HTTP call] → Accepted deliberately (D5): refreshes are rare and provider latency is seconds, while the alternatives — optimistic CAS with loss-recovery classification, or session-level advisory locks pooled across callers — both reintroduce the false-revocation race this design exists to kill. Pool impact is bounded by refresh frequency; documented here as the price of correctness.
- [Reqwest widens the dependency tree] → Mitigated by disabling default features (no native-tls, no blocking runtime); `cargo deny` gates licenses/advisories at the same gate step as today.
- [Fixed windows allow burst-at-boundary doubling] → Accepted for item 2; the gate is the single choke point later items tune, and the persisted window shape does not change if policy switches to sliding windows.
- [Single-generation retired-hash retention misses reuse of older generations] → Provider-side rotation already invalidated those tokens, so presenting them fails upstream as invalid_grant and lands in the classified path anyway; storing full history would retain secret-derived material for no added detection power here.
- [State hashing means a lost DB row cannot be recovered from a callback] → Intents are short-lived conveniences; the user retries the connection, which is the correct outcome for a lost intent.
- [Wiremock-based tests depend on an external crate's behavior] → It is dev-only, matches the WireMock approach named in `docs/TESTING.md`, and fixture files stay reviewable artifacts independent of the stub mechanics.

## Migration Plan

Not applicable: no deployed data exists and migrations are forbidden by development status. Rollback is branch deletion.

## Open Questions

None.
