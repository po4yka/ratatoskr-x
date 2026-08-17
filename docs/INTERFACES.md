# X connector interfaces

## Inbound

OAuth connect/callback/refresh/revoke, bookmark sync/full snapshot, folder sync, bookmark add/remove, compliance revalidation, legacy import, and operation commands.

## Outbound

Account/bookmark/folder/post/social-source/compliance events, linked-URL extraction requests, Knowledge indexing triggers, and safe progress/results.

## Provider boundary

Typed clients expose pagination tokens, requested expansions/fields, rate-limit/credit metadata, refresh, and mutation results. Raw provider shapes do not become public contracts.

## Rules

Commands contain account/user/operation/idempotency. Full authority is committed only after every page succeeds. Write-back validates granted scopes and current state. External URLs are expanded and passed to Extractor without conflating article and post. Errors distinguish auth/reauth, limits/budget, unavailable content, policy/compliance, invalid response, and transient provider failure.
