## Context

See [proposal.md](proposal.md). `x-sync` already persists complete snapshots through the official-envelope source seam, uses `BudgetGate` before every page, and gives absence authority only to atomically completed runs. It has no partial-run state, scheduler grammar, or repair ledger.

## Goals / Non-Goals

**Goals:**

- Keep incremental discovery separate from full-snapshot absence authority.
- Persist the watermark, escalation state, terminal scan outcome, and repair evidence in the owned first-version schema.
- Make all provider access bounded before contact and deterministic in the disposable PostgreSQL integration harness.

**Non-Goals:**

- Provider HTTP wiring, folders, write-back, retries/backoff policy, events, or external scheduler transport.

## Decisions

### D1: Watermark is a provider post identity observed in scan order

The incremental source returns newest-first pages. The service stops only after observing the committed provider post identifier; it advances to the first observed post only after the run reaches that marker. A page cap reached first is a gap, because neither parsing IDs nor relying on publication timestamps can prove coverage. This keeps timestamp semantics truthful.

### D2: Partial scans only upsert observations

An incremental scan uses the same normalization/persistence path as a full scan, but writes active observations directly and never marks missing rows removed. The account state holds `requires_full_snapshot`; gap, request-cap, source failure, or budget refusal leaves the old watermark in place. A successful full completion clears that state.

### D3: Run and account caps are both enforced before fetches

The operation receives `incremental_page_cap` and `incremental_request_cap`; construction rejects a request cap that is not strictly lower than the configured full-snapshot cap. Before each request, it checks remaining per-run capacity and reserves the durable budget. The lower of these boundaries ends the run before another source call.

### D4: Repair evidence derives only from full reconciliation

Full finalization compares the prior active bookmark projection against its staged candidate set. It stores a repair row keyed by `(completed_snapshot_id, bookmark_id)` whenever it corrects state after a partial-scan era. The unique key makes replay safe; a repeated finalization cannot duplicate evidence.

### D5: Scheduler grammar stays a typed in-process boundary

`x-sync` consumes the existing scheduler type `x.bookmarks.scan_requested.v1` (the scheduler prefixes it with `cmd.` when publishing) with a typed account payload. The executor selects a full snapshot when account state requires it. It does not add an HTTP route or scheduler client, which keeps platform transport ownership outside this bounded context.

## Risks / Trade-offs

- [The provider reorders items] → only a previously observed provider identifier proves the scan has crossed the watermark; uncertain coverage escalates.
- [Incremental budget is depleted] → the scan stays non-authoritative, contacts no further page, and the next scheduler run retries from the old watermark.
- [A full finalization is retried after uncertain process state] → unique repair evidence and existing completion-state checks avoid duplicate repairs.

## Migration Plan

Development status forbids migrations. The change edits `schema.sql` in place and extends disposable-database catalog tests. A failed deployment uses the prior application binary against a disposable development database; no durable-production-data compatibility path exists in this phase.
