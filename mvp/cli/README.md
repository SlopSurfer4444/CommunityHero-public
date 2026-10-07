# CommunityHero headless CLI

## Native runtime maintenance

`maintenance` uses the existing authenticated native status, register-target,
begin and checkpoint routes. It never stops a process, starts a successor, edits
SQL, changes provider auth, or retries a POST. There is no bootstrap/export read.
The current transition's previously recorded invocations remain historical evidence.

Create one reviewed public JSON plan and pin its exact bytes:

```json
{
  "schemaVersion": 1,
  "kind": "communityhero-runtime-maintenance-plan",
  "account": "likeavto",
  "baseUrl": "http://127.0.0.1:4199",
  "currentCore": {"path": "<absolute admitted current core>", "sha256": "<actual sha256>"},
  "currentState": {"path": "<absolute current owner pointer>", "sha256": "<actual sha256>"},
  "initialOwner": {"account": "LikeAvto", "runtimeId": "<actual native runtime ID>", "releaseSha256": "<current core sha256>", "epoch": 1},
  "targetAdmission": {"path": "<absolute reviewed native target admission>", "sha256": "<actual sha256>"},
  "logicalAttemptId": "<stable native transfer attempt ID>",
  "journalDirectory": "<absolute directory reserved for this logical transfer>"
}
```

The placeholders are invalid. Use actual owner/epoch and company pins, and keep
BAW (`baw-russia` / `BAW Russia`) in its own plan and journal. The target admission
is the native `root-reviewed-native-runtime-target-admission` file: its
`installedCore` points to the **future** core. It is separate from `currentCore`.
The maintained client verifies the current core, current pointer and complete
current CLI asset set before importing that core's client. Native registration
performs the target's full release admission; this command cannot manufacture it.

```text
node mvp/cli/communityhero.mjs maintenance --account likeavto --plan <absolute plan> --plan-sha <hash> --action status --invocation observe-01
node mvp/cli/communityhero.mjs maintenance --account likeavto --plan <absolute plan> --plan-sha <hash> --action register-target --invocation register-01
node mvp/cli/communityhero.mjs maintenance --account likeavto --plan <absolute plan> --plan-sha <hash> --action begin --invocation begin-01
node mvp/cli/communityhero.mjs maintenance --account likeavto --plan <absolute plan> --plan-sha <hash> --action checkpoint --invocation checkpoint-01
```

Invoke each step explicitly after inspecting the preceding result. `initialOwner`
is the running owner; checkpoint uses its next epoch. Status defaults to the
initial epoch; pass `--owner-epoch <initial+1>` to observe after begin. Other actions
cannot override the epoch. Existing `--session-file` / `COMMUNITYHERO_SESSION`
authentication is unchanged; secrets never enter the plan or journal.

Maintenance requests default to 600000 ms. `--request-timeout-ms` accepts
1..1800000 ms for this command. A timeout is UNKNOWN, never proof of rollback.
Maintenance injects a builtin `http.request` transport into the admitted client's
existing fetch seam. It accepts only the plan's exact numeric loopback origin,
GET `/api/session` and the four maintenance POST routes. It does not use Undici's
independent 300-second header/body deadlines; the client AbortSignal bounds both
header and body observation. There is no redirect following, connection pool or
transport retry. Request bodies are limited to 64 KiB, responses to 8 MiB, and
headers to 16 KiB. A bound violation after mutation dispatch also remains UNKNOWN.
Journal errors retain only allowlisted phase, cause/name, optional nested cause
and timeout/status numbers, never exception messages, HTTP bodies or headers.
Older admitted clients may omit nested causes; the adapter also promotes a safe
nested socket code into the outer code for those clients. This does not infer
the missing cause of any historical timeout.
The command makes at most one mutation and one immediate reconciliation read;
there is no polling loop or automatic retry. A returned exact drained transfer
can settle a checkpoint even when its HTTP response failed after commit.
Begin or registration uncertainty remains unresolved. Exit4 means unresolved.

