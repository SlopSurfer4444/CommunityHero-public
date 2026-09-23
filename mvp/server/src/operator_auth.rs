//! Small, fail-closed access-key authentication for the private operator pilot.
//! Network trust (Host, TLS proxy and Origin) is enforced by the HTTP layer.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const COOKIE: &str = "__Host-communityhero_session";
const SESSION_SECONDS: i64 = 8 * 60 * 60;
const LOGIN_WINDOW: Duration = Duration::from_secs(300);
const LOGIN_LIMIT: usize = 30;

#[derive(Clone)]
pub struct Actor {
    pub id: String,
    pub name: String,
    pub role: String,
    pub csrf_token: String,
    // Separate from browser-session lifetime; never include in public_json/Debug.
    pub(crate) authority_generation: Option<String>,
}
impl std::fmt::Debug for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Actor")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("role", &self.role)
            .field("csrf_token", &"[redacted]")
            .finish()
    }
}
impl Actor {
    pub fn local_owner(csrf_token: impl Into<String>) -> Self {
        Self {
            id: "local-owner".into(),
            name: "Владелец".into(),
            role: "owner".into(),
            csrf_token: csrf_token.into(),
            authority_generation: None,
        }
    }
    pub fn public_json(&self) -> Value {
        json!({"id":self.id,"name":self.name,"role":self.role})
    }
    pub fn valid_csrf(&self, supplied: &str) -> bool {
        constant_time_equal(&hash(supplied), &hash(&self.csrf_token))
    }
}

pub struct Login {
    pub actor: Actor,
    pub set_cookie: String,
}

#[derive(Debug, PartialEq)]
pub enum AuthError {
    InvalidCredentials,
    RateLimited,
    Unavailable,
}

#[derive(Clone)]
pub struct Auth {
    access_file: PathBuf,
    db: SqlitePool,
    attempts: Arc<Mutex<VecDeque<Instant>>>,
}

struct Operator {
    id: String,
    name: String,
    token_hash: String,
}

impl Auth {
    /// No access file means remote access is disabled, not anonymous access.
    pub async fn load(data_dir: &Path) -> Result<Option<Self>, String> {
        let Some(path) = std::env::var_os("COMMUNITYHERO_ACCESS_FILE") else {
            return Ok(None);
        };
        if path.is_empty() {
            return Err("COMMUNITYHERO_ACCESS_FILE is empty".into());
        }
        Self::open(data_dir, PathBuf::from(path)).await.map(Some)
    }

    pub(crate) async fn open(data_dir: &Path, access_file: PathBuf) -> Result<Self, String> {
        read_operators(&access_file)
            .await
            .map_err(|_| "Invalid operator access configuration".to_string())?;
        tokio::fs::create_dir_all(data_dir)
            .await
            .map_err(|_| "Cannot create session directory".to_string())?;
        let options = SqliteConnectOptions::new()
            .filename(data_dir.join("operator-sessions.sqlite"))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let db = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .map_err(|_| "Cannot open operator session store".to_string())?;
        sqlx::query("CREATE TABLE IF NOT EXISTS operator_sessions (session_hash TEXT PRIMARY KEY, operator_id TEXT NOT NULL, credential_hash TEXT NOT NULL, expires_at INTEGER NOT NULL, created_at INTEGER NOT NULL)")
            .execute(&db).await.map_err(|_| "Cannot initialize operator sessions".to_string())?;
        Ok(Self {
            access_file,
            db,
            attempts: Arc::new(Mutex::new(VecDeque::new())),
        })
    }

