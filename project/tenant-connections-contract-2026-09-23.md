# Company and connection authority — backlog draft

Date: 2026-09-23. Status: **backlog only; not active runtime functionality**.
Owner correction during remediation returned native APIs and self-service
multitenancy to backlog. Current delivery priority is LikeAvto through Angry.Space
performance and reliability. No tenant migration, native connector cutover,
OAuth callback, runtime configuration, or live database change is authorized by
this document.

## Preserved draft

`mvp/server/src/tenant_connections.rs` is an isolated, unintegrated Rust draft.
It is not declared in the server module tree, has not been compiled or tested,
and does not enforce runtime tenant isolation. Its eight test functions describe
intended negative cases, not verified results. Work stopped before integration.
Existing LikeAvto/BAW profiles, operations and approvals were not changed here.

The draft separates company/workspace membership, brand/account policy,
provider connection and a personal operator conversation. Membership defaults
to deny; connection dispatch additionally requires exact approved binding,
company/workspace scope, active status, credential reference, unexpired auth and
capabilities verified for that binding revision. No token or credential value is
present. Capability presence in this draft establishes no native API support.

## Future contract and consumers

- `operator_auth` would resolve persisted membership for the authenticated actor;
  global role text alone would never grant access to a selected company.
- Storage would require composite company/workspace keys and foreign keys for
  members, connections, resources, jobs, knowledge and personal conversations.
  A company can own multiple independently selected social accounts. Brand
  policy and a provider login are separate identities.
- Dispatch would consume `authorize_connection_action` as an additional gate
  alongside exact content/context approval, owner fencing, UNKNOWN exclusion and
  readback. It must reload current membership and connection authority before
  dispatch. Existing approvals must never receive retrospective authority.
- Connector aliases would scope opaque external IDs to the owning connection;
  colliding IDs across companies/accounts must remain distinct. Reconnection or
  provider replacement must not retarget historical approvals or UNKNOWN work.
- Personal assistant conversations require actor plus company/workspace scope.
  Sharing company knowledge does not grant access to colleagues' private chats.

The proposed Owner/Admin/Operator/Viewer role matrix is tentative. Per-connection
member restrictions, membership versioning, last-owner transfer protection,
credential vault resolution and revocation, database constraints, HTTP admission,
quotas and scheduling remain future implementation and acceptance work. No
offline migration was created because no persistence consumer was admitted.

## Self-service OAuth requirements, not an implemented flow

The draft callback validator binds actor, session hash, company/workspace,
provider, selected connection, connection revision and a short expiry to a
server-issued state transaction. A real implementation must generate secure
random state, bind the configured callback/issuer/client and required PKCE
material, then atomically claim a durable pending transaction **before** token
exchange. Merely calling the pure validator cannot prevent concurrent replay.
After exchange, independently verify the authorized external account and its
granted permissions before activating a company-owned connection. Reject a
revoked member, changed owner/binding or unselected account. Never accept the
callback's company/account fields as authority or expose tokens in product data.

Google's official [web-server OAuth guide](https://developers.google.com/identity/protocols/oauth2/web-server)
documents state verification and offline authorization; its [best practices](https://developers.google.com/identity/protocols/oauth2/resources/best-practices)
require handling refresh-token invalidation. These references support the
security contract, not a claim that CommunityHero has implemented YouTube auth.
Meta's official documentation returned HTTP 429 during this source pass and the
VK ID documentation could not be fetched. No current Instagram/VK permission or
OAuth availability claim was admitted. Provider-specific app review, scopes,
token lifetime, selected-resource proof and capability tests must be researched
again when this backlog is explicitly activated.
