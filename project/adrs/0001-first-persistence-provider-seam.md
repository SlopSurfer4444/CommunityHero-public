---
artifact_type: "adr"
product: "CommunityHero"
topic: "first-persistence-provider-seam"
status: "accepted"
created: "2026-07-06"
updated: "2026-09-07"
docs_decision: "updated"
privacy: "no_private_product_data"
related_specs:
  - "project/specs/communityhero-tech-stack.md"
  - "project/specs/communityhero-auth-access-contract-v1.md"
  - "project/specs/communityhero-moderation-policy-and-data-model.md"
  - "project/specs/communityhero-product-surface-map-v1.md"
  - "project/specs/communityhero-ai-prepared-operator-workflow.md"
  - "project/specs/communityhero-canonical-connector-data-model-v1.md"
  - "project/specs/communityhero-db-adapter-decision-v1.md"
---
# ADR 0001: First Persistence And Provider Seam

## Context

CommunityHero now has a fixture-backed Next.js alpha with:

- a stateful cockpit loop;
- shared route surface snapshots;
- provider-neutral auth/access helpers;
- protected app surfaces;
- event-backed analytics and audit fixtures.

The next implementation risk is accidentally wiring the UI directly to a
database, auth provider, social SDK, or AI provider. That would make the current
cockpit harder to test, harder to keep fixture-backed, and harder to change
when the first real connector is chosen.

This ADR decides the seam before implementation. It does not implement a
database, auth provider, OAuth flow, connector, email delivery, queue worker, or
AI provider.

## Decision

CommunityHero will use a port-and-adapter seam around the product core.

The product core remains TypeScript domain/app logic. It owns normalized
CommunityHero concepts such as workspace, user, invite, session, integration,
channel account, work item, post/media, author, classification, policy decision,
draft, action audit, ingestion run, native action job, and metric event.

External systems sit behind explicit adapter ports:

- `CommunityHeroStore` for product persistence;
- `AuthIdentityAdapter` for external identity/session provider integration;
- `ConnectorAdapter` for channel ingestion and source context fetch;
- `NativeActionAdapter` for reply/hide/delete/restore/open-native attempts;
- `AiPolicyAdapter` for preparation of classifications, explanations, drafts and
  explicit non-reply/moderation/evidence-limited decisions;
- `EvidenceProvider` for scoped evidence retrieval, including optional
  AutoKnowledge, Fixture and Null implementations;
- `PrivatePayloadStore` for raw provider payloads and raw private source data.

The durable product-store direction is portable Postgres. The later
[DB adapter decision](../specs/communityhero-db-adapter-decision-v1.md) records
Drizzle ORM/Kit; this ADR's original ORM deferral is historical. Selecting those
tools does not mean a runtime database is connected. This ADR owns the seams
and cross-project data ownership; concrete host/auth and deployment remain
separate decisions.

## Data Ownership

### Product Database

The product database owns normalized, queryable product state:

- `Workspace`;
- `User`, `Invite`, `Session`, `AccessAuditEvent`;
- `Integration`, `ChannelAccount`, `CapabilityProfile`;
- `PostMedia`, `Thread`, `Author`;
- `WorkItem`;
- `Classification`, `PolicyDecision`, `EvidenceReceipt`, `Draft`;
- prepared decision/review state, publication discussion briefs, scoped and
  versioned editorial positions, and exact ready-set approvals with dependency
  revisions (target records, not a claim of existing tables or runtime types);
- `ActionAuditEvent`;
- `IngestionRun`;
- `NativeActionJob`;
- `MetricEvent`.

It stores references, hashes, status fields, idempotency keys, timestamps,
operator-visible summaries, and audit receipts.

CommunityHero owns storage of its comments, prepared work, review, positions,
authorized actions and confirmations through these product records and the
private-payload boundary below. An external reasoning/evidence provider does
not become the product database. Local reference relationships do not imply
shared databases or foreign keys across projects.

### Private Payload Store

Raw external payloads, raw comments, raw author identifiers, source URLs,
profile URLs, provider request/response bodies, cookies, tokens, and secrets do
not belong in repo-visible evidence or public logs.

When raw data must be retained for runtime operation, it lives behind
`PrivatePayloadStore` and product access controls. Product rows store only
references, hashes, digests, redacted summaries, and capability/error codes.

### Auth Provider