    pub async fn login(&self, token: &str) -> Result<Login, AuthError> {
        {
            // Global rather than trusting spoofable forwarded IPs. Bounded memory.
            let now = Instant::now();
            let mut attempts = self.attempts.lock().await;
            while attempts
                .front()
                .is_some_and(|t| now.duration_since(*t) >= LOGIN_WINDOW)
            {
                attempts.pop_front();
            }
            if attempts.len() >= LOGIN_LIMIT {
                return Err(AuthError::RateLimited);
            }
            attempts.push_back(now);
        }
        if token.len() < 32 || token.len() > 512 {
            return Err(AuthError::InvalidCredentials);
        }
        let supplied = hash(token);
        let operators = read_operators(&self.access_file).await?;
        // Compare every entry; no early-exit based on secret bytes or operator index.
        let mut matched = None;
        for operator in operators {
            if constant_time_equal(&operator.token_hash, &supplied) {
                matched = Some(operator);
            }
        }
        let operator = matched.ok_or(AuthError::InvalidCredentials)?;
        let secret = random_secret();
        let now = chrono::Utc::now().timestamp();
        let mut tx = self.db.begin().await.map_err(|_| AuthError::Unavailable)?;
        sqlx::query("DELETE FROM operator_sessions WHERE expires_at <= ?")
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|_| AuthError::Unavailable)?;
        // Retain at most 20 concurrent browser sessions per operator.
        sqlx::query("DELETE FROM operator_sessions WHERE operator_id = ? AND session_hash NOT IN (SELECT session_hash FROM operator_sessions WHERE operator_id = ? ORDER BY created_at DESC, rowid DESC LIMIT 19)")
            .bind(&operator.id).bind(&operator.id).execute(&mut *tx).await.map_err(|_| AuthError::Unavailable)?;
        sqlx::query("INSERT INTO operator_sessions (session_hash, operator_id, credential_hash, expires_at, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(hash(&secret)).bind(&operator.id).bind(&operator.token_hash).bind(now + SESSION_SECONDS).bind(now)
            .execute(&mut *tx).await.map_err(|_| AuthError::Unavailable)?;
        tx.commit().await.map_err(|_| AuthError::Unavailable)?;
        Ok(Login {
            actor: actor(&operator, &secret),
            set_cookie: format!(
                "{COOKIE}={secret}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_SECONDS}"
            ),
        })
    }

    pub async fn authenticate(&self, cookie_header: &str) -> Result<Option<Actor>, AuthError> {
        let Some(secret) = session_secret(cookie_header) else {
            return Ok(None);
        };
        let row = sqlx::query("SELECT operator_id, credential_hash FROM operator_sessions WHERE session_hash = ? AND expires_at > ?")
            .bind(hash(secret)).bind(chrono::Utc::now().timestamp()).fetch_optional(&self.db).await.map_err(|_| AuthError::Unavailable)?;
        let Some(row) = row else { return Ok(None) };
        let id: String = row
            .try_get("operator_id")
            .map_err(|_| AuthError::Unavailable)?;
        let credential_hash: String = row
            .try_get("credential_hash")
            .map_err(|_| AuthError::Unavailable)?;
        // Read the current allow-list on each request: removal or key rotation revokes
        // already issued sessions, including across server restarts.
        let operators = read_operators(&self.access_file).await?;
        Ok(operators
            .iter()
            .find(|o| o.id == id && constant_time_equal(&o.token_hash, &credential_hash))
            .map(|operator| actor(operator, secret)))
    }

    pub async fn logout(&self, cookie_header: &str) -> Result<(), AuthError> {
        if let Some(secret) = session_secret(cookie_header) {
            sqlx::query("DELETE FROM operator_sessions WHERE session_hash = ?")
                .bind(hash(secret))
                .execute(&self.db)
                .await
                .map_err(|_| AuthError::Unavailable)?;
        }
        Ok(())
    }

    /// Recheck the allow-list immediately before dispatch, without extending or
    /// consulting a browser session. Session expiry/logout blocks new requests;
    /// removal or credential rotation also invalidates already admitted work.
    pub(crate) async fn authority_is_current(
        &self,
        id: &str,
        generation: &str,
    ) -> Result<bool, AuthError> {
        let operators = read_operators(&self.access_file).await?;
        Ok(operators.iter().any(|operator| {
            operator.id == id && constant_time_equal(&authority_generation(operator), generation)
        }))
    }

    pub fn clear_cookie() -> &'static str {
        "__Host-communityhero_session=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0"
    }
}

