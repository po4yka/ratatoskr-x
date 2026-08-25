# Tasks: oauth-pkce-connection

Every behaviour task is a pair: the first task adds a test that fails for the stated assertion (minimal compiling stubs are allowed so the failure is an assertion, not a compile error), the second makes it pass. A task that cannot start from a failing test states why in one line. Integration tests in sections 2–7 require PostgreSQL at `X_TEST_DATABASE_URL`.

## 1. Workspace dependencies and configuration

- [ ] 1.1 Add crates `crates/x-oauth` and `crates/x-budget` (empty lib skeletons inheriting workspace lints) and exactly-pinned `[workspace.dependencies]`: `aes-gcm`, `rand_core` (feature `OsRng`), `sha2`, `base64`, sqlx `chrono` feature, `reqwest` (`default-features = false`, features `json`,`rustls-tls`), dev-only `wiremock`. Verify `cargo metadata --no-deps` lists seven members and `cargo deny --locked check` accepts the tree. Cannot start from a failing test: manifest and dependency configuration.
- [ ] 1.2 RED: add `crates/x-core/tests/config.rs::oauth_budget_and_security_sections_load_with_documented_defaults` asserting extracted defaults: `oauth.read_scopes` equals `["users.read","tweet.read","bookmark.read","offline.access"]`, `intent_ttl_seconds == 600`, `budgets.request_cap_per_window` and `window_seconds` equal their documented defaults, `security.token_encryption_key` is absent by default, and no variant of the loaded tree mentions a write scope. Make it fail first by adding the three sections as empty structs whose `Default` yields those literal values except a deliberately wrong `intent_ttl_seconds` of 0.
- [ ] 1.3 GREEN: replace the stubs with the real `SecurityConfig`/`OauthConfig`/`BudgetConfig` models (deny_unknown_fields, documented defaults incl. provider URLs and `client_id`/`redirect_uri` required-nonempty validation); the test passes.
- [ ] 1.4 RED: add `crates/x-core/tests/config.rs::malformed_encryption_key_reports_violation`: a key string that is not 32 bytes of base64url must yield `ConfigError::Invalid` with at least one violation naming the setting, while a valid 32-byte base64url key extracts cleanly. Make it fail with a validation stub that accepts any key string.
- [ ] 1.5 GREEN: implement key decoding/validation in `validate()` collecting the violation; the test passes.
- [ ] 1.6 RED: add `crates/x-core/tests/config.rs::secret_key_debug_rendering_redacts_material`: `format!("{:?}", key)` must contain `redacted` and must not contain the raw input. Make it fail with a transparent newtype stub deriving plain `Debug`.
- [ ] 1.7 GREEN: hand-write `Debug` for the key newtype rendering `[redacted]`; the test passes.

## 2. Schema extension (`x_archive`, edited in place)

- [ ] 2.1 RED: update the expected table set in `crates/x-persistence/tests/schema.rs::fresh_database_receives_full_owned_inventory` to seventeen names including `oauth_intents` and `api_budget_windows`; confirm it fails because `schema.sql` still creates fifteen.
- [ ] 2.2 GREEN: edit root `schema.sql` adding `x_archive.oauth_intents` (id, internal_user_id, state_hash UNIQUE, code_verifier_encrypted bytea, nonce, redirect_uri, requested_scopes text[], created_at, expires_at, consumed_at) and `x_archive.api_budget_windows` (account_id, window_start, window_seconds, request_cap, used_requests, PRIMARY KEY (account_id, window_start)); the test passes.
- [ ] 2.3 RED: add `crates/x-persistence/tests/schema.rs::accounts_connection_state_is_constrained`: inserting an account row with state `connected` must succeed and with `disconnected` must be rejected by the CHECK constraint. Confirm it fails because the accounts table has no state column at all (the failure mode the task states).
- [ ] 2.4 GREEN: add `accounts.state text NOT NULL DEFAULT 'connected'` with the six-value CHECK (`connected`,`refresh_required`,`reauth_required`,`revoked`,`suspended`,`paused`); the test passes.
- [ ] 2.5 RED: add `crates/x-persistence/tests/schema.rs::credentials_carry_superseded_refresh_hash_column`: querying the catalog for `credentials.superseded_refresh_hash` of type text must find it; confirm it fails because the column does not exist.
- [ ] 2.6 GREEN: add `credentials.superseded_refresh_hash text`; the test passes.

