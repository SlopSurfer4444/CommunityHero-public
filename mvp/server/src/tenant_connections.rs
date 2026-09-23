//! Tenant and connection authority. No provider I/O, credentials, or implicit pilot grants.
//! All records passed here must come from trusted persistence, never request JSON.
use crate::connectors::{ActionKind, Capabilities, ConnectorBinding, ConnectorKind};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompanyScope {
    pub company_id: String,
    pub workspace_id: String,
}
impl CompanyScope {
    pub fn validate(&self) -> Result<(), Denied> {
        if valid_id(&self.company_id) && valid_id(&self.workspace_id) {
            Ok(())
        } else {
            Err(Denied::InvalidScope)
        }
    }
}
fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role { Owner, Admin, Operator, Viewer }
impl Role {
    pub fn parse(value: &str) -> Result<Self, Denied> {
        match value {
            "owner" => Ok(Self::Owner), "admin" => Ok(Self::Admin),
            "operator" => Ok(Self::Operator), "viewer" => Ok(Self::Viewer),
            _ => Err(Denied::Role),
        }
    }
    fn permits(self, permission: Permission) -> bool {
        match permission {
            Permission::Read => true,
            Permission::Draft | Permission::Approve => self != Self::Viewer,
            Permission::ManageConnections => matches!(self, Self::Owner | Self::Admin),
            Permission::ManageMembers => self == Self::Owner,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission { Read, Draft, Approve, ManageConnections, ManageMembers }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Membership {
    pub actor_id: String,
    pub scope: CompanyScope,
    pub role: Role,
    pub active: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    InvalidScope, Membership, Role, ConnectionScope, StaleBinding,
    ConnectionUnavailable, CredentialsUnavailable, CapabilityUnverified, OAuthState,
}
impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Intentionally does not reveal foreign company/account identities.
        write!(f, "Tenant authority denied: {self:?}")
    }
}
impl std::error::Error for Denied {}

pub fn authorize_member(
    actor_id: &str, scope: &CompanyScope, membership: Option<&Membership>, permission: Permission,
) -> Result<(), Denied> {
    scope.validate()?;
    let member = membership.ok_or(Denied::Membership)?;
    if !valid_id(actor_id) || !member.active || member.actor_id != actor_id || member.scope != *scope {
        return Err(Denied::Membership);
    }
    if !member.role.permits(permission) { return Err(Denied::Role); }
    Ok(())
}

/// An opaque vault record ID, not a token, URL, file path, or environment variable.
/// The resolver must additionally enforce company + workspace + connection scope.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialRef(String);
impl CredentialRef {
    pub fn new(reference: &str) -> Result<Self, Denied> {
        if reference.is_empty() || reference.len() > 128 || !reference.bytes().all(|b|
            b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(Denied::CredentialsUnavailable);
        }
        Ok(Self(reference.to_owned()))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}
impl fmt::Debug for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("CredentialRef([redacted])") }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionStatus { Pending, Active, Expired, Revoked, Disabled, Error }
#[derive(Clone, Debug)]
pub struct Connection {
    pub company_id: String,
    pub binding: ConnectorBinding,
    pub status: ConnectionStatus,
    pub credential_ref: Option<CredentialRef>,
    /// Unix seconds. None means expiry is unknown and fails closed for dispatch.
    pub auth_expires_at: Option<i64>,
    pub capabilities: Capabilities,
    /// Capabilities must be verified again after any binding revision change.
    pub verified_binding_revision: Option<u64>,
}
impl Connection {
    pub fn validate_scope(&self, scope: &CompanyScope) -> Result<(), Denied> {
        scope.validate()?;
        if self.company_id != scope.company_id || self.binding.workspace_id != scope.workspace_id {
            return Err(Denied::ConnectionScope);
        }
        self.binding.validate_scope(&scope.workspace_id, &self.binding.account_id)
            .map_err(|_| Denied::ConnectionScope)
    }
}

/// Additional gate, never a replacement for exact text/context approval, owner fencing,
/// UNKNOWN exclusion, or provider readback. Local completion does not call this function.
pub fn authorize_connection_action(
    actor_id: &str, scope: &CompanyScope, membership: Option<&Membership>,
    connection: &Connection, approved_binding: &ConnectorBinding, action: ActionKind, now: i64,
) -> Result<(), Denied> {
    authorize_member(actor_id, scope, membership, Permission::Approve)?;
    connection.validate_scope(scope)?;
    if connection.binding != *approved_binding { return Err(Denied::StaleBinding); }
    if connection.status != ConnectionStatus::Active { return Err(Denied::ConnectionUnavailable); }
    if now < 0 || connection.credential_ref.is_none() ||
        !connection.auth_expires_at.is_some_and(|expiry| expiry > now) {
        return Err(Denied::CredentialsUnavailable);
    }
    if connection.capabilities.connector != connection.binding.connector ||
        connection.verified_binding_revision != Some(connection.binding.revision) ||
        !connection.capabilities.supports(action) {
        return Err(Denied::CapabilityUnverified);
    }
    Ok(())
}

/// Personal discussion identity is distinct from company knowledge and shared work items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalChatScope {
    pub company: CompanyScope,
    pub actor_id: String,
    pub conversation_id: String,
}
impl PersonalChatScope {
    pub fn authorize(&self, actor_id: &str, membership: Option<&Membership>) -> Result<(), Denied> {
        authorize_member(actor_id, &self.company, membership, Permission::Read)?;
        if self.actor_id != actor_id || !valid_id(&self.conversation_id) { return Err(Denied::Membership); }
        Ok(())
    }
}