fn actor(operator: &Operator, secret: &str) -> Actor {
    Actor {
        id: operator.id.clone(),
        name: operator.name.clone(),
        role: "operator".into(),
        csrf_token: hash(&format!("communityhero-csrf-v1:{secret}")),
        authority_generation: Some(authority_generation(operator)),
    }
}
fn authority_generation(operator: &Operator) -> String {
    // Domain separation keeps this binding distinct from the configured access-
    // key hash. Neither raw access keys nor their configured hashes leave Auth.
    hash(&format!(
        "communityhero-dispatch-authority-v1:{}:{}",
        operator.id, operator.token_hash
    ))
}
fn random_secret() -> String {
    // UUID v4 uses the OS CSPRNG. Three provide >256 random bits before SHA-256.
    hash(&format!(
        "{}{}{}",
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    ))
}
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn constant_time_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0_u8, |diff, (x, y)| diff | (x ^ y))
        == 0
}
fn session_secret(cookie_header: &str) -> Option<&str> {
    if cookie_header.len() > 8192 {
        return None;
    }
    let mut values = cookie_header.split(';').filter_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == COOKIE).then_some(value)
    });
    let value = values.next()?;
    if values.next().is_some() || value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some(value)
}
async fn read_operators(path: &Path) -> Result<Vec<Operator>, AuthError> {
    if tokio::fs::metadata(path)
        .await
        .map_err(|_| AuthError::Unavailable)?
        .len()
        > 32_768
    {
        return Err(AuthError::Unavailable);
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| AuthError::Unavailable)?;
    let config: Value = serde_json::from_slice(&bytes).map_err(|_| AuthError::Unavailable)?;
    let entries = config["operators"]
        .as_array()
        .ok_or(AuthError::Unavailable)?;
    if entries.len() > 32 {
        return Err(AuthError::Unavailable);
    }
    let mut ids = HashSet::new();
    let mut hashes = HashSet::new();
    entries
        .iter()
        .map(|entry| {
            let id = entry["id"].as_str().ok_or(AuthError::Unavailable)?;
            let name = entry["name"].as_str().ok_or(AuthError::Unavailable)?;
            let token_hash = entry["tokenHash"]
                .as_str()
                .ok_or(AuthError::Unavailable)?
                .to_ascii_lowercase();
            if id.is_empty()
                || id.len() > 64
                || id == "local-owner"
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || name.trim().is_empty()
                || name.len() > 128
                || name.chars().any(char::is_control)
                || token_hash.len() != 64
                || !token_hash.bytes().all(|b| b.is_ascii_hexdigit())
                || !ids.insert(id.to_owned())
                || !hashes.insert(token_hash.clone())
            {
                return Err(AuthError::Unavailable);
            }
            Ok(Operator {
                id: id.to_owned(),
                name: name.to_owned(),
                token_hash,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    async fn fixture() -> (tempfile::TempDir, Auth) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.json");
        tokio::fs::write(
            &path,
            json!({"operators":[{"id":"alice","name":"Alice","tokenHash":hash(TOKEN)}]})
                .to_string(),
        )
        .await
        .unwrap();
        let auth = Auth::open(dir.path(), path).await.unwrap();
        (dir, auth)
    }
    #[tokio::test]
    async fn session_survives_restart_has_csrf_and_logout_revokes() {
        let (dir, auth) = fixture().await;
        let login = auth.login(TOKEN).await.unwrap();
        assert!(
            login
                .set_cookie
                .contains("HttpOnly; Secure; SameSite=Strict")
        );
        let cookie = login.set_cookie.split(';').next().unwrap();
        let persisted: (String, String) =
            sqlx::query_as("SELECT session_hash, credential_hash FROM operator_sessions")
                .fetch_one(&auth.db)
                .await
                .unwrap();
        assert_ne!(persisted.0, session_secret(cookie).unwrap());
        assert_eq!(persisted.1, hash(TOKEN));
        assert!(!format!("{:?}", login.actor).contains(&login.actor.csrf_token));
        let reopened = Auth::open(dir.path(), auth.access_file.clone())
            .await
            .unwrap();
        let restored = reopened.authenticate(cookie).await.unwrap().unwrap();
        assert_eq!(restored.id, "alice");
        assert!(restored.valid_csrf(&login.actor.csrf_token));
        assert!(!restored.valid_csrf("wrong"));
        assert_eq!(restored.role, "operator");
        reopened.logout(cookie).await.unwrap();
        assert!(auth.authenticate(cookie).await.unwrap().is_none());
    }
    #[tokio::test]
    async fn removed_or_rotated_operator_cannot_use_existing_session() {
        let (_dir, auth) = fixture().await;
        let login = auth.login(TOKEN).await.unwrap();
        tokio::fs::write(
            &auth.access_file,
            json!({"operators":[{"id":"alice","name":"Alice","tokenHash":hash("different")}]})
                .to_string(),
        )
        .await
        .unwrap();
        assert!(
            auth.authenticate(&login.set_cookie)
                .await
                .unwrap()
                .is_none()
        );
        tokio::fs::write(&auth.access_file, "{\"operators\":[]}")
            .await
            .unwrap();
        assert!(
            auth.authenticate(&login.set_cookie)
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn expired_session_and_forged_cookie_are_rejected() {
        let (_dir, auth) = fixture().await;
        let login = auth.login(TOKEN).await.unwrap();
        sqlx::query("UPDATE operator_sessions SET expires_at = 0")
            .execute(&auth.db)
            .await
            .unwrap();
        assert!(
            auth.authenticate(&login.set_cookie)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            auth.authority_is_current(
                &login.actor.id,
                login.actor.authority_generation.as_deref().unwrap()
            )
            .await
            .unwrap(),
            "Browser expiry blocks admission but does not revoke admitted work"
        );
        assert!(
            auth.authenticate(&format!("{COOKIE}={}", random_secret()))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            auth.authenticate("actor=local-owner")
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn dispatch_generation_tracks_credentials_not_session_or_display_name() {
        let (dir, auth) = fixture().await;
        let login = auth.login(TOKEN).await.unwrap();
        let generation = login.actor.authority_generation.as_ref().unwrap();
        assert_ne!(generation, &hash(TOKEN));
        assert!(!format!("{:?}", login.actor).contains(generation));
        assert!(!login.actor.public_json().to_string().contains(generation));
        auth.logout(&login.set_cookie).await.unwrap();
        let reopened = Auth::open(dir.path(), auth.access_file.clone())
            .await
            .unwrap();
        assert!(
            reopened
                .authority_is_current("alice", generation)
                .await
                .unwrap()
        );
        tokio::fs::write(
            &auth.access_file,
            json!({"operators":[{"id":"alice","name":"Renamed","tokenHash":hash(TOKEN)}]})
                .to_string(),
        )
        .await
        .unwrap();
        assert!(
            reopened
                .authority_is_current("alice", generation)
                .await
                .unwrap()
        );
        tokio::fs::write(
            &auth.access_file,
            json!({"operators":[{"id":"alice","name":"Renamed","tokenHash":hash("replacement")}]})
                .to_string(),
        )
        .await
        .unwrap();
        assert!(
            !reopened
                .authority_is_current("alice", generation)
                .await
                .unwrap()
        );
        tokio::fs::write(&auth.access_file, "not-json")
            .await
            .unwrap();
        assert_eq!(
            reopened.authority_is_current("alice", generation).await,
            Err(AuthError::Unavailable)
        );
    }
    #[tokio::test]
    async fn login_attempts_are_bounded_and_wrong_keys_fail() {
        let (_dir, auth) = fixture().await;
        for _ in 0..LOGIN_LIMIT {
            assert!(matches!(
                auth.login("wrong").await,
                Err(AuthError::InvalidCredentials)
            ));
        }
        assert!(matches!(
            auth.login(TOKEN).await,
            Err(AuthError::RateLimited)
        ));
        assert_eq!(auth.attempts.lock().await.len(), LOGIN_LIMIT);
    }
    #[tokio::test]
    async fn duplicate_ids_and_local_owner_are_invalid_configurations() {
        let (_dir, auth) = fixture().await;
        for operators in [
            json!([{"id":"local-owner","name":"Owner","tokenHash":hash(TOKEN)}]),
            json!([{"id":"a","name":"A","tokenHash":hash(TOKEN)},{"id":"a","name":"B","tokenHash":hash("other")}]),
        ] {
            tokio::fs::write(
                &auth.access_file,
                json!({"operators":operators}).to_string(),
            )
            .await
            .unwrap();
            assert!(matches!(
                auth.login(TOKEN).await,
                Err(AuthError::Unavailable)
            ));
        }
    }
    #[test]
    fn ambiguous_cookie_and_identity_leak_are_rejected() {
        let value = random_secret();
        assert!(session_secret(&format!("{COOKIE}={value}; {COOKIE}={value}")).is_none());
        let actor = Actor::local_owner("secret-csrf");
        assert!(!actor.public_json().to_string().contains("secret-csrf"));
    }
}