## 3. Token cipher envelope (`x-oauth`)

- [ ] 3.1 RED: add `crates/x-oauth/tests/cipher.rs::non_32_byte_key_is_refused`: constructing the cipher from a 31-byte and from a malformed base64url key must return the typed key error, and a valid 32-byte key must construct. Make it fail with a constructor stub accepting any byte slice.
- [ ] 3.2 GREEN: implement key parsing/validation with `CipherError::Key`; the test passes.
- [ ] 3.3 RED: add `crates/x-oauth/tests/cipher.rs::each_sealing_uses_a_fresh_nonce`: sealing the same plaintext twice under one key must produce envelopes whose nonce portions differ. Make it fail with a sealing stub that writes a fixed zero nonce.
- [ ] 3.4 GREEN: generate a random 12-byte nonce per sealing from `OsRng`; the test passes.
- [ ] 3.5 RED: add `crates/x-oauth/tests/cipher.rs::tampered_envelope_is_rejected`: flipping any ciphertext bit must make opening fail without producing plaintext. Make it fail with the stub seal that concatenates `version||nonce||plaintext` unauthenticated.
- [ ] 3.6 GREEN: implement real AES-256-GCM sealing/opening with the `v1` format byte; include companion `round_trip_preserves_payload` verified green in the same task (it cannot fail first once authenticated encryption exists, because a correct round trip is implied by authentication).
- [ ] 3.7 RED: add `crates/x-oauth/tests/cipher.rs::envelope_does_not_survive_account_or_purpose_relocation`: an envelope sealed for `(account_a, Credential)` opened as `(account_b, Credential)` or `(account_a, IntentVerifier)` must fail with the binding error. Make it fail by leaving the AAD parameter unused in the stub.
- [ ] 3.8 GREEN: bind account UUID bytes plus ASCII purpose label as AAD; the test passes.

## 4. PKCE and authorization intents (`x-oauth`, `x-persistence`)

- [ ] 4.1 RED: add `crates/x-oauth/tests/pkce.rs::challenge_matches_rfc7636_appendix_vector`: deriving the challenge for the appendix-B verifier must equal `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk`. Make it fail with a derivation stub returning the verifier unchanged.
- [ ] 4.2 GREEN: derive `BASE64URL(SHA256(verifier))` with `sha2` + `base64`; the test passes.
- [ ] 4.3 RED: add `crates/x-oauth/tests/pkce.rs::generated_verifier_and_state_meet_length_and_alphabet_rules`: verifier length must be within 43–128 chars over the base64url alphabet and state must decode to 32 bytes. Make it fail with generator stubs returning fixed short strings.
- [ ] 4.4 GREEN: generate the verifier from 64 random bytes and state from 32 random bytes via `OsRng`, base64url unpadded; the test passes.
- [ ] 4.5 RED: add `crates/x-oauth/tests/pkce.rs::authorization_url_carries_minimized_read_consent_without_leaking_verifier`: the built URL must carry `response_type=code`, client id, redirect URI, `code_challenge_method=S256`, the derived challenge, the state, and a scope list equal to exactly the configured read set joined in order, and must not contain the verifier substring. Make it fail with a URL-builder stub omitting challenge and state.
- [ ] 4.6 GREEN: assemble the authorization URL from configuration and the issued intent; the test passes.
- [ ] 4.7 RED: add `crates/x-persistence/tests/oauth_intents_repo.rs::intent_round_trips_through_digest_lookup`: inserting an intent must allow lookup by SHA-256 digest of the state returning internal-user binding, encrypted verifier, redirect URI, requested scopes, and expiry, and must return none for an unseen digest. Make it fail with repository stubs whose lookup always returns none.
- [ ] 4.8 GREEN: implement the intents repository (insert, digest lookup, transactional consume marking `consumed_at`) against `x_archive.oauth_intents`; the test passes.
- [ ] 4.9 RED: add `crates/x-oauth/tests/callback_matrix.rs` with the four outcome tests sharing one resolver seam: `valid_callback_is_accepted_once_exposing_binding_and_verifier`, `replayed_state_is_rejected_after_consumption`, `expired_intent_is_rejected_and_never_consumed`, `unknown_state_is_unmatchable_without_side_effects`. Make all four fail with a resolver stub that refuses every presentation as unmatched (the first three fail on the refusal they must not get, the fourth fails because the stub leaves no way to distinguish outcomes).
- [ ] 4.10 GREEN: implement callback resolution: digest lookup, expiry check against an injected clock, single transactional consumption, decrypted verifier exposure, and the three distinct refusals; all four tests pass.