/// Server-issued OAuth transaction. Persist only hashes/references; never OAuth codes/tokens.
/// This validator deliberately does not consume state: the durable store MUST atomically
/// claim one pending row before token exchange. No callback handler is enabled by this type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthTransaction {
    pub scope: CompanyScope,
    pub actor_id: String,
    pub session_hash: String,
    pub state_hash: String,
    pub provider: ConnectorKind,
    pub connection_id: String,
    pub binding_revision: u64,
    pub issued_at: i64,
    pub expires_at: i64,
    pub consumed: bool,
}
pub struct OAuthCallback<'a> {
    pub scope: &'a CompanyScope,
    pub actor_id: &'a str,
    pub session_hash: &'a str,
    pub state_hash: &'a str,
    pub provider: ConnectorKind,
    pub connection_id: &'a str,
    pub binding_revision: u64,
    pub now: i64,
}
fn digest(value: &str) -> bool { value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) }
pub fn validate_oauth_callback(
    pending: &OAuthTransaction, callback: &OAuthCallback<'_>, membership: Option<&Membership>,
) -> Result<(), Denied> {
    authorize_member(callback.actor_id, callback.scope, membership, Permission::ManageConnections)?;
    if pending.consumed || pending.scope != *callback.scope || pending.actor_id != callback.actor_id ||
        !digest(pending.session_hash.as_str()) || !digest(pending.state_hash.as_str()) ||
        pending.session_hash != callback.session_hash || pending.state_hash != callback.state_hash ||
        pending.provider != callback.provider || pending.connection_id != callback.connection_id ||
        !valid_id(&pending.connection_id) || pending.binding_revision == 0 ||
        pending.binding_revision != callback.binding_revision || pending.issued_at < 0 ||
        pending.expires_at <= pending.issued_at || pending.expires_at - pending.issued_at > 600 ||
        callback.now < pending.issued_at || callback.now >= pending.expires_at {
        return Err(Denied::OAuthState);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::Support;
    fn scope() -> CompanyScope { CompanyScope { company_id: "company-a".into(), workspace_id: "workspace-a".into() } }
    fn member(role: Role) -> Membership { Membership { actor_id: "alice".into(), scope: scope(), role, active: true } }
    fn connection() -> Connection {
        let mut capabilities = Capabilities::unknown(ConnectorKind::Vk);
        capabilities.publish_reply = Support::Supported;
        Connection {
            company_id: "company-a".into(), binding: ConnectorBinding { id: "conn-1".into(),
                workspace_id: "workspace-a".into(), account_id: "brand-policy-1".into(),
                connector: ConnectorKind::Vk, revision: 7, provider_account_id: "42".into() },
            status: ConnectionStatus::Active, credential_ref: Some(CredentialRef::new("vault-record-7").unwrap()),
            auth_expires_at: Some(2000), capabilities, verified_binding_revision: Some(7),
        }
    }
    fn dispatch(c: &Connection) -> Result<(), Denied> {
        authorize_connection_action("alice", &scope(), Some(&member(Role::Operator)), c, &c.binding, ActionKind::PublishReply, 1000)
    }
    #[test] fn no_global_or_inactive_membership_grants() {
        assert_eq!(authorize_member("alice", &scope(), None, Permission::Read), Err(Denied::Membership));
        let mut m = member(Role::Owner);
        m.scope.company_id = "company-b".into();
        assert_eq!(authorize_member("alice", &scope(), Some(&m), Permission::Read), Err(Denied::Membership));
        m.scope = scope(); m.active = false;
        assert_eq!(authorize_member("alice", &scope(), Some(&m), Permission::Read), Err(Denied::Membership));
        m.active = true;
        assert_eq!(authorize_member("bob", &scope(), Some(&m), Permission::Read), Err(Denied::Membership));
    }
    #[test] fn role_matrix_and_unknown_roles_are_closed() {
        for role in [Role::Owner, Role::Admin, Role::Operator, Role::Viewer] {
            let m = member(role);
            for (permission, expected) in [(Permission::Read,true),
                (Permission::Draft,role != Role::Viewer), (Permission::Approve,role != Role::Viewer),
                (Permission::ManageConnections,matches!(role,Role::Owner|Role::Admin)),
                (Permission::ManageMembers,role == Role::Owner)] {
                assert_eq!(authorize_member("alice", &scope(), Some(&m), permission).is_ok(), expected);
            }
        }
        assert_eq!(Role::parse("superuser"), Err(Denied::Role));
    }
    #[test] fn colliding_provider_and_connection_ids_do_not_cross_companies() {
        let mut c = connection(); assert!(dispatch(&c).is_ok());
        c.company_id = "company-b".into(); assert_eq!(dispatch(&c),Err(Denied::ConnectionScope));
        c.company_id = "company-a".into(); c.binding.workspace_id = "workspace-b".into();
        assert_eq!(dispatch(&c),Err(Denied::ConnectionScope));
    }
    #[test] fn another_connection_or_revision_cannot_inherit_approval() {
        let c = connection();
        for field in ["id","revision","account","providerAccount","provider"] {
            let mut approved = c.binding.clone();
            match field { "id" => approved.id="conn-2".into(), "revision" => approved.revision+=1,
                "account" => approved.account_id="brand-policy-2".into(),
                "providerAccount" => approved.provider_account_id="43".into(),
                _ => approved.connector=ConnectorKind::Instagram }
            assert_eq!(authorize_connection_action("alice",&scope(),Some(&member(Role::Owner)),&c,&approved,ActionKind::PublishReply,1000),Err(Denied::StaleBinding));
        }
    }
    #[test] fn connection_health_expiry_and_capability_are_independent() {
        for status in [ConnectionStatus::Pending,ConnectionStatus::Expired,ConnectionStatus::Revoked,ConnectionStatus::Disabled,ConnectionStatus::Error] {
            let mut c=connection(); c.status=status; assert_eq!(dispatch(&c),Err(Denied::ConnectionUnavailable));
        }
        for expiry in [None,Some(999),Some(1000)] {
            let mut c=connection(); c.auth_expires_at=expiry; assert_eq!(dispatch(&c),Err(Denied::CredentialsUnavailable));
        }
        let mut c=connection(); c.credential_ref=None; assert_eq!(dispatch(&c),Err(Denied::CredentialsUnavailable));
        for support in [Support::Unknown,Support::Unsupported] {
            let mut c=connection(); c.capabilities.publish_reply=support; assert_eq!(dispatch(&c),Err(Denied::CapabilityUnverified));
        }
        let mut c=connection(); c.verified_binding_revision=Some(6); assert_eq!(dispatch(&c),Err(Denied::CapabilityUnverified));
        c.verified_binding_revision=Some(7); c.capabilities.connector=ConnectorKind::Youtube;
        assert_eq!(dispatch(&c),Err(Denied::CapabilityUnverified));
    }
    #[test] fn private_chat_stays_personal_within_company() {
        let chat=PersonalChatScope { company:scope(),actor_id:"alice".into(),conversation_id:"chat-1".into() };
        assert!(chat.authorize("alice",Some(&member(Role::Operator))).is_ok());
        let mut bob=member(Role::Owner); bob.actor_id="bob".into();
        assert_eq!(chat.authorize("bob",Some(&bob)),Err(Denied::Membership));
    }
    #[test] fn credential_reference_has_no_debug_disclosure_or_path_resolution() {
        let reference=CredentialRef::new("record-123").unwrap();
        assert!(!format!("{reference:?}").contains("record-123"));
        assert_eq!(reference.as_str(),"record-123");
        for invalid in ["", "../token", "https://vault/token", "Bearer secret", "TOKEN=value"] {
            assert!(CredentialRef::new(invalid).is_err());
        }
    }
    #[test] fn oauth_replay_session_scope_owner_and_revision_are_bound() {
        let pending=OAuthTransaction { scope:scope(),actor_id:"alice".into(),session_hash:"a".repeat(64),
            state_hash:"b".repeat(64),provider:ConnectorKind::Vk,connection_id:"conn-1".into(),
            binding_revision:7,issued_at:1000,expires_at:1600,consumed:false };
        let scope=scope(); let member=member(Role::Admin);
        let callback=OAuthCallback {scope:&scope,actor_id:"alice",session_hash:&pending.session_hash,
            state_hash:&pending.state_hash,provider:ConnectorKind::Vk,connection_id:"conn-1",binding_revision:7,now:1001};
        assert!(validate_oauth_callback(&pending,&callback,Some(&member)).is_ok());
        for mutation in 0..10 {
            let mut invalid=pending.clone();
            match mutation { 0=>invalid.consumed=true,1=>invalid.scope.company_id="company-b".into(),
                2=>invalid.actor_id="bob".into(),3=>invalid.session_hash="c".repeat(64),
                4=>invalid.state_hash="d".repeat(64),5=>invalid.connection_id="conn-2".into(),
                6=>invalid.binding_revision=8,7=>invalid.provider=ConnectorKind::Youtube,
                8=>invalid.expires_at=1001,_=>invalid.issued_at=1002 }
            assert_eq!(validate_oauth_callback(&invalid,&callback,Some(&member)),Err(Denied::OAuthState));
        }
        let mut operator=member.clone(); operator.role=Role::Operator;
        assert_eq!(validate_oauth_callback(&pending,&callback,Some(&operator)),Err(Denied::Role));
    }
}