`logicalAttemptId` and the journal directory stay fixed across physical
invocations. Each `--invocation` creates a new immutable directory; one fsynced
dispatch slot per logical action prevents a different invocation ID from
replaying a mutation. Unready checkpoint observations do not consume that slot.
Read-only status is available in the same journal after UNKNOWN and while a
mutation owns `active.lock`. A crashed owner leaves that lock and dispatch evidence;
there is no age-based takeover. Reconcile the exact prior attempt before any
explicit operator recovery. Never change a plan/journal path to evade its guard.
`refused-or-incomplete` also requires inspecting the preserved journal: a disk
error after dispatch does not prove that the native mutation failed.

This replaces future release-specific prestop copies, not their historical
evidence. Retain old source pins, intent/ACK/readback/error records and partial
journals. Successor construction, native stop proof and launcher admission stay
separate; `stopAuthorized` is always false here.

This dependency-free Node client coordinates the existing Rust HTTP authority. It does not access the database, provider bridge, model, or browser directly. Preparation prefers the engine preparation endpoint, which runs the server's evidence bundle, research-review, admission, and generated-proposal provenance flow. An exact `404` falls back to the legacy conversation path for older engines; other preparation errors do not fall through. The default `run` mode only prepares new proposals; it cannot approve unless `--autonomous` is present, and it cannot execute unless both `--autonomous` and `--execute` are present.

HTTP requests wait up to 120 seconds by default, shared by the CLI and direct
`CommunityHeroClient` callers. On loopback only, preparation, approval, editorial
review, approval execution and item context-refresh admission POSTs wait up to
300 seconds so a busy local writer can commit their result. Other routes retain
120 seconds. `--request-timeout-ms` (1–3600000 milliseconds) overrides both defaults;
direct clients can set `timeoutMs`. These are per-request deadlines, not job lifetimes.
The checked-in remote proxy has a 300-second read timeout; increasing the client
deadline does not change that proxy ceiling. A timed-out mutation remains UNKNOWN;
increasing the deadline never authorizes replaying a previously uncertain request.

Once a job ID is returned, polling follows that existing job until terminal status
or Ctrl-C. `--poll-ms` sets the interval; optional `--max-polls` bounds observation
and stops with `POLL_LIMIT` (or a stopped bulk result), preserving the checkpoint
and job ID for inspection/resume. This does not fail or cancel the server job.

Preparation exposes each durably admitted group through `prepare.ready` progress
and exact proposal ID/revision/item references in its checkpoint, while other
groups still run. `prepare --resume PATH` observes the original job. A failed
preparation keeps all ready references and never starts another preparation on
resume. Grouped jobs consume only admitted candidates from that same job; older
engines retain their completed-job behavior.

`run --autonomous` drains each ready batch through the existing editorial and
approval checks; `--execute` also executes it immediately. Each batch has a
durable child checkpoint in `PATH.ready/`, journaled by its parent before any
approval. Keep that directory together with the parent for recovery. If no path
is supplied, autonomous run creates one under `~/.communityhero/checkpoints/`
and prints its path in `checkpoint.created` progress. A fresh invocation refuses
to overwrite a checkpoint; use `--resume` with the same selection and server.
Lost mutation responses recover only through the existing exact admission
receipts. Unconfirmed outcomes stop the drain without reposting. A failed
preparation after ready batches returns `prepare-partial-failed`, preserving
each batch's approval/execution outcome separately from preparation failure.

Checkpointed `run`/`drain`, queue children and `bulk` save an execute request ID and
the hash of `{approvalId}` before posting `{requestId}` to the approval's execute
route. On resume, an uncertain admission is inspected through
`GET /api/local-admissions/execute/{requestId}`. Only a committed receipt with the
same key, kind, payload hash and approval, and an existing execute job bound to
that approval, can restore polling. Recovery never repeats the execute POST.
Missing, inaccessible or mismatched receipts remain UNKNOWN; a receipt proves
local job admission, not successful provider operations. The server scopes the
receipt to the authenticated actor and the client checks its account binding.
Legacy uncertain checkpoints keep their previous inspection/HOLD behavior.
Standalone `execute` and `approve --execute` retain their existing unkeyed syntax;
use a checkpointed workflow for durable recovery after a lost response.

