# CommunityHero headless CLI

This dependency-free Node client coordinates the existing Rust HTTP authority. It does not access the database, provider bridge, model, or browser directly. Preparation prefers the engine preparation endpoint, which runs the server's evidence bundle, research-review, admission, and generated-proposal provenance flow. An exact `404` falls back to the legacy conversation path for older engines; other preparation errors do not fall through. The default `run` mode only prepares new proposals; it cannot approve unless `--autonomous` is present, and it cannot execute unless both `--autonomous` and `--execute` are present.

Every command that reads or changes workspace data requires an explicit account binding:

```powershell
node ./mvp/cli/communityhero.mjs status --account likeavto
node ./mvp/cli/communityhero.mjs sync --account likeavto --wait
node ./mvp/cli/communityhero.mjs prepare --account likeavto --item ITEM_ID
node ./mvp/cli/communityhero.mjs approve --account likeavto --proposal PROPOSAL_ID@1
node ./mvp/cli/communityhero.mjs execute --account likeavto --approval APPROVAL_ID --wait
node ./mvp/cli/communityhero.mjs readback --account likeavto
node ./mvp/cli/communityhero.mjs reconcile --account likeavto --operation OPERATION_ID --wait
```

Provider-wide inspection is a separate bounded, read-only engine job. It never queues model preparation or actions. One invocation runs one bounded slice and records exact coverage plus the provider resume token; continue only by naming that saved file:

```powershell
node ./mvp/cli/communityhero.mjs scan --account likeavto --checkpoint C:/private/likeavto-scan.json --page-size 100 --max-pages 8 --max-items 800 --max-elapsed-ms 30000
node ./mvp/cli/communityhero.mjs scan --account likeavto --resume C:/private/likeavto-scan.json --checkpoint C:/private/likeavto-scan.json
```

The result is called `complete` only when the provider reports `stopReason: exhausted`, `hasMore: false`, `nextResume: null`, no failures, and every coverage window is exhausted. Otherwise it is `incomplete`, even if a bounded job itself completed successfully. The opaque `nextResume` value is passed back unchanged. Held or non-generated comments are not recycled by `scan`; preparation remains an explicit exact-item command, so older held items cannot crowd new items out through an automatic loop.

For a bounded resumable workflow, name the exact item set and keep its checkpoint outside source control:

```powershell
# Prepare only (default).
node ./mvp/cli/communityhero.mjs run --account likeavto --item ITEM_ID --checkpoint C:/private/communityhero-run.json

# Explicitly authorize automatic approval of only proposals created by this run.
node ./mvp/cli/communityhero.mjs run --account likeavto --resume C:/private/communityhero-run.json --autonomous

# Explicitly continue through execution and bounded readback polling.
node ./mvp/cli/communityhero.mjs drain --account likeavto --resume C:/private/communityhero-run.json --autonomous --execute --max-polls 120
```

For the whole observed mixed queue, `queue` refreshes provider state, chooses oldest actionable items, and processes independent slices inside a configurable batch. Engine preparation and exact-revision approval both accept up to 100 items. It imports account policy materials before the first model request. Default mode prepares drafts only and can walk the currently known queue without publishing:

```powershell
node ./mvp/cli/communityhero.mjs queue --account likeavto --checkpoint C:/private/likeavto-queue.json --batch-size 60
```

Publication requires both explicit flags. The queue checkpoint owns a separate child checkpoint for every slice, so restart resumes a known assistant/approval/execution job and never repeats an unknown mutation:

```powershell
node ./mvp/cli/communityhero.mjs queue --account likeavto --resume C:/private/likeavto-queue.json --autonomous --execute --max-cycles 1000
```

This resume form also upgrades a queue checkpoint that already finished in prepare-only mode. It approves and executes only the exact proposal revisions stored in that queue's child checkpoints, without regenerating them or adopting other drafts. Each upgraded slice is refreshed before the next one. Repeating the same resume after execution is a no-op.

Held and failed items are recorded and skipped for that pass so their siblings continue. The command reports verified replies, verified closes without reply, confirmed operation failures, unknown and stale operation outcomes, explicit holds, unresolved transport items, failed slice items, preparation count, coverage, and its stop reason. These categories are disjoint: an unknown durable operation is counted once and is not also counted as an unresolved slice. It claims `known-complete-no-eligible` only after the canonical `sync.openCoverage` all-open record reports both `done: true` and `coverageComplete: true`; `sync.openFrontier` is accepted only as an older-engine fallback. A present canonical record always wins, including when it reports incomplete evidence. Bounded closed-history catch-up does not block current queue work. Pending open pagination continues while its cursor/pages advance. A stalled or unknown frontier, incomplete evidence, unavailable account policy, or the cycle limit produces a stopped result instead of a false empty queue.

The checkpoint records the conversation, assistant job, exact generated proposal revisions, approval, execution job, operations, and reconciliation jobs. It never contains the session cookie or CSRF token. A network failure during a mutation is recorded as an unknown outcome and blocks automatic resume; inspect `proposals`, `status`, and `readback` before deciding what happened. The CLI never blindly resends such a request. It also never adopts unrelated pre-existing proposals into an autonomous run. `drain` reconciles `unknown` operations through the Rust readback authority; it never repeats execution.

`propose` is a lower-level operator command for turning an exact current draft or supplied text into a proposal without a model call. `prepare` and `run` use the full assistant generation path. Neither path publishes by itself.

Local `127.0.0.1:4186` access uses the server's local-owner session. For a configured HTTPS operator endpoint, provide an existing browser/operator session through `COMMUNITYHERO_SESSION` or a protected `--session-file`. A session file can contain the raw Cookie header value or `{"cookie":"..."}`. Secrets are not accepted as command-line values and are never printed.

Progress is emitted as JSON lines on stderr; the final result is JSON on stdout. Polling is bounded by `--poll-ms` and `--max-polls`. Ctrl-C stops client polling without claiming that a server-side job was cancelled.

Current HTTP limitation: the server has no standalone provider readback endpoint. `readback` therefore reports the durable operation ledger, while `reconcile` asks the Rust authority to perform provider readback only for an operation already marked `unknown`.