## 5. Code exchange and scope audit (`x-oauth`)

- [ ] 5.1 Record synthetic fixtures under `fixtures/oauth/`: `token_success.json`, `token_downgraded_scope.json`, `token_invalid_grant.json`, `refresh_rotated.json`, `revocation_ok.json` shaped after official documentation with placeholder secrets only. Cannot start from a failing test: recorded response data.
- [ ] 5.2 RED: add `crates/x-oauth/tests/exchange.rs::successful_exchange_activates_account_with_encrypted_tokens`: against a wiremock server serving `token_success.json` with an expectation capturing the request body, exchanging an accepted intent must send form fields `grant_type=authorization_code`, the code, redirect URI, and the stored verifier (HTTP Basic client auth when a secret is configured), then store a credential whose payload decrypts to the returned token pair under the account binding, record granted scopes verbatim and expiry, and leave the account connected. Make it fail with an exchange stub that consumes nothing and stores nothing.
- [ ] 5.3 GREEN: implement the token-endpoint adapter over `reqwest` (configurable base URL) and the activation path through the credentials repository; the test passes.
- [ ] 5.4 RED: add `crates/x-oauth/tests/exchange.rs::downgraded_grant_refuses_activation_naming_missing_scopes`: serving `token_downgraded_scope.json` (missing `bookmark.read`) must produce the typed downgrade error naming every missing read scope, leave no active credential, and still make the observed grant auditable in the stored non-active credential row. Make it fail with the stub treating any token response as success.
- [ ] 5.5 GREEN: implement the granted-versus-requested scope diff and the refusal path; the test passes.

## 6. Refresh rotation, reuse detection, revocation (`x-oauth`)

- [ ] 6.1 RED: add `crates/x-oauth/tests/refresh.rs::refresh_rotates_tokens_atomically_retiring_prior_hash`: refreshing with the current refresh token against `refresh_rotated.json` must store the new pair atomically, move the prior token's hash into `superseded_refresh_hash`, and expose no intermediate mixed state. Make it fail with a refresh stub that performs no write.
- [ ] 6.2 GREEN: implement rotation as a guarded CAS update moving the retired hash; the test passes.
- [ ] 6.3 RED: add `crates/x-oauth/tests/refresh.rs::retired_refresh_token_presentation_revokes_family`: presenting the retired token must be refused as reuse, scrub the payload, set credential status revoked, and set the account to reauth-required. Make it fail with the stub classifying every mismatched presentation as stale.
- [ ] 6.4 GREEN: implement the reuse branch (hash comparison against `superseded_refresh_hash`); the test passes.
- [ ] 6.5 RED: add `crates/x-oauth/tests/refresh.rs::unknown_stale_token_is_refused_without_touching_stored_credential`: presenting a token matching neither current nor retired must yield the stale error and leave payload/status/account untouched. Make it fail with a stub that revokes on any mismatch.
- [ ] 6.6 GREEN: implement the stale branch mutating nothing; the test passes.
- [ ] 6.7 RED: add `crates/x-oauth/tests/refresh.rs::upstream_invalidation_differs_from_transport_failure`: an `invalid_grant` body must mark the credential expired and account reauth-required while retaining the evidence payload, whereas a connection error must return the transient variant changing nothing. Make it fail with a stub mapping every failure to the transient variant.
- [ ] 6.8 GREEN: classify provider responses (status/body class) into invalidation versus transport failure with their distinct state effects; the test passes.
- [ ] 6.9 RED: add `crates/x-oauth/tests/revoke.rs::revocation_scrubs_local_material`: revoking an active connection must call the configured revocation endpoint with the current token (captured by wiremock), then empty the payload, set status revoked, and mark the account revoked. Make it fail with a revoke stub that skips both the call and the scrub.
- [ ] 6.10 GREEN: implement the revocation flow; the test passes.
- [ ] 6.11 RED: add `crates/x-oauth/tests/revoke.rs::revocation_is_idempotent_without_repeat_provider_contact`: revoking an already-revoked connection and revoking an account without a credential must succeed with the wiremock expectation requiring zero further calls. Make it fail with the stub calling the provider every time.
- [ ] 6.12 GREEN: add the idempotent early-return branches; the test passes.
- [ ] 6.13 GUARD, cannot start from a failing test because every OAuth error type is value-free by construction (thiserror static messages, no payload interpolation): add `crates/x-oauth/tests/secrecy.rs::error_renderings_exclude_secret_inputs` formatting every flow error with marker secret strings in scope and asserting none appear.

