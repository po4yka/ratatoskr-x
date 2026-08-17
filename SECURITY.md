# Security Policy for Ratatoskr X

Report vulnerabilities privately. Do not publish access/refresh tokens, private bookmarks, protected posts, personal exports, production API payloads, or usage/billing information.

Security review is required for OAuth PKCE/state, token encryption/refresh/revoke, scope changes, write-back, provider compliance, private/protected content, account binding, rate/credit budgets, and audit.

Baseline: least-privilege read scopes; write scope requested separately; credentials remain only in this service; no token logs/events; authorize every account/bookmark query; bounded retries/concurrency; revalidate upstream availability according to provider policy; explicit user consent for external mutations.