The auth provider owns external identity proof and provider session mechanics.

CommunityHero still owns:

- workspace membership;
- app roles;
- app permissions;
- invite lifecycle;
- access audit;
- first allowed app route;
- disabled-user and disabled-workspace behavior.

Provider user ids and session ids are stored as hashes or private references.
App permissions are never inferred from provider metadata alone.

### Social Providers

Social platforms own native objects and native permissions.

CommunityHero owns the normalized work state, policy decision, capability
result, action intent, idempotency key, and audit receipt. The UI never calls a
social SDK directly.

### AI Provider

The AI provider prepares a decision for every freshly observed current comment
before selection: draft reply, explicit no-reply, moderation proposal or precise
evidence-limited result. It may propose public-reply content but does not own
policy approval, authority to send/moderate, native execution or final audit.
Permitted bounded context/media/fact resolution precedes generic human
escalation; detailed semantics belong to the
[prepared workflow](../specs/communityhero-ai-prepared-operator-workflow.md).

Real comment processing through AI requires an explicit privacy/provider
posture. Fixture and synthetic data remain the default development path.

### Portable CommunityOps And Connector Boundary — 2026-09-07

The owner direction relayed by Auto is a target boundary for later extraction,
not evidence of an implemented CommunityOps integration. Source references are
[Auto direction](../../../Angry.Space.Auto-symphony/docs/future-community-intelligence-north-star.md)
and [CommunityOps boundary](../../../Angry.Space.CommunityOps-dev/docs/communityhero-portable-communityops-boundary.md).
Historical task IDs in those documents carry no current execution authority.

The portable reasoning path consumes `ConversationItem`,
`ThreadOrPublicationContext` and `Evidence`, and returns `Decision` plus
`ActionIntent`. CommunityOps owns its reusable decision/runtime-policy
mechanisms; CommunityHero owns its product workflow, interface and storage.
An intent crosses a separate, explicitly controlled execution boundary. A
connector owns authentication, provider object mappings, limits, capabilities,
concrete writes and independent result verification. The UI does not call
Angry.Space/ProviderLab or social SDKs directly.

Angry.Space/ProviderLab is a replaceable compatibility adapter and proving
ground. The neutral kernel contains no Angry.Space queue statuses, provider
object IDs, bridge mechanics, universal `close`, or BAW TikTok reply-length
rule. Provider-native IDs may remain in isolated connector mappings/product
ingestion records, but the kernel receives opaque scoped identities. Internal
completion in CommunityHero is distinct from an actual native close or
mark-handled operation; Telegram keeps its own action semantics.

Candidate reusable mechanisms include context/media acquisition and reuse,
bounded missing-fact/specialist work, evidence/quality checks, client/platform
isolation, provenance and versioning, deduplication, history/recovery, and
controlled per-item execution with independent verification. Reuse within
Angry.Space does not establish universality. Before extraction, remove private
data, client facts/contacts, UI behavior and API specifics; name the supported
scope and test both compatible and incompatible examples. Nonportable behavior
stays in the client or platform layer.

Client onboarding primarily supplies accepted rules/knowledge and
configuration; channel onboarding supplies a distinct reusable connector.
An owner correction is a proposed scoped rule, not automatic global learning.
No rule/source cleanup or cross-repository transfer occurs merely because this
boundary is recorded.

### AutoKnowledge Evidence Port

The target contract is
`EvidenceProvider.retrieve(EvidenceRequest) -> autoknowledge.evidence-bundle.v1`.
Requests state the exact tenant, publication/thread/topic/question, allowed
source scope and freshness needs. Bundles carry provenance, freshness,
contradictions, citations and exact applicability. Versioned `EvidenceBundleRef`
references identify the consumed evidence; they are not database foreign keys
or a route to another project's internal rows/schema.

AutoKnowledge is a separate provider of information and owns its evidence and
retrieval implementation. It does not own CommunityHero decisions, editorial
positions, approvals or execution authority. There is no shared database,
cross-project foreign key or shared persistence schema. CommunityHero may cache
appropriately scoped evidence while preserving provenance and access controls.

Fixture and Null implementations keep the core runnable without AutoKnowledge.
Unavailable enrichment reduces the result's completeness; it does not disable
the entire product or manufacture action readiness. A decision-critical gap
still produces bounded clarification/partial work and an explicit limit.