## 7. API budget gate (`x-budget`)

- [ ] 7.1 RED: add `crates/x-budget/tests/gate.rs::reservation_within_cap_counts_durably_across_instances`: reserving cost under the cap must succeed, be visible to an independently constructed gate over the same database, and refuse nothing else until the cap. Make it fail with a gate stub keeping usage in memory only.
- [ ] 7.2 GREEN: implement window accounting in `x_archive.api_budget_windows` through the budget-windows repository (transactional upsert of usage); the test passes.
- [ ] 7.3 RED: add `crates/x-budget/tests/gate.rs::over_cap_reservation_is_blocked_before_any_call_and_charges_nothing`: requesting more than remaining must yield the exhausted outcome carrying the reset time and leave persisted usage unchanged. Make it fail with the stub granting every reservation.
- [ ] 7.4 GREEN: enforce the cap comparison and the refused-nothing-charged semantics; the test passes.
- [ ] 7.5 RED: add `crates/x-budget/tests/gate.rs::expired_window_opens_fresh_zeroed_window`: after advancing the injected clock past the window, a reservation must land in a new window starting at that instant with full allowance, and the superseded window's usage must remain readable unchanged. Make it fail with a stub that keeps charging the original window forever.
- [ ] 7.6 GREEN: implement rollover on expiry preserving historical rows; the test passes.
- [ ] 7.7 RED: add `crates/x-budget/tests/gate.rs::racing_reservations_never_exceed_the_cap`: spawning more concurrent reservations than remaining allowance must accept exactly the remainder in total and refuse the rest with persisted usage equal to the accepted sum. Make it fail against the unlocked read-modify-write of 7.2, which loses updates under concurrency.
- [ ] 7.8 GREEN: serialize same-account reservations with `SELECT … FOR UPDATE` row locking; the test passes.
- [ ] 7.9 RED: add `crates/x-budget/tests/gate.rs::refund_releases_failed_cost_without_driving_usage_negative`: refunding part of a reservation lowers usage by exactly that amount, and refunding more than charged clamps at zero rather than going negative. Make it fail with a stub whose refund is a no-op.
- [ ] 7.10 GREEN: implement bounded refund; the test passes.

## 8. Gate, documentation, archive, integration

- [ ] 8.1 Update `README.md` (status paragraph and milestone wording: OAuth/PKCE, encrypted credentials, and budgets implemented; sync items remain) and `DEVELOPMENT.md` (toolchain paragraph now that Reqwest/Rustls and encrypted credentials exist; gate list unchanged and still byte-identical to ci.yml). Cannot start from a failing test: documentation.
- [ ] 8.2 Run the full local gate with the disposable Postgres container and capture evidence: `git diff --check`, `openspec validate --all --strict`, `cargo fetch --locked`, `cargo deny --locked check`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, file-length ratchet command, `cargo build --workspace --locked`, `cargo test --workspace --locked`, `cargo build --workspace --locked --release`.
- [ ] 8.3 With every box above ticked, commit the branch (Conventional Commits), archive the change via OpenSpec, and verify `openspec validate --archived`.
- [ ] 8.4 Integrate: merge the branch into `main`, push `main` to the remote, delete the worktree and the task branch.