New approvals require editorial acceptance of the exact saved proposal revision
and text. `run`/`drain`, queue children and `bulk` request this gate before creating
a new approval. The server reuses valid generation review receipts; an all-reused
batch finishes without another model review. The CLI never changes proposal text
or silently approves a subset when the editor returns `revise` or `hold`.
Bulk with both `--partial-admission` and `--continuation-policy continue-independent`
explicitly permits the editorial-accepted subset to proceed. The original scope,
all held verdicts and a separately hashed approval subset remain in the checkpoint.
The server still checks currentness, authority and the exact editorial receipts.
An `editorial-held` result preserves accepted/reused references and each held
reference, reason and optional suggested text for operator review. After changing
a draft, start a new review of its new exact revision.

For manually created or edited proposals, review them explicitly before `approve`:

```powershell
node ./mvp/cli/communityhero.mjs editorial-review --account likeavto --proposal PROPOSAL_ID@1 --checkpoint C:/private/editorial.json
node ./mvp/cli/communityhero.mjs editorial-review --account likeavto --resume C:/private/editorial.json
```

The editorial checkpoint saves its request ID and exact payload hash before the
POST. Resume inspects `GET /api/local-admissions/editorial/{requestId}` and polls
only the bound `editorial_review` job. Missing receipts remain UNKNOWN, and
completed or running reviews are not submitted again. This requires a server
with the editorial endpoint; there is no older-server bypass. Existing v53
approvals, admitted executions and unknown admission recovery retain their
original paths; they are not retrospectively submitted for model review. Legacy
unapproved drafts pass the new gate when resumed for a new approval.

Every command that reads or changes workspace data requires an explicit account binding:

```powershell
node ./mvp/cli/communityhero.mjs status --account likeavto
node ./mvp/cli/communityhero.mjs sync --account likeavto --wait
node ./mvp/cli/communityhero.mjs context-refresh --account likeavto --item LOCAL_ITEM_ID --wait
node ./mvp/cli/communityhero.mjs prepare --account likeavto --item ITEM_ID
node ./mvp/cli/communityhero.mjs approve --account likeavto --proposal PROPOSAL_ID@1
node ./mvp/cli/communityhero.mjs execute --account likeavto --approval APPROVAL_ID --wait
node ./mvp/cli/communityhero.mjs readback --account likeavto
node ./mvp/cli/communityhero.mjs reconcile --account likeavto --operation OPERATION_ID --wait
```

`context-refresh` asks the Rust authority to fetch and merge the current source
context for exactly one CommunityHero local item. It sends a bodyless POST with
owner/CSRF authority and starts or deduplicates a durable `target_refresh` job;
it does not publish, execute an approval, or start a full sync. The result is
not permission to reuse an old proposal: review the refreshed item and create
and approve exact current revisions separately. If `--wait` reaches its poll
limit, use the printed job ID to inspect or continue polling without another
POST:

```powershell
node ./mvp/cli/communityhero.mjs context-refresh --account likeavto --item LOCAL_ITEM_ID --job JOB_ID --wait
```

The poll-only form verifies that the job is `target_refresh` for that same
local item. A lost or malformed POST response is an unknown mutation outcome;
inspect the server job ledger before deciding whether another launch is needed.
This command requires a server release with the new endpoint; the earlier BAW
v26 live runtime does not have it.

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

For the whole observed mixed queue, `queue` refreshes provider state, chooses oldest actionable items, and asks the server for a byte-aware preparation plan before model work. The plan partitions a selection of at most 100 items into exact subsets and explicit holds; the server rechecks each subset when preparation starts. The queue saves the planned subsets in its checkpoint before launching them. Engine preparation and exact-revision approval both accept up to 100 items, but the evidence byte limit can require smaller subsets. There is no daily item limit. It imports account policy materials before the first model request. Default mode prepares drafts only and can walk the currently known queue without publishing:

```powershell
node ./mvp/cli/communityhero.mjs queue --account likeavto --checkpoint C:/private/likeavto-queue.json --batch-size 60
```

