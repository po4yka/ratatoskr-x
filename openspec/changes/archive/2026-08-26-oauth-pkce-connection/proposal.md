# OAuth PKCE connection

## Why

The legacy Field Theory pipeline authenticated to X with user-supplied `cookies.txt` files driven through Playwright — fragile, against platform rules, and unable to access bookmarks safely at scale. Implementation plan item 2 of `docs/IMPLEMENTATION_PLAN.md` calls for the deliberate replacement: an official OAuth 2.0 Authorization Code connection with PKCE, so every later sync item (items 3–6) can run against authorized tokens instead of a browser session.

## What Changes

- Add a PKCE authorization-intent lifecycle: generation of `state`, nonce, and RFC 7636 S256 verifier/challenge; a persisted one-time intent record bound to an internal user and an expiry; construction of the provider authorization URL from a minimized read-scope set (`users.read tweet.read bookmark.read offline.access`; write scopes are never requested here).
- Add callback validation as an explicit matrix: unknown, expired, replayed, and valid states each produce distinct typed outcomes; a valid callback is consumed exactly once and releases the verifier for exchange.
- Add token exchange against the X OAuth 2.0 token endpoint over Reqwest/Rustls, keeping provider HTTP shapes inside an adapter; responses are validated and recorded as synthetic fixtures.
- Add encrypted token storage at rest: AES-256-GCM envelopes versioned by a leading format byte, keyed from configuration (`RATATOSKR__SECURITY__TOKEN_ENCRYPTION_KEY`, never logged, never persisted), bound per account through additional authenticated data; tampered ciphertext, wrong keys, and cross-account payload moves are rejected.
- Add granted-scope auditing: the exact scopes returned by the provider are recorded, and a grant missing part of the requested read set fails the connection with a typed downgrade error instead of activating a degraded connection.
- Add refresh with rotation: refreshing swaps access and refresh tokens atomically under optimistic concurrency, retains a hash of the immediately retired refresh token, and treats presentation of that retired token as reuse — revoking the credential family and marking the account `reauth_required`. Provider rejections are classified so upstream invalidation, transient unavailability, and local staleness remain distinguishable.
- Add revocation: a revoke path that informs the provider's revocation endpoint and then scrubs local secret material (payload emptied, status `revoked`, account state `revoked`) while preserving audit columns.
- Add per-account API budget accounting: fixed request windows persisted in `x_archive.api_budget_windows`, with a gate primitive that reserves request cost before any provider call happens and hard-blocks over-budget callers; windows roll over on expiry and usage survives process restarts.
- Extend the owned schema in place (no migrations): new `x_archive.oauth_intents` and `x_archive.api_budget_windows` tables, a connection `state` column on `x_archive.accounts`, and a `superseded_refresh_hash` column on `x_archive.credentials`.
- Extend typed configuration with `oauth` and `budgets` sections plus the redacted security key setting, following the existing figment conventions.

Out of scope: bookmark fetching and synchronization (plan items 3–6), Knowledge integration, HTTP endpoints for connect/callback (they arrive with Platform/eventing wiring), NATS events, write-back.

## Capabilities

### New Capabilities

- `oauth-connection`: how an internal user connects an X account through official OAuth 2.0 with PKCE — intent issuance and single-use consumption, the callback state matrix, code exchange, minimized scopes with audit and downgrade refusal, encrypted at-rest storage, rotation-aware refresh with reuse detection, and revocation that scrubs material.
- `api-budget`: how provider API request cost is accounted per account in durable fixed windows, and how the budget gate blocks work that would exceed the cap before any provider call is made.

### Modified Capabilities

- `x-archive-schema`: the owned table inventory grows by `oauth_intents` and `api_budget_windows`; accounts carry an explicit connection state vocabulary; credentials gain a superseded-refresh-token hash column supporting rotation reuse detection.

## Impact

- New crates `crates/x-oauth` (PKCE, cipher envelope, token adapter, flow orchestration) and `crates/x-budget` (window gate), plus the first repository modules in `crates/x-persistence` targeting the extended schema; configuration grows in `crates/x-core`.
- New exactly-pinned workspace dependencies: `aes-gcm`, `rand_core`/`rand`, `sha2`, `base64`, `reqwest` (Rustls, no default TLS), `chrono` via the sqlx feature flag; `wiremock` as a dev-dependency for recorded-fixture provider stubs. All must pass the `deny.toml` license allowlist.
- `schema.sql` edited in place; the existing inventory integration test and spec requirement updated to the new seventeen-table inventory.
- No cross-repository contract changes; nothing is published to other repositories. No service binary behavior change: `services/x` wiring stays untouched until Platform-facing routes exist.
