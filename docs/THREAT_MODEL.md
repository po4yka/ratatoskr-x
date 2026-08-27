# X connector threat model

## Assets

OAuth credentials, private bookmarks/folders, protected post visibility, mutation authority, API credits/rate limits, and compliance state.

## Threats and controls

- **Token theft/account mix-up:** encrypted tokens, PKCE/state, exact user binding, refresh/revoke, no propagation.
- **Scope escalation:** the default OAuth intent remains read-only; write re-consent is separately
  account-bound, identity-checked, and accepted only with the complete read-plus-`bookmark.write`
  grant, while posting/liking/reposting/folder/bulk methods do not exist.
- **Private content leak:** owner authorization, protected-state handling, redacted telemetry.
- **False removal:** only complete snapshot authority; interrupted pagination never commits absence.
- **Forged or replayed approval:** immutable expiring consent binds owner/account/action/target/time/
  surface and is atomically consumed once; OAuth scope alone is insufficient.
- **Duplicate or uncertain write:** durable account-scoped idempotency fingerprint elects one
  provider attempt; exact retries return stored state, and possible acceptance is reconciled only by
  complete snapshot authority rather than blind retry.
- **Audit/diagnostic leakage:** bounded typed request/rate evidence only; tokens, Authorization
  headers, raw responses, idempotency keys, and private post content are never stored or rendered.
- **Budget borrowing:** every provider mutation reserves the hard bookmark-write class immediately
  before contact; read/sync allowance cannot fund or be reduced by writes.
- **Cost/limit exhaustion:** budgets, bounded concurrency, retry-after, checkpoints, incremental policy.
- **Compliance violation/stale content:** explicit revalidation/tombstones and policy-aware retention.
- **Provider payload injection:** schema validation and untrusted text treatment.

Re-review for DMs, account-wide post ingestion, additional write capabilities, streaming/webhooks, or public sharing.