Publication requires both explicit flags. The queue checkpoint owns a separate child checkpoint for every slice, so restart resumes a known assistant/approval/execution job and never repeats an unknown mutation:

```powershell
node ./mvp/cli/communityhero.mjs queue --account likeavto --resume C:/private/likeavto-queue.json --autonomous --execute --max-cycles 1000
```

This ordinary CLI resume form also upgrades a queue checkpoint that already finished in prepare-only mode. It approves and executes only the exact proposal revisions stored in that queue's child checkpoints, without regenerating them or adopting other drafts. The queue refreshes source coverage after the saved group before claiming completion. Repeating the same resume after execution is a no-op.

The native Rust conductor in execute mode explicitly opts in to discovering existing engine-generated, unedited revision-1 drafts inside its durable fixed item manifest. Discovery requires current item/source fingerprints and complete operation history, followed by fresh editorial review and the normal exact-revision bulk approval/execute path. Manual, edited, stale, foreign-owned, ambiguous or already operated drafts remain holds. Adopted drafts keep their original identity and are counted separately from newly prepared proposals. Old non-pristine queue journals retain their original policy. A mixed editorial batch keeps atomic admission: accepted but unsent siblings receive explicit holds. A hard crash that leaves an adopted bulk child's `.lock` produces `CHECKPOINT_LOCKED` and requires owner inspection; it never deletes the lock or guesses another admission.

Preparation concurrency follows the engine's verified `prepareWorkers` capability and configured company worker width; the default remains one. With width greater than one, the conductor persists canonical independent family windows and waits for the current preparation's job/reservation ACK to be saved before starting up to width-minus-one separate preparation-only producers. Same-family byte splits cannot overlap. Sending remains sequential. Every observer is joined on cancellation or UNKNOWN, preserving the original paid child checkpoints. Healthy cohorts also join before the next sender handoff, so a slow producer can delay that handoff. Engines without this capability keep the existing preparation overlap behavior; this capability does not itself authorize publishing or a company configuration change.

For a direct `prepare` or `run` selection, the CLI checks the same advisory plan. If it needs multiple batches or holds an item, use `queue --checkpoint` to process the split without dropping recipients. A lost preparation mutation response remains an unknown outcome; the planner never authorizes a blind retry.

Held and failed items are recorded and skipped for that pass so their siblings continue. The command reports verified replies, verified closes without reply, confirmed operation failures, unknown and stale operation outcomes, explicit holds, unresolved transport items, failed slice items, preparation count, coverage, and its stop reason. These categories are disjoint: an unknown durable operation is counted once and is not also counted as an unresolved slice. It claims `known-complete-no-eligible` only after the canonical `sync.openCoverage` all-open record reports both `done: true` and `coverageComplete: true`; `sync.openFrontier` is accepted only as an older-engine fallback. A present canonical record always wins, including when it reports incomplete evidence. Bounded closed-history catch-up does not block current queue work. Pending open pagination continues while its cursor/pages advance. A stalled or unknown frontier, incomplete evidence, unavailable account policy, or the cycle limit produces a stopped result instead of a false empty queue. When execution is requested, exhausted queue coverage remains visible separately: missing, dispatching, unknown, or incompletely covered operations stop the queue as `operation-outcomes-unresolved`; confirmed failed or stale operations stop it as `operation-failures`. A later resume rechecks unresolved operations without resending execution.

The checkpoint records the conversation, assistant job, exact generated proposal revisions, approval, execution job, operations, and reconciliation jobs. It never contains the session cookie or CSRF token. Selected-item proposal and operation checks use the same bounded server review as the UI and require complete operation coverage before approval or a success claim. A network failure during a mutation is recorded as an unknown outcome and blocks automatic resume; inspect `proposals`, `status`, and `readback` before deciding what happened. The CLI never blindly resends such a request. Ordinary CLI queues do not adopt unrelated pre-existing proposals; the native conductor's bounded opt-in is described above. `drain` reconciles `unknown` operations through the Rust readback authority; it never repeats execution.

