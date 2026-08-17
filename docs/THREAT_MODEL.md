# X connector threat model

## Assets

OAuth credentials, private bookmarks/folders, protected post visibility, mutation authority, API credits/rate limits, and compliance state.

## Threats and controls

- **Token theft/account mix-up:** encrypted tokens, PKCE/state, exact user binding, refresh/revoke, no propagation.
- **Scope escalation:** display granted scopes and require separate write consent.
- **Private content leak:** owner authorization, protected-state handling, redacted telemetry.
- **False removal:** only complete snapshot authority; interrupted pagination never commits absence.
- **Duplicate write:** idempotency, current-state check, serialized mutation, audit.
- **Cost/limit exhaustion:** budgets, bounded concurrency, retry-after, checkpoints, incremental policy.
- **Compliance violation/stale content:** explicit revalidation/tombstones and policy-aware retention.
- **Provider payload injection:** schema validation and untrusted text treatment.

Re-review for DMs, account-wide post ingestion, additional write capabilities, streaming/webhooks, or public sharing.