### Portability Verification Required Before Transfer

These are future acceptance cases, not executed tests or accepted extractions:

| Case | Positive example | Incompatible counterexample to reject |
| --- | --- | --- |
| LikeAvto and BAW Russia | Reuse context acquisition and provenance checks with distinct accepted client packs | Copy LikeAvto commercial facts/contacts into BAW, or promote a BAW TikTok limit into the kernel |
| Telegram | Prepare a scoped reply/no-reply decision and map allowed effects through its own connector | Translate an Angry.Space queue close into a fabricated universal Telegram close |
| Legacy adapter removed | Deterministic core fixtures run with a neutral connector double | Kernel requires an Angry.Space account, queue label, provider ID or bridge |
| AutoKnowledge unavailable | Fixture/Null yields useful but explicitly less-enriched work | Shared DB/FK dependency, whole-product failure, or invented evidence/readiness |
| Revised facts or positions | Only dependent unpublished work changes; manual edits and receipts survive | Cross-client propagation, rewriting published history or silently reusing an old approval |

## Transaction Boundaries

The first persistence implementation should keep the local state transitions
in these flows atomic, with external execution separated from the transaction:

- accept invite -> create/activate user -> create session -> write access audit;
- ingest source event -> upsert source context/work item -> write ingestion
  counters -> preserve idempotency;
- move/close/pin/assign/note work item -> update internal state -> append audit;
- approve exact ready versions -> persist intent/job -> separately execute
  adapter per item -> append independently verified or uncertain result;
- correct AI/policy result -> preserve original result -> write correction
  signal/audit -> update derived metric event.

No external native mutation should occur without an idempotency key and an
append-only audit result.

An external side effect is not atomic with a product database transaction.
Persist intent before execution, preserve uncertain outcomes across crashes,
and reconcile before retrying; never roll back a receipt to pretend the
external effect did not happen. A fact/rule/position revision invalidates the
affected unpublished ready-set bindings without rewriting historical approvals.

## Fixture Fallback

The current fixtures stay valuable and must not be thrown away.

The first implementation after this ADR should keep a fixture adapter that
implements the same read model shape as the future `CommunityHeroStore`.
Fixture-backed tests should prove:

- route surfaces do not depend on provider SDKs;
- auth/access checks do not require a live provider;
- connector capability caveats can be rendered without live tokens;
- audit and metric derivation can run without raw private payloads.
- the neutral kernel runs after removal of the Angry.Space adapter;
- Fixture/Null evidence works without AutoKnowledge or a shared database.

## Historical Implementation Options (2026-07-06)

The original options below are historical planning context, not a current
instruction to start work. Current source, later decisions and a new owner
scope govern any future implementation:

1. `persistence-port-contract`: define TypeScript store/adapter interfaces,
   repository result shapes, and fixture implementations. No database package,
   no migrations, no live provider.
2. `schema-and-migration-spike`: choose concrete database/ORM/migration tools
   and create the first schema/migration. This needs a fresh lifecycle decision
   because it adds dependencies and durable storage.

The original recommendation was `persistence-port-contract`.

It keeps the current app running, reduces future rewrites, and lets code move
from fixture globals toward an explicit store seam before committing to a
vendor.

## Deferred Decisions

- Concrete Postgres host.
- Concrete deployment/migration rollout (ORM/Kit choice is recorded by the
  later DB adapter decision).
- Auth provider and login method.
- Email delivery provider.
- Queue/worker runtime.
- Realtime transport.
- First live connector sandbox.
- AI provider/privacy posture for real company comments.
- Runtime retention policy for raw private payloads.

## Consequences

Positive:

- UI and domain code stay testable without external services.
- Fixture alpha remains a first-class development mode.
- Provider changes do not force cockpit rewrites.
- Privacy boundaries are explicit before real comments or credentials exist.
- Audit, idempotency, and capability gates stay product concepts, not adapter
  side effects.

Costs:

- More interface design before the first database table.
- The next implementation slice must resist adding a vendor too early.
- Some route code will need a small refactor from fixture globals toward store
  reads.

## Acceptance

This ADR is accepted when:

- the seam names product-owned state versus provider-owned state;
- fixture fallback remains explicit;
- raw private payloads are separated from product rows and repo-visible
  evidence;
- provider work is deferred;
- the next legal implementation route is named.