`propose` is a lower-level operator command for turning an exact current draft or supplied text into a proposal without a model call. `prepare` and `run` use the full assistant generation path. Neither path publishes by itself.

Local `127.0.0.1:4186` access uses the server's local-owner session. For a configured HTTPS operator endpoint, provide an existing browser/operator session through `COMMUNITYHERO_SESSION` or a protected `--session-file`. A session file can contain the raw Cookie header value or `{"cookie":"..."}`. Secrets are not accepted as command-line values and are never printed.

Progress is emitted as JSON lines on stderr; the final result is JSON on stdout. Ctrl-C interrupts an active polling GET or its wait interval without claiming that a server-side job was cancelled. No polling or interruption path resends an admission POST.

Current HTTP limitation: the server has no standalone provider readback endpoint. `readback` therefore reports the durable operation ledger, while `reconcile` asks the Rust authority to perform provider readback only for an operation already marked `unknown`.

`bulk` executes a finite, reviewed proposal set without generating or adopting new drafts. Its JSON input is an array, or an object containing `proposals`, with exact `{ "id": "PROPOSAL_ID", "revision": 1, "itemId": "ITEM_ID" }` references. Include `itemId` from the review so operation evidence can be queried even when proposal metadata is outside bounded bootstrap history. If omitted, the CLI resolves it only from an exact current proposal revision. Bulk does not refresh the queue or schedule an autonomous server continuation.

```powershell
# Save reviewed scope and policies; no approval or execution occurs.
node ./mvp/cli/communityhero.mjs bulk --account likeavto --proposals-file C:/private/reviewed.json --checkpoint C:/private/bulk.json

# Execute the saved exact scope. Approval defaults to atomic admission.
node ./mvp/cli/communityhero.mjs bulk --account likeavto --resume C:/private/bulk.json --execute --max-polls 120

# Explicitly admit safe subsets and continue independent slices after mixed outcomes.
node ./mvp/cli/communityhero.mjs bulk --account likeavto --proposals-file C:/private/reviewed.json --checkpoint C:/private/partial-bulk.json --partial-admission --continuation-policy continue-independent --execute
```

`--batch-size` is 1–100. `--partial-admission` explicitly selects the server's partial admission API; the result retains every held reference, reason, message and HTTP status. With the default `stop-on-mixed` policy, accepted references execute once and any holds or failed/unresolved operations stop continuation before another slice. `continue-independent` permits later slices after terminal job evidence and complete operation coverage, while preserving UNKNOWN operations without retrying or reconciling them. Missing jobs, incomplete ledger coverage, mutation uncertainty without a bound job, or a polling limit produce a finite stopped result. The summary reports holds, failures, UNKNOWNs and the pending tail separately; finishing other slices never converts UNKNOWN into success.

Each checkpoint binds the company, server, reviewed scope, saved policies and exact slice admission payload hashes. Resume cannot replace them. Local approval request IDs and keyed execution attempt IDs are persisted before their POSTs. Lost responses recover only from matching committed receipts; execute recovery also checks the existing job's approval binding. Old bulk checkpoints with only a local execution attempt ID (no execute protocol/hash) never use it as a server receipt key: they retain bounded existing-job inspection or HOLD. Absence from bounded history never grants permission to resend. `--execute` is required on every mutating invocation.

In explicit partial + independent mode, mixed editorial verdicts no longer hold
back accepted proposals in the same slice. Suggested replacement text is never
applied automatically. `editorialHeld` describes editorial exclusions; `held`
describes later server admission exclusions, such as a changed branch. An all-held
slice makes no approval request. A saved mixed verdict can resume without another
model review; a lost approval/execute response recovers its original exact subset.
Completed historical checkpoints remain completed and are not reopened to send
previously held proposals.

Concurrent access is excluded by a sibling `CHECKPOINT_PATH.lock` file containing process ID, start time and account/server binding. A hard process crash can leave this lock behind. Before resuming, inspect that exact lock and verify the previous owning process has stopped; then remove only that lock file. The CLI deliberately never deletes a possibly active owner's lock automatically. The checkpoint and server ledger remain the recovery evidence.
