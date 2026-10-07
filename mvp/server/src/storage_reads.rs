//! Hot read projections. Each query sees one database statement snapshot; these
//! are preflight observations, never substitutes for transactional claim checks.
use super::*;
use crate::{ApiError,StatusCode};
use sqlx::Connection;

#[path = "storage_dispatch.rs"]
pub(super) mod dispatch;
#[path = "storage_preparation_status.rs"]
mod preparation_status;

const SQLITE_JOB: &str = r#"
SELECT (SELECT CASE WHEN ?2
  THEN json_remove(j.value,'$.prepareBundle.request','$.editorialPlan','$.editorialBatches')
  ELSE j.value END
  FROM json_each(w.payload,'$.jobs') j
  WHERE json_extract(CASE WHEN j.type='object' THEN j.value ELSE '{}' END,'$.id')=?1 LIMIT 1)
FROM workspace w WHERE w.id=1
"#;

const PG_JOB: &str = r#"
SELECT w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
  AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 j.id,j.kind,j.status,j.ref_id,
 (CASE WHEN $3 THEN
   (CASE WHEN jsonb_typeof(j.payload->'prepareBundle')='object'
     THEN j.payload #- '{prepareBundle,request}' ELSE j.payload END)
     -'editorialPlan'-'editorialBatches'
   ELSE j.payload END)::text AS payload
FROM communityhero.workspaces w
LEFT JOIN communityhero.jobs j ON j.workspace_id=w.id AND j.id=$2 WHERE w.id=$1
"#;

const PG_SCHEDULE: &str = r#"
SELECT w.execution_enabled,
       (jsonb_typeof(w.metadata)='object' AND
        jsonb_typeof(w.metadata->'account')='string' AND
        w.account=w.metadata->>'account') IS TRUE AS identity_valid,
       jsonb_build_object(
         'sync',jsonb_build_object(
           'fastStatus',jsonb_build_object('nextRunAt',w.metadata#>'{sync,fastStatus,nextRunAt}'),
           'background',jsonb_build_object('nextRunAt',w.metadata#>'{sync,background,nextRunAt}'),
           'pendingContext',COALESCE((
             SELECT jsonb_object_agg(p.key,jsonb_build_object(
               'status',p.value->'status','jobId',p.value->'jobId',
               'retryAt',p.value->'retryAt','queuedAt',p.value->'queuedAt','reason',p.value->'reason'))
             FROM jsonb_each(CASE WHEN jsonb_typeof(w.metadata#>'{sync,pendingContext}')='object'
               THEN w.metadata#>'{sync,pendingContext}' ELSE '{}'::jsonb END) p
           ),'{}'::jsonb)),
         'jobs',COALESCE((
           SELECT jsonb_agg(jsonb_build_object('id',j.id,'kind',j.kind,'status',j.status) ORDER BY j.ordinal)
           FROM communityhero.jobs j
           WHERE j.workspace_id=w.id AND j.status IN ('running','queued')
         ),'[]'::jsonb))::text AS schedule
FROM communityhero.workspaces w WHERE w.id=$1
"#;

const SQLITE_SCHEDULE: &str = r#"
SELECT json_object(
  'sync',json_object(
    'fastStatus',json_object('nextRunAt',json_extract(w.payload,'$.sync.fastStatus.nextRunAt')),
    'background',json_object('nextRunAt',json_extract(w.payload,'$.sync.background.nextRunAt')),
    'pendingContext',json(COALESCE((
      SELECT json_group_object(p.key,json_object(
        'status',json_extract(CASE WHEN p.type='object' THEN p.value ELSE '{}' END,'$.status'),
        'jobId',json_extract(CASE WHEN p.type='object' THEN p.value ELSE '{}' END,'$.jobId'),
        'retryAt',json_extract(CASE WHEN p.type='object' THEN p.value ELSE '{}' END,'$.retryAt'),
        'queuedAt',json_extract(CASE WHEN p.type='object' THEN p.value ELSE '{}' END,'$.queuedAt'),
        'reason',json_extract(CASE WHEN p.type='object' THEN p.value ELSE '{}' END,'$.reason')))
      FROM json_each(CASE WHEN json_type(w.payload,'$.sync.pendingContext')='object'
        THEN json_extract(w.payload,'$.sync.pendingContext') ELSE '{}' END) p
    ),'{}'))),
  'jobs',json(COALESCE((
    SELECT json_group_array(json_object('id',json_extract(j.value,'$.id'),
      'kind',json_extract(j.value,'$.kind'),'status',json_extract(j.value,'$.status')))
    FROM json_each(w.payload,'$.jobs') j
    WHERE json_extract(CASE WHEN j.type='object' THEN j.value ELSE '{}' END,'$.status') IN ('running','queued')
  ),'[]'))) AS schedule
FROM workspace w WHERE w.id=1
"#;

// Keep literal matching and Unicode case folding in Rust. Database collations
// differ between SQLite and PostgreSQL; only the source projection lives here.
// No draft, observed-message evidence, assistant chat, job or operation payload
// is returned by this query. Array order remains the persisted ordinal order.
const PG_SEARCH: &str = r#"
SELECT w.execution_enabled,
       (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
        AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
       jsonb_build_object(
         'items',COALESCE((SELECT jsonb_agg(jsonb_build_object(
           'id',i.payload->'id','itemId',i.payload->'itemId','branchId',i.payload->'branchId',
           'targetId',i.payload->'targetId','postId',i.payload->'postId','author',i.payload->'author',
           'text',i.payload->'text','preview',i.payload->'preview','title',i.payload->'title',
           'workflow',i.payload->'workflow','platform',i.payload->'platform','createdAt',i.payload->'createdAt',
           'revision',i.payload->'revision','triageTags',i.payload->'triageTags')
           ORDER BY i.ordinal) FROM communityhero.items i WHERE i.workspace_id=w.id),'[]'::jsonb),
         'posts',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',p.payload->'id','title',p.payload->'title')
           ORDER BY p.ordinal) FROM communityhero.posts p WHERE p.workspace_id=w.id),'[]'::jsonb),
         'branches',COALESCE((SELECT jsonb_agg(jsonb_build_object(
           'id',b.payload->'id','postId',b.payload->'postId',
           'messages',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',m.value->'id','author',m.value->'author') ORDER BY m.ordinality)
             FROM jsonb_array_elements(CASE WHEN jsonb_typeof(b.payload->'messages')='array'
               THEN b.payload->'messages' ELSE '[]'::jsonb END) WITH ORDINALITY m),'[]'::jsonb))
           ORDER BY b.ordinal) FROM communityhero.branches b WHERE b.workspace_id=w.id),'[]'::jsonb))::text AS projection
FROM communityhero.workspaces w WHERE w.id=$1
"#;

const SQLITE_SEARCH: &str = r#"
SELECT json_object(
  'items',json(COALESCE((SELECT json_group_array(json_object(
    'id',json_extract(i.value,'$.id'),'itemId',json_extract(i.value,'$.itemId'),
    'branchId',json_extract(i.value,'$.branchId'),'targetId',json_extract(i.value,'$.targetId'),
    'postId',json_extract(i.value,'$.postId'),'author',json_extract(i.value,'$.author'),
    'text',json_extract(i.value,'$.text'),'preview',json_extract(i.value,'$.preview'),
    'title',json_extract(i.value,'$.title'),'workflow',json_extract(i.value,'$.workflow'),
    'platform',json_extract(i.value,'$.platform'),'createdAt',json_extract(i.value,'$.createdAt'),
    'revision',json_extract(i.value,'$.revision'),'triageTags',json_extract(i.value,'$.triageTags')))
    FROM json_each(w.payload,'$.items') i),'[]')),
  'posts',json(COALESCE((SELECT json_group_array(json_object('id',json_extract(p.value,'$.id'),'title',json_extract(p.value,'$.title')))
    FROM json_each(w.payload,'$.posts') p),'[]')),
  'branches',json(COALESCE((SELECT json_group_array(json_object(
    'id',json_extract(b.value,'$.id'),'postId',json_extract(b.value,'$.postId'),
    'messages',json(COALESCE((SELECT json_group_array(json_object('id',json_extract(m.value,'$.id'),'author',json_extract(m.value,'$.author')))
      FROM json_each(CASE WHEN json_type(b.value,'$.messages')='array' THEN json_extract(b.value,'$.messages') ELSE '[]' END) m),'[]'))))
    FROM json_each(w.payload,'$.branches') b),'[]'))) AS projection
FROM workspace w WHERE w.id=1
"#;

const PG_KNOWLEDGE_CATALOG: &str = r#"
SELECT w.execution_enabled,
       (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
        AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
       json_build_object(
         'entries',COALESCE((SELECT json_agg(e.payload ORDER BY e.ordinal)
           FROM communityhero.knowledge_entries e WHERE e.workspace_id=w.id),'[]'::json),
         'versions',COALESCE((SELECT json_agg(v.payload ORDER BY v.ordinal)
           FROM communityhero.knowledge_versions v WHERE v.workspace_id=w.id),'[]'::json))::text AS projection,
       CASE WHEN $2 THEN COALESCE((SELECT json_agg(f.payload ORDER BY f.ordinal)
             FROM communityhero.feedback f WHERE f.workspace_id=w.id),'[]'::json)::text
           ELSE NULL::text END AS feedback
FROM communityhero.workspaces w WHERE w.id=$1
"#;

const SQLITE_KNOWLEDGE_CATALOG: &str = r#"
SELECT CASE WHEN ? THEN json_object(
  'entries',json(COALESCE(json_extract(w.payload,'$.knowledge_entries'),'[]')),
  'versions',json(COALESCE(json_extract(w.payload,'$.knowledge_versions'),'[]')),
  'feedback',json(COALESCE(json_extract(w.payload,'$.feedback'),'[]')))
ELSE json_object(
  'entries',json(COALESCE(json_extract(w.payload,'$.knowledge_entries'),'[]')),
  'versions',json(COALESCE(json_extract(w.payload,'$.knowledge_versions'),'[]'))) END AS projection
FROM workspace w WHERE w.id=1
"#;

// The existing bootstrap sanitizer already removes these fields. Do that work
// before transport/JSON decoding, retaining all compact jobs until actor
// filtering applies the history limit. Never pre-limit another actor's jobs.
// Aggregate JSON text, not JSONB containers: valid independent rows can total
// more than JSONB's 28-bit container offsets. Metadata stays a separate column
// in this same statement and is merged with the entity projection in Rust.
const PG_BOOTSTRAP: &str = r#"
SELECT w.execution_enabled,
       (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
        AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
       (w.metadata-'companyKnowledgeCoverage')::text AS metadata,
       json_build_object(
         'posts',COALESCE((SELECT json_agg(p.payload ORDER BY p.ordinal) FROM communityhero.posts p WHERE p.workspace_id=w.id),'[]'::json),
         'branches',COALESCE((SELECT json_agg(b.payload-'observedMessages' ORDER BY b.ordinal) FROM communityhero.branches b WHERE b.workspace_id=w.id),'[]'::json),
         'items',COALESCE((SELECT json_agg(i.payload ORDER BY i.ordinal) FROM communityhero.items i WHERE i.workspace_id=w.id),'[]'::json),
         'conversations',COALESCE((SELECT json_agg(c.payload ORDER BY c.ordinal) FROM communityhero.conversations c WHERE c.workspace_id=w.id),'[]'::json),
         'proposals',COALESCE((SELECT json_agg(p.payload ORDER BY p.ordinal) FROM communityhero.proposals p WHERE p.workspace_id=w.id),'[]'::json),
         'approvals',COALESCE((SELECT json_agg(a.payload-'approvalAuthority' ORDER BY a.ordinal) FROM communityhero.approvals a WHERE a.workspace_id=w.id),'[]'::json),
         'operations',COALESCE((SELECT json_agg(o.payload-'dispatchAuthority' ORDER BY o.ordinal) FROM communityhero.operations o WHERE o.workspace_id=w.id),'[]'::json),
         'materials',COALESCE((SELECT json_agg(m.payload ORDER BY m.ordinal) FROM communityhero.materials m WHERE m.workspace_id=w.id),'[]'::json),
         'mediaReadinessCatalog',json_build_object(
           'knowledge_entries',COALESCE((SELECT json_agg(e.payload ORDER BY e.ordinal)
             FROM communityhero.knowledge_entries e JOIN communityhero.knowledge_versions v
               ON v.workspace_id=e.workspace_id AND v.entry_id=e.id AND v.id=e.current_version_id
             WHERE e.workspace_id=w.id AND v.payload->>'kind' IN ('transcript','visual_context')),'[]'::json),
           'knowledge_versions',COALESCE((SELECT json_agg(v.payload ORDER BY e.ordinal)
             FROM communityhero.knowledge_entries e JOIN communityhero.knowledge_versions v
               ON v.workspace_id=e.workspace_id AND v.entry_id=e.id AND v.id=e.current_version_id
             WHERE e.workspace_id=w.id AND v.payload->>'kind' IN ('transcript','visual_context')),'[]'::json)),
         'jobs',COALESCE((SELECT json_agg(compact.payload ORDER BY j.ordinal)
           FROM communityhero.jobs j
           CROSS JOIN LATERAL (SELECT CASE WHEN jsonb_typeof(j.payload->'prepareBundle')='object'
             THEN j.payload #- '{prepareBundle,request}' ELSE j.payload END AS payload) bundle
           CROSS JOIN LATERAL (SELECT CASE WHEN jsonb_typeof(bundle.payload#>'{result,visualProgress}')='object'
             THEN bundle.payload #- '{result,visualProgress,sourceProjection}'
               #- '{result,visualProgress,leaseId}' #- '{result,visualProgress,materialEpoch}'
             ELSE bundle.payload END AS payload) visual
           CROSS JOIN LATERAL (SELECT visual.payload-'editorialPlan'-'editorialBatches' AS payload) compact
           WHERE j.workspace_id=w.id),'[]'::json))::text AS projection
FROM communityhero.workspaces w WHERE w.id=$1
"#;

const SQLITE_BOOTSTRAP: &str = r#"
SELECT json_set(json_remove(w.payload,'$.audit','$.knowledge_entries','$.knowledge_versions','$.feedback','$.companyKnowledgeCoverage'),
  '$.mediaReadinessCatalog',json_object(
    'knowledge_entries',json(COALESCE((SELECT json_group_array(json(e.value))
      FROM json_each(w.payload,'$.knowledge_entries') e JOIN json_each(w.payload,'$.knowledge_versions') v
        ON json_extract(v.value,'$.entryId')=json_extract(e.value,'$.id') AND json_extract(v.value,'$.id')=json_extract(e.value,'$.currentVersionId')
      WHERE json_extract(v.value,'$.kind') IN ('transcript','visual_context')),'[]')),
    'knowledge_versions',json(COALESCE((SELECT json_group_array(json(v.value))
      FROM json_each(w.payload,'$.knowledge_entries') e JOIN json_each(w.payload,'$.knowledge_versions') v
        ON json_extract(v.value,'$.entryId')=json_extract(e.value,'$.id') AND json_extract(v.value,'$.id')=json_extract(e.value,'$.currentVersionId')
      WHERE json_extract(v.value,'$.kind') IN ('transcript','visual_context')),'[]'))),
  '$.branches',json(COALESCE((SELECT json_group_array(json_remove(b.value,'$.observedMessages')) FROM json_each(w.payload,'$.branches') b),'[]')),
  '$.approvals',json(COALESCE((SELECT json_group_array(json_remove(a.value,'$.approvalAuthority')) FROM json_each(w.payload,'$.approvals') a),'[]')),
  '$.operations',json(COALESCE((SELECT json_group_array(json_remove(o.value,'$.dispatchAuthority')) FROM json_each(w.payload,'$.operations') o),'[]')),
  '$.jobs',json(COALESCE((SELECT json_group_array(json_remove(j.value,'$.prepareBundle.request',
    '$.editorialPlan','$.editorialBatches','$.result.visualProgress.sourceProjection',
    '$.result.visualProgress.leaseId','$.result.visualProgress.materialEpoch'))
    FROM json_each(w.payload,'$.jobs') j),'[]'))) AS projection
FROM workspace w WHERE w.id=1
"#;

fn postgres_guard(record: &sqlx::postgres::PgRow) -> ApiResult<()> {
    if record.try_get::<bool, _>("execution_enabled")? {
        return Err(internal("PostgreSQL pilot execution must remain disabled"));
    }
    if !record.try_get::<bool, _>("identity_valid")? {
        return Err(internal("Workspace identity mismatch"));
    }
    Ok(())
}

// Keep metadata and entity projections in separate text columns from the same
// SQL statement snapshot. Reconstruct the former right-biased JSONB merge in
// Rust: neither a large collection nor the whole response becomes one JSONB
// container (PostgreSQL's container offset limit is about 256 MiB).
fn merge_bootstrap_projection(metadata_payload:&str,entity_payload:&str)->ApiResult<Value>{
    let mut metadata=parse(metadata_payload)?;
    let Value::Object(entities)=parse(entity_payload)? else{return Err(internal("Invalid bootstrap entity projection"));};
    let target=metadata.as_object_mut().ok_or_else(||internal("Invalid bootstrap metadata projection"))?;
    target.extend(entities);
    Ok(metadata)
}

impl Database {
    /// Account admission and counters do not require loading comment/media history.
    pub(crate) async fn read_engine_status(&self) -> ApiResult<Value> {
        match self {
            Self::Sqlite(_) => self.read().await,
            Self::Postgres { reader, .. } => {
                let record = sqlx::query(r#"
SELECT w.execution_enabled,
 (jsonb_typeof(w.metadata->'account')='string' AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 jsonb_build_object('account',w.metadata->'account','connectorBinding',w.metadata->'connectorBinding',
 'storageGeneration',w.metadata->'storageGeneration',
 'counts',jsonb_build_object(
 'items',(SELECT count(*) FROM communityhero.items WHERE workspace_id=w.id),
 'prepared',(SELECT count(*) FROM communityhero.items WHERE workspace_id=w.id AND payload->>'workflow'='prepared'),
 'attention',(SELECT count(*) FROM communityhero.items WHERE workspace_id=w.id AND payload->>'workflow'='attention'),
 'unknown',(SELECT count(*) FROM communityhero.operations WHERE workspace_id=w.id AND status='unknown'),
 'dispatching',(SELECT count(*) FROM communityhero.operations WHERE workspace_id=w.id AND status='dispatching')))::text AS projection
FROM communityhero.workspaces w WHERE w.id=$1
"#).bind(WORKSPACE).fetch_one(reader).await?;
                postgres_guard(&record)?;
                parse(&record.try_get::<String, _>("projection")?)
            }
        }
    }
    /// Coherent actor-neutral bootstrap input, with only already-hidden fields
    /// removed. The HTTP/cache layer still owns actor filtering and delta bases.
    pub(crate) async fn read_bootstrap_source(&self) -> ApiResult<Value> {
        let payload=match self {
            Self::Sqlite(pool)=>sqlx::query_scalar::<_,String>(SQLITE_BOOTSTRAP).fetch_one(pool).await?,
            Self::Postgres { reader: pool, .. }=>{
                let record=sqlx::query(PG_BOOTSTRAP).bind(WORKSPACE).fetch_one(pool).await?;
                postgres_guard(&record)?;
                return merge_bootstrap_projection(record.try_get::<&str,_>("metadata")?,record.try_get::<&str,_>("projection")?);
            }
        };
        parse(&payload)
    }

    /// Search reads known public-comment fields, never assistant/model history.
    /// One statement supplies a coherent view of all fallback lookup inputs.
    pub(crate) async fn read_search_context(&self) -> ApiResult<Value> {
        let payload = match self {
            Self::Sqlite(pool) => sqlx::query_scalar::<_, String>(SQLITE_SEARCH)
                .fetch_one(pool).await?,
            Self::Postgres { reader: pool, .. } => {
                let record = sqlx::query(PG_SEARCH).bind(WORKSPACE).fetch_one(pool).await?;
                postgres_guard(&record)?;
                record.try_get::<String, _>("projection")?
            }
        };
        parse(&payload)
    }

    /// Preserve catalog history/order exactly; omit feedback at the database
    /// projection for operators rather than loading private feedback to discard it.
    pub(crate) async fn read_knowledge_catalog(&self, include_feedback: bool) -> ApiResult<Value> {
        let payload = match self {
            Self::Sqlite(pool) => sqlx::query_scalar::<_, String>(SQLITE_KNOWLEDGE_CATALOG)
                .bind(include_feedback).fetch_one(pool).await?,
            Self::Postgres { reader: pool, .. } => {
                let record = sqlx::query(PG_KNOWLEDGE_CATALOG).bind(WORKSPACE)
                    .bind(include_feedback).fetch_one(pool).await?;
                postgres_guard(&record)?;
                let mut catalog=parse(record.try_get::<&str,_>("projection")?)?;
                if include_feedback{
                    let feedback=record.try_get::<Option<&str>,_>("feedback")?
                        .ok_or_else(||internal("Owner knowledge feedback projection missing"))?;
                    catalog.as_object_mut().ok_or_else(||internal("Invalid knowledge catalog projection"))?
                        .insert("feedback".into(),parse(feedback)?);
                }
                return Ok(catalog);
            }
        };
        parse(&payload)
    }

    /// Fetch one durable job, including its evidence. A missing job is `None`;
    /// a missing/invalid workspace remains an error. Never consult an app cache.
    pub(crate) async fn read_job(&self, key: &str) -> ApiResult<Option<Value>> {
        self.read_job_projected(key, false).await
    }

    /// The public point endpoint already hides these three cold evidence
    /// fields. Remove them before SQL transport/JSON decoding on every poll,
    /// preserving every other field and the complete internal reader above.
    pub(crate) async fn read_job_public(&self, key: &str) -> ApiResult<Option<Value>> {
        self.read_job_projected(key, true).await
    }

    async fn read_job_projected(&self, key: &str, public: bool) -> ApiResult<Option<Value>> {
        match self {
            Self::Sqlite(pool) => {
                // SQLite retains its one-document format. JSON1 avoids decoding
                // unrelated evidence into Rust even though SQLite scans the JSON.
                let payload: Option<String> = sqlx::query_scalar(SQLITE_JOB)
                    .bind(key).bind(public).fetch_one(pool).await?;
                payload.map(|s| parse(&s)).transpose()
            }
            Self::Postgres { reader: pool, .. } => {
                let record = sqlx::query(PG_JOB)
                    .bind(WORKSPACE).bind(key).bind(public).fetch_one(pool).await?;
                #[cfg(test)] crate::performance::r3_sql_read();
                postgres_guard(&record)?;
                let Some(payload) = record.try_get::<Option<String>, _>("payload")? else {
                    return Ok(None);
                };
                let value = parse(&payload)?;
                if record.try_get::<&str, _>("id")? != text(&value, "id")? {
                    return Err(internal("Record identity mismatch"));
                }
                for (column, field) in projection("jobs") {
                    if (!value[*field].is_null() && !value[*field].is_string())
                        || record.try_get::<Option<String>, _>(*column)?.as_deref()
                            != value[*field].as_str()
                    {
                        return Err(internal("Record relational projection mismatch"));
                    }
                }
                Ok(Some(value))
            }
        }
    }

    /// Minimal shape understood by the scheduler predicates: deadlines, context
    /// retry/ownership fields, and active job identities only. Completed jobs
    /// cannot block a lane or make a running context entry non-abandoned.
    /// PostgreSQL uses the relational job projection validated at startup and
    /// maintained in the same write transaction as each payload.
    pub(crate) async fn read_schedule(&self) -> ApiResult<Value> {
        let payload = match self {
            Self::Sqlite(pool) => {
                sqlx::query_scalar::<_, String>(SQLITE_SCHEDULE)
                    .fetch_one(pool)
                    .await?
            }
            Self::Postgres { reader: pool, .. } => {
                let record = sqlx::query(PG_SCHEDULE)
                    .bind(WORKSPACE)
                    .fetch_one(pool)
                    .await?;
                postgres_guard(&record)?;
                record.try_get::<String, _>("schedule")?
            }
        };
        parse(&payload)
    }

    /// Include connection acquisition in the deadline. This checks storage and
    /// the workspace guard, not provider health or complete workspace integrity.
    pub(crate) async fn readiness(&self, deadline: Duration) -> ApiResult<()> {
        tokio::time::timeout(deadline, async {
            match self {
                Self::Sqlite(pool) => {
                    // No JSON scan: startup and writes own document validation.
                    let _: i32 = sqlx::query_scalar("SELECT 1 FROM workspace WHERE id=1")
                        .fetch_one(pool).await?;
                }
                Self::Postgres { writer, reader } => {
                    // Probe database availability independently of both busy
                    // pools. This is not proof of owner-lease health. The outer
                    // deadline bounds connection setup and the read query.
                    if writer.is_closed() || reader.is_closed() { return Err(internal("Storage pool closed")); }
                    let mut connection=PgConnection::connect_with(&reader.connect_options()).await?;
                    sqlx::query("SET default_transaction_read_only = on")
                        .execute(&mut connection).await?;
                    let record = sqlx::query(
                        "SELECT execution_enabled,(jsonb_typeof(metadata)='object' AND jsonb_typeof(metadata->'account')='string' AND account=metadata->>'account') IS TRUE AS identity_valid FROM communityhero.workspaces WHERE id=$1",
                    ).bind(WORKSPACE).fetch_one(&mut connection).await?;
                    postgres_guard(&record)?;
                    if writer.is_closed() || reader.is_closed() { return Err(internal("Storage pool closed")); }
                }
            }
            Ok(())
        }).await.map_err(|_| internal("Storage readiness deadline exceeded"))?
    }
}

// Bounded diagnostics are separate from the explicit history catalog above.
// Validate exact selected immutable bodies before projecting a public head.
const KNOWLEDGE_HEAD_BYTES: usize = 256 * 1024;
const KNOWLEDGE_RAW_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub(crate) struct KnowledgeHeadsQuery {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub entry_ids: Option<String>,
    pub source_material_ids: Option<String>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct KnowledgeCursor {
    version:u8, account:String, binding_sha256:String, selection_sha256:String,
    limit:usize, ordinal:i64, id:String,
}
fn diagnostic_budget(scope:&str)->ApiError {
    ApiError(StatusCode::PAYLOAD_TOO_LARGE,format!("diagnostic_budget_exceeded:{scope}; complete raw evidence requires the existing /api/knowledge history contract or offline workspace export"))
}
fn bounded_id(value:&str)->ApiResult<()> {
    if value.is_empty()||value.len()>512||value.chars().any(char::is_control) {return Err(crate::bad("Invalid bounded knowledge ID"));}
    Ok(())
}
fn knowledge_selector(query:&KnowledgeHeadsQuery)->ApiResult<(usize,Vec<String>,Vec<String>,Value)> {
    let limit=query.limit.unwrap_or(20);
    if limit==0||limit>100||query.entry_ids.is_some()&&query.source_material_ids.is_some(){return Err(crate::bad("Select one knowledge selector and limit between 1 and 100"));}
    let parse_ids=|raw:&Option<String>|->ApiResult<Vec<String>> {
        let Some(raw)=raw else{return Ok(vec![]);};
        let ids=raw.split(',').map(str::to_owned).collect::<Vec<_>>();
        let mut seen=HashSet::new();
        if ids.len()>20 {return Err(crate::bad("Select at most 20 exact knowledge IDs"));}
        for id in &ids{bounded_id(id)?;if !seen.insert(id){return Err(crate::bad("Duplicate knowledge selector"));}}
        Ok(ids)
    };
    let entries=parse_ids(&query.entry_ids)?;let sources=parse_ids(&query.source_material_ids)?;
    let selection=serde_json::json!({"kind":if !entries.is_empty(){"entry_ids"}else if !sources.is_empty(){"source_material_ids"}else{"current_heads"},"entryIds":entries,"sourceMaterialIds":sources});
    Ok((limit,entries,sources,selection))
}
fn cursor_encode(cursor:&KnowledgeCursor)->ApiResult<String> {
    let bytes=serde_json::to_vec(cursor).map_err(|_|internal("Knowledge cursor serialization failed"))?;
    Ok(bytes.iter().map(|b|format!("{b:02x}")).collect())
}
fn cursor_decode(raw:&str)->ApiResult<KnowledgeCursor> {
    if raw.len()>8192||raw.len()%2!=0||!raw.bytes().all(|b|b.is_ascii_hexdigit()){return Err(crate::bad("Invalid knowledge cursor"));}
    let bytes=(0..raw.len()).step_by(2).map(|n|u8::from_str_radix(&raw[n..n+2],16)).collect::<Result<Vec<_>,_>>().map_err(|_|crate::bad("Invalid knowledge cursor"))?;
    let cursor:KnowledgeCursor=serde_json::from_slice(&bytes).map_err(|_|crate::bad("Invalid knowledge cursor"))?;
    bounded_id(&cursor.id)?;
    if cursor.version!=1||cursor.ordinal<0||cursor.limit==0||cursor.limit>100{return Err(crate::bad("Invalid knowledge cursor"));}
    Ok(cursor)
}
fn knowledge_identity(account:&str,binding:&str)->ApiResult<Value> {
    let mut value=serde_json::json!({"account":account});
    value["connectorBinding"]=parse(binding)?;
    crate::accounts::Profile::from_workspace(&value)?;crate::active_binding(&value)?;
    Ok(value)
}
fn knowledge_head(entry:&Value,version:&Value)->Value {
    serde_json::json!({"id":entry["id"],"versionId":version["id"],"hash":version["hash"],
        "revision":version["sourceRevision"],"kind":version["kind"],"status":version["status"],
        "trust":version["trust"],"title":version["title"],"sourceUrl":version["sourceUrl"],
        "sourceMaterialId":entry["sourceMaterialId"],"textLength":version["text"].as_str().map(str::len)})
}
fn validate_knowledge_pair(account:&str,entry:&Value,version:&Value,current:bool)->ApiResult<()> {
    if ["id","entryId","hash","kind","status","trust","title","text","sourceUrl"].iter().any(|field|!version[*field].is_string()){return Err(internal("Malformed selected knowledge version"));}
    let mut selected=serde_json::json!({"account":account,"knowledge_entries":[],"knowledge_versions":[version]});
    if current{selected["knowledge_entries"]=serde_json::json!([entry]);}
    crate::knowledge::validate_catalog(&selected).map_err(internal)?;
    if version["scope"]["account"]!=account||entry["scope"]["account"]!=account
        ||version["entryId"]!=entry["id"]||version["sourceMaterialId"]!=entry["sourceMaterialId"]{
        return Err(internal("Selected knowledge identity mismatch"));
    }
    Ok(())
}
fn knowledge_response_budget(value:Value,budget:usize)->ApiResult<Value> {
    if serde_json::to_vec(&value).map_err(|_|internal("Knowledge serialization failed"))?.len()>budget{return Err(diagnostic_budget("knowledge_response"));}
    Ok(value)
}

// The +1 sentinel belongs to the same statement snapshot as its current join.
// Oversize raw values are measured but suppressed in SQL before wire/decode.
const PG_KNOWLEDGE_HEADS:&str=r#"
WITH selected AS (
 SELECT e.* FROM communityhero.knowledge_entries e WHERE e.workspace_id=$1
 AND (cardinality($2::text[])=0 OR e.id=ANY($2::text[]) OR e.payload->>'id'=ANY($2::text[]))
 AND (cardinality($3::text[])=0 OR e.source_material_id=ANY($3::text[]) OR e.payload->>'sourceMaterialId'=ANY($3::text[]))
 AND (e.ordinal>$4 OR e.ordinal=$4 AND e.id>$5) ORDER BY e.ordinal,e.id LIMIT $6
)
SELECT w.account,(w.metadata->'connectorBinding')::text AS binding,w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 e.id,e.ordinal,e.current_version_id,e.source_material_id,v.id AS version_id,v.entry_id,
 v.source_material_id AS version_source_material_id,
 octet_length(e.payload::text)::bigint+COALESCE(octet_length(v.payload::text),0) AS bytes,
 CASE WHEN sum(octet_length(e.payload::text)::bigint+COALESCE(octet_length(v.payload::text),0)) OVER()<=$7 THEN e.payload::text END AS entry,
 CASE WHEN sum(octet_length(e.payload::text)::bigint+COALESCE(octet_length(v.payload::text),0)) OVER()<=$7 THEN v.payload::text END AS version,
 (e.source_material_id IS NULL OR (SELECT count(*)=1 AND bool_and(m.id=m.payload->>'id') FROM communityhero.materials m WHERE m.workspace_id=w.id AND (m.id=e.source_material_id OR m.payload->>'id'=e.source_material_id))) AS material_valid,
 (SELECT count(*) FROM communityhero.knowledge_entries peer WHERE peer.workspace_id=w.id
 AND (peer.id=e.id OR peer.payload->>'id'=e.id OR peer.source_material_id=e.source_material_id OR peer.payload->>'sourceMaterialId'=e.source_material_id)) AS peers
FROM communityhero.workspaces w LEFT JOIN selected e ON true
LEFT JOIN communityhero.knowledge_versions v ON v.workspace_id=e.workspace_id AND v.id=e.current_version_id
WHERE w.id=$1 ORDER BY e.ordinal,e.id LIMIT ($6+1)
"#;
const SQLITE_KNOWLEDGE_HEADS:&str=r#"
WITH selected AS (
 SELECT e.value,CAST(e.key AS INTEGER) AS ordinal FROM workspace w,json_each(w.payload,'$.knowledge_entries') e WHERE w.id=1
 AND (json_array_length(?1)=0 OR json_extract(e.value,'$.id') IN (SELECT value FROM json_each(?1)))
 AND (json_array_length(?2)=0 OR json_extract(e.value,'$.sourceMaterialId') IN (SELECT value FROM json_each(?2)))
 AND (CAST(e.key AS INTEGER)>?3 OR CAST(e.key AS INTEGER)=?3 AND json_extract(e.value,'$.id')>?4)
 ORDER BY CAST(e.key AS INTEGER),json_extract(e.value,'$.id') LIMIT ?5
)
SELECT json_extract(w.payload,'$.account') AS account,json_extract(w.payload,'$.connectorBinding') AS binding,
 e.ordinal,json_extract(e.value,'$.id') AS id,
 length(CAST(e.value AS BLOB))+COALESCE(length(CAST(v.value AS BLOB)),0) AS bytes,
 CASE WHEN sum(length(CAST(e.value AS BLOB))+COALESCE(length(CAST(v.value AS BLOB)),0)) OVER()<=?6 THEN e.value END AS entry,
 CASE WHEN sum(length(CAST(e.value AS BLOB))+COALESCE(length(CAST(v.value AS BLOB)),0)) OVER()<=?6 THEN v.value END AS version,
 (json_extract(e.value,'$.sourceMaterialId') IS NULL OR (SELECT count(*)=1 FROM json_each(w.payload,'$.materials') m WHERE json_extract(m.value,'$.id')=json_extract(e.value,'$.sourceMaterialId'))) AS material_valid,
 (SELECT count(*) FROM json_each(w.payload,'$.knowledge_entries') peer WHERE
 json_extract(peer.value,'$.id')=json_extract(e.value,'$.id') OR json_extract(peer.value,'$.sourceMaterialId')=json_extract(e.value,'$.sourceMaterialId')) AS peers
FROM workspace w LEFT JOIN selected e ON true
LEFT JOIN json_each(w.payload,'$.knowledge_versions') v ON json_extract(v.value,'$.id')=json_extract(e.value,'$.currentVersionId')
WHERE w.id=1 ORDER BY e.ordinal,json_extract(e.value,'$.id') LIMIT (?5+1)
"#;
struct KnowledgeSelected { identity:Value, rows:Vec<(i64,Value,Value)> }
impl Database {
    pub(crate) async fn read_knowledge_heads(&self,query:&KnowledgeHeadsQuery)->ApiResult<Value> {
        let(limit,entries,sources,selection)=knowledge_selector(query)?;
        let cursor=query.cursor.as_deref().map(cursor_decode).transpose()?;
        let ordinal=cursor.as_ref().map_or(-1,|c|c.ordinal);let last_id=cursor.as_ref().map_or("",|c|c.id.as_str());
        let mut selected=KnowledgeSelected{identity:Value::Null,rows:vec![]};let mut raw_bytes=0usize;
        match self {
            Self::Postgres{reader,..}=>{
                let records=sqlx::query(PG_KNOWLEDGE_HEADS).bind(WORKSPACE).bind(&entries).bind(&sources)
                    .bind(ordinal).bind(last_id).bind((limit+1) as i64).bind(KNOWLEDGE_RAW_BYTES as i64).fetch_all(reader).await?;
                for record in records {
                    postgres_guard(&record)?;
                    selected.identity=knowledge_identity(record.try_get("account")?,record.try_get("binding")?)?;
                    let Some(id)=record.try_get::<Option<&str>,_>("id")? else{continue;};
                    let bytes=record.try_get::<i64,_>("bytes")?;if bytes<0{return Err(internal("Invalid knowledge byte count"));}
                    raw_bytes=raw_bytes.checked_add(bytes as usize).ok_or_else(||diagnostic_budget("knowledge_integrity"))?;
                    if raw_bytes>KNOWLEDGE_RAW_BYTES{return Err(diagnostic_budget("knowledge_integrity"));}
                    let entry=parse(record.try_get::<Option<&str>,_>("entry")?.ok_or_else(||diagnostic_budget("knowledge_integrity"))?)?;
                    let version=parse(record.try_get::<Option<&str>,_>("version")?.ok_or_else(||internal("Missing selected knowledge head version"))?)?;
                    if !record.try_get::<bool,_>("material_valid")?||record.try_get::<i64,_>("peers")?!=1||entry["id"]!=id||entry["currentVersionId"].as_str()!=record.try_get::<Option<&str>,_>("current_version_id")?
                        ||entry["sourceMaterialId"].as_str()!=record.try_get::<Option<&str>,_>("source_material_id")?
                        ||version["id"].as_str()!=record.try_get::<Option<&str>,_>("version_id")?
                        ||version["entryId"].as_str()!=record.try_get::<Option<&str>,_>("entry_id")?
                        ||version["sourceMaterialId"].as_str()!=record.try_get::<Option<&str>,_>("version_source_material_id")? {
                        return Err(internal("Selected knowledge relational identity mismatch"));
                    }
                    selected.rows.push((record.try_get::<i32,_>("ordinal")? as i64,entry,version));
                }
            }
            Self::Sqlite(pool)=>{
                let records=sqlx::query(SQLITE_KNOWLEDGE_HEADS).bind(serde_json::json!(entries).to_string()).bind(serde_json::json!(sources).to_string())
                    .bind(ordinal).bind(last_id).bind((limit+1) as i64).bind(KNOWLEDGE_RAW_BYTES as i64).fetch_all(pool).await?;
                for record in records {
                    selected.identity=knowledge_identity(record.try_get("account")?,record.try_get("binding")?)?;
                    let Some(id)=record.try_get::<Option<&str>,_>("id")? else{
                        if record.try_get::<Option<i64>,_>("ordinal")?.is_some(){return Err(internal("Missing selected knowledge ID"));}continue;
                    };
                    let bytes=record.try_get::<i64,_>("bytes")?;if bytes<0{return Err(internal("Invalid knowledge byte count"));}
                    raw_bytes=raw_bytes.checked_add(bytes as usize).ok_or_else(||diagnostic_budget("knowledge_integrity"))?;
                    if raw_bytes>KNOWLEDGE_RAW_BYTES{return Err(diagnostic_budget("knowledge_integrity"));}
                    let entry=parse(record.try_get::<Option<&str>,_>("entry")?.ok_or_else(||diagnostic_budget("knowledge_integrity"))?)?;
                    let version=parse(record.try_get::<Option<&str>,_>("version")?.ok_or_else(||internal("Missing selected knowledge head version"))?)?;
                    if !record.try_get::<bool,_>("material_valid")?||record.try_get::<i64,_>("peers")?!=1||entry["id"]!=id{return Err(internal("Duplicate selected knowledge head"));}
                    selected.rows.push((record.try_get("ordinal")?,entry,version));
                }
            }
        }
        if selected.identity.is_null(){return Err(internal("Knowledge workspace missing"));}
        if selected.rows.len()>limit+1{return Err(diagnostic_budget("knowledge_cardinality"));}
        let mut measurement=crate::performance::Span::new("knowledge.heads.integrity");measurement.counts(selected.rows.len(),raw_bytes,1);drop(measurement);
        let account=text(&selected.identity,"account")?;
        let binding_sha256=crate::preparation_materials::hash(&selected.identity["connectorBinding"]);
        let selection_sha256=crate::preparation_materials::hash(&selection);
        if cursor.as_ref().is_some_and(|c|c.account!=account||c.binding_sha256!=binding_sha256||c.selection_sha256!=selection_sha256||c.limit!=limit){return Err(crate::bad("Knowledge cursor company, binding or selection mismatch"));}
        let mut seen=HashSet::new();let mut previous=None;
        for(position,entry,version)in &selected.rows {
            if *position<0||previous.is_some_and(|old|old>=*position)||!seen.insert(text(entry,"id")?){return Err(internal("Selected knowledge identity/order mismatch"));}
            previous=Some(*position);validate_knowledge_pair(account,entry,version,true)?;
        }
        let more=selected.rows.len()>limit;selected.rows.truncate(limit);
        let next=if more {let(position,entry,_)=selected.rows.last().ok_or_else(||internal("Knowledge cursor has no head"))?;
            Some(cursor_encode(&KnowledgeCursor{version:1,account:account.into(),binding_sha256,selection_sha256,limit,ordinal:*position,id:text(entry,"id")?.into()})?)}else{None};
        let heads=selected.rows.iter().map(|(_,e,v)|knowledge_head(e,v)).collect::<Vec<_>>();
        knowledge_response_budget(serde_json::json!({"schemaVersion":1,"account":account,"connectorBinding":selected.identity["connectorBinding"],
            "observedAt":crate::now(),"selection":selection,"coverage":{"returned":heads.len(),"pageComplete":true,"selectionComplete":!more,"historyIncluded":false},
            "heads":heads,"nextCursor":next}),KNOWLEDGE_HEAD_BYTES)
    }
}
const PG_KNOWLEDGE_EXACT:&str=r#"
SELECT w.account,(w.metadata->'connectorBinding')::text AS binding,w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 e.id AS entry_id,e.current_version_id,e.source_material_id,v.id AS version_id,v.entry_id AS version_entry_id,
 v.source_material_id AS version_source_material_id,
 octet_length(e.payload::text)::bigint+COALESCE(octet_length(v.payload::text),0) AS bytes,
 CASE WHEN sum(octet_length(e.payload::text)::bigint+COALESCE(octet_length(v.payload::text),0)) OVER()<=$4 THEN e.payload::text END AS entry,
 CASE WHEN sum(octet_length(e.payload::text)::bigint+COALESCE(octet_length(v.payload::text),0)) OVER()<=$4 THEN v.payload::text END AS version,
 (e.source_material_id IS NULL OR (SELECT count(*)=1 AND bool_and(m.id=m.payload->>'id') FROM communityhero.materials m WHERE m.workspace_id=w.id AND (m.id=e.source_material_id OR m.payload->>'id'=e.source_material_id))) AS material_valid,
 (SELECT count(*) FROM communityhero.knowledge_entries peer WHERE peer.workspace_id=w.id AND
 (peer.id=e.id OR peer.payload->>'id'=e.id OR peer.source_material_id=e.source_material_id OR peer.payload->>'sourceMaterialId'=e.source_material_id)) AS peers
FROM communityhero.workspaces w
LEFT JOIN communityhero.knowledge_versions v ON v.workspace_id=w.id AND
 (($3 AND (v.id=$2 OR v.payload->>'id'=$2)) OR (NOT $3 AND v.id IN
 (SELECT head.current_version_id FROM communityhero.knowledge_entries head WHERE head.workspace_id=w.id AND (head.id=$2 OR head.payload->>'id'=$2))))
LEFT JOIN communityhero.knowledge_entries e ON e.workspace_id=w.id AND
 (($3 AND (e.id=v.entry_id OR e.id=v.payload->>'entryId')) OR (NOT $3 AND (e.id=$2 OR e.payload->>'id'=$2)))
WHERE w.id=$1 ORDER BY e.ordinal,v.ordinal LIMIT 2
"#;
const SQLITE_KNOWLEDGE_EXACT:&str=r#"
SELECT json_extract(w.payload,'$.account') AS account,json_extract(w.payload,'$.connectorBinding') AS binding,
 json_extract(e.value,'$.id') AS entry_id,json_extract(v.value,'$.id') AS version_id,json_extract(e.value,'$.currentVersionId') AS current_version_id,
 length(CAST(e.value AS BLOB))+COALESCE(length(CAST(v.value AS BLOB)),0) AS bytes,
 CASE WHEN sum(length(CAST(e.value AS BLOB))+COALESCE(length(CAST(v.value AS BLOB)),0)) OVER()<=?3 THEN e.value END AS entry,
 CASE WHEN sum(length(CAST(e.value AS BLOB))+COALESCE(length(CAST(v.value AS BLOB)),0)) OVER()<=?3 THEN v.value END AS version,
 (json_extract(e.value,'$.sourceMaterialId') IS NULL OR (SELECT count(*)=1 FROM json_each(w.payload,'$.materials') m WHERE json_extract(m.value,'$.id')=json_extract(e.value,'$.sourceMaterialId'))) AS material_valid,
 (SELECT count(*) FROM json_each(w.payload,'$.knowledge_entries') peer WHERE
 json_extract(peer.value,'$.id')=json_extract(e.value,'$.id') OR json_extract(peer.value,'$.sourceMaterialId')=json_extract(e.value,'$.sourceMaterialId')) AS peers
FROM workspace w
LEFT JOIN json_each(w.payload,'$.knowledge_versions') v ON
 ((?2 AND json_extract(v.value,'$.id')=?1) OR (NOT ?2 AND json_extract(v.value,'$.id') IN
 (SELECT json_extract(head.value,'$.currentVersionId') FROM json_each(w.payload,'$.knowledge_entries') head WHERE json_extract(head.value,'$.id')=?1)))
LEFT JOIN json_each(w.payload,'$.knowledge_entries') e ON
 ((?2 AND json_extract(e.value,'$.id')=json_extract(v.value,'$.entryId')) OR (NOT ?2 AND json_extract(e.value,'$.id')=?1))
WHERE w.id=1 ORDER BY CAST(e.key AS INTEGER),CAST(v.key AS INTEGER) LIMIT 2
"#;
impl Database {
    pub(crate) async fn read_knowledge_entry(&self,id:&str,expected_version_id:Option<&str>)->ApiResult<Value> {
        self.read_knowledge_exact(id,false,expected_version_id).await
    }
    pub(crate) async fn read_knowledge_version(&self,id:&str)->ApiResult<Value> {
        self.read_knowledge_exact(id,true,None).await
    }
    async fn read_knowledge_exact(&self,id:&str,historical:bool,expected_version_id:Option<&str>)->ApiResult<Value> {
        bounded_id(id)?;if let Some(expected)=expected_version_id{bounded_id(expected)?;}
        let mut identity=Value::Null;let mut pairs=vec![];
        match self {
            Self::Postgres{reader,..}=>{
                let records=sqlx::query(PG_KNOWLEDGE_EXACT).bind(WORKSPACE).bind(id).bind(historical).bind(KNOWLEDGE_RAW_BYTES as i64).fetch_all(reader).await?;
                for record in records {
                    postgres_guard(&record)?;identity=knowledge_identity(record.try_get("account")?,record.try_get("binding")?)?;
                    let selected_exists=record.try_get::<Option<&str>,_>(if historical{"version_id"}else{"entry_id"})?.is_some();
                    if !selected_exists{continue;}
                    let current_version:Option<&str>=record.try_get("current_version_id")?;
                    if !historical&&expected_version_id.is_some_and(|expected|current_version!=Some(expected)){return Err(ApiError(StatusCode::CONFLICT,"Knowledge current version changed".into()));}
                    let size=record.try_get::<Option<i64>,_>("bytes")?.ok_or_else(||internal("Selected knowledge entry missing"))?;
                    if size<0||size as usize>KNOWLEDGE_RAW_BYTES{return Err(diagnostic_budget("knowledge_exact"));}
                    let entry=parse(record.try_get::<Option<&str>,_>("entry")?.ok_or_else(||diagnostic_budget("knowledge_exact"))?)?;
                    let version=parse(record.try_get::<Option<&str>,_>("version")?.ok_or_else(||internal("Missing selected knowledge version"))?)?;
                    if !record.try_get::<bool,_>("material_valid")?||record.try_get::<i64,_>("peers")?!=1||entry["id"].as_str()!=record.try_get::<Option<&str>,_>("entry_id")?
                        ||entry["currentVersionId"].as_str()!=record.try_get::<Option<&str>,_>("current_version_id")?
                        ||entry["sourceMaterialId"].as_str()!=record.try_get::<Option<&str>,_>("source_material_id")?
                        ||version["id"].as_str()!=record.try_get::<Option<&str>,_>("version_id")?
                        ||version["entryId"].as_str()!=record.try_get::<Option<&str>,_>("version_entry_id")?
                        ||version["sourceMaterialId"].as_str()!=record.try_get::<Option<&str>,_>("version_source_material_id")?{
                        return Err(internal("Selected knowledge relational identity mismatch"));
                    }
                    pairs.push((entry,version));
                }
            }
            Self::Sqlite(pool)=>{
                let records=sqlx::query(SQLITE_KNOWLEDGE_EXACT).bind(id).bind(historical).bind(KNOWLEDGE_RAW_BYTES as i64).fetch_all(pool).await?;
                for record in records {
                    identity=knowledge_identity(record.try_get("account")?,record.try_get("binding")?)?;
                    let exists=record.try_get::<Option<&str>,_>(if historical{"version_id"}else{"entry_id"})?.is_some();
                    if !exists{continue;}
                    let current_version:Option<&str>=record.try_get("current_version_id")?;
                    if !historical&&expected_version_id.is_some_and(|expected|current_version!=Some(expected)){return Err(ApiError(StatusCode::CONFLICT,"Knowledge current version changed".into()));}
                    let size=record.try_get::<Option<i64>,_>("bytes")?.ok_or_else(||internal("Selected knowledge entry missing"))?;
                    if size<0||size as usize>KNOWLEDGE_RAW_BYTES{return Err(diagnostic_budget("knowledge_exact"));}
                    let entry=parse(record.try_get::<Option<&str>,_>("entry")?.ok_or_else(||diagnostic_budget("knowledge_exact"))?)?;
                    let version=parse(record.try_get::<Option<&str>,_>("version")?.ok_or_else(||internal("Missing selected knowledge version"))?)?;
                    if !record.try_get::<bool,_>("material_valid")?||record.try_get::<i64,_>("peers")?!=1{return Err(internal("Duplicate selected knowledge head"));}
                    pairs.push((entry,version));
                }
            }
        }
        if identity.is_null(){return Err(internal("Knowledge workspace missing"));}
        if pairs.is_empty(){return Err(ApiError(StatusCode::NOT_FOUND,"Selected knowledge record missing".into()));}
        if pairs.len()!=1{return Err(internal("Duplicate selected knowledge record"));}
        let(entry,version)=pairs.pop().unwrap();let account=text(&identity,"account")?;
        if (if historical{version["id"].as_str()}else{entry["id"].as_str()})!=Some(id){return Err(internal("Selected knowledge identity mismatch"));}
        let current=entry["currentVersionId"]==version["id"];
        validate_knowledge_pair(account,&entry,&version,current)?;
        if !historical&&!current{return Err(internal("Current selected knowledge version mismatch"));}
        if expected_version_id.is_some_and(|expected|entry["currentVersionId"]!=expected){return Err(ApiError(StatusCode::CONFLICT,"Knowledge current version changed".into()));}
        knowledge_response_budget(serde_json::json!({"schemaVersion":1,"account":account,"connectorBinding":identity["connectorBinding"],
            "observedAt":crate::now(),"selection":{"kind":if historical{"exact_version"}else{"exact_entry"},"id":id},
            "entry":entry,"version":version,"isCurrent":current,"coverage":{"complete":true,"historyIncluded":historical}}),KNOWLEDGE_RAW_BYTES)
    }
}
#[cfg(test)]
#[path = "storage_reads_tests.rs"]
mod tests;
