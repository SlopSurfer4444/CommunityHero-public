//! Hot read projections. Each query sees one database statement snapshot; these
//! are preflight observations, never substitutes for transactional claim checks.
use super::*;
use sqlx::Connection;

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
               'retryAt',p.value->'retryAt','queuedAt',p.value->'queuedAt'))
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
        'queuedAt',json_extract(CASE WHEN p.type='object' THEN p.value ELSE '{}' END,'$.queuedAt')))
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
       (jsonb_build_object(
         'entries',COALESCE((SELECT jsonb_agg(e.payload ORDER BY e.ordinal)
           FROM communityhero.knowledge_entries e WHERE e.workspace_id=w.id),'[]'::jsonb),
         'versions',COALESCE((SELECT jsonb_agg(v.payload ORDER BY v.ordinal)
           FROM communityhero.knowledge_versions v WHERE v.workspace_id=w.id),'[]'::jsonb))
        || CASE WHEN $2 THEN jsonb_build_object('feedback',COALESCE((SELECT jsonb_agg(f.payload ORDER BY f.ordinal)
             FROM communityhero.feedback f WHERE f.workspace_id=w.id),'[]'::jsonb))
           ELSE '{}'::jsonb END)::text AS projection
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
const PG_BOOTSTRAP: &str = r#"
SELECT w.execution_enabled,
       (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
        AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
       ((w.metadata-'companyKnowledgeCoverage') || jsonb_build_object(
         'posts',COALESCE((SELECT jsonb_agg(p.payload ORDER BY p.ordinal) FROM communityhero.posts p WHERE p.workspace_id=w.id),'[]'::jsonb),
         'branches',COALESCE((SELECT jsonb_agg(b.payload-'observedMessages' ORDER BY b.ordinal) FROM communityhero.branches b WHERE b.workspace_id=w.id),'[]'::jsonb),
         'items',COALESCE((SELECT jsonb_agg(i.payload ORDER BY i.ordinal) FROM communityhero.items i WHERE i.workspace_id=w.id),'[]'::jsonb),
         'conversations',COALESCE((SELECT jsonb_agg(c.payload ORDER BY c.ordinal) FROM communityhero.conversations c WHERE c.workspace_id=w.id),'[]'::jsonb),
         'proposals',COALESCE((SELECT jsonb_agg(p.payload ORDER BY p.ordinal) FROM communityhero.proposals p WHERE p.workspace_id=w.id),'[]'::jsonb),
         'approvals',COALESCE((SELECT jsonb_agg(a.payload-'approvalAuthority' ORDER BY a.ordinal) FROM communityhero.approvals a WHERE a.workspace_id=w.id),'[]'::jsonb),
         'operations',COALESCE((SELECT jsonb_agg(o.payload-'dispatchAuthority' ORDER BY o.ordinal) FROM communityhero.operations o WHERE o.workspace_id=w.id),'[]'::jsonb),
         'materials',COALESCE((SELECT jsonb_agg(m.payload ORDER BY m.ordinal) FROM communityhero.materials m WHERE m.workspace_id=w.id),'[]'::jsonb),
         'mediaReadinessCatalog',jsonb_build_object(
           'knowledge_entries',COALESCE((SELECT jsonb_agg(e.payload ORDER BY e.ordinal)
             FROM communityhero.knowledge_entries e JOIN communityhero.knowledge_versions v
               ON v.workspace_id=e.workspace_id AND v.entry_id=e.id AND v.id=e.current_version_id
             WHERE e.workspace_id=w.id AND v.payload->>'kind' IN ('transcript','visual_context')),'[]'::jsonb),
           'knowledge_versions',COALESCE((SELECT jsonb_agg(v.payload ORDER BY e.ordinal)
             FROM communityhero.knowledge_entries e JOIN communityhero.knowledge_versions v
               ON v.workspace_id=e.workspace_id AND v.entry_id=e.id AND v.id=e.current_version_id
             WHERE e.workspace_id=w.id AND v.payload->>'kind' IN ('transcript','visual_context')),'[]'::jsonb)),
         'jobs',COALESCE((SELECT jsonb_agg(CASE WHEN jsonb_typeof(j.payload->'prepareBundle')='object'
           THEN j.payload #- '{prepareBundle,request}' ELSE j.payload END ORDER BY j.ordinal)
           FROM communityhero.jobs j WHERE j.workspace_id=w.id),'[]'::jsonb)))::text AS projection
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
  '$.jobs',json(COALESCE((SELECT json_group_array(json_remove(j.value,'$.prepareBundle.request')) FROM json_each(w.payload,'$.jobs') j),'[]'))) AS projection
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

impl Database {
    /// Coherent actor-neutral bootstrap input, with only already-hidden fields
    /// removed. The HTTP/cache layer still owns actor filtering and delta bases.
    pub(crate) async fn read_bootstrap_source(&self) -> ApiResult<Value> {
        let payload=match self {
            Self::Sqlite(pool)=>sqlx::query_scalar::<_,String>(SQLITE_BOOTSTRAP).fetch_one(pool).await?,
            Self::Postgres { reader: pool, .. }=>{
                let record=sqlx::query(PG_BOOTSTRAP).bind(WORKSPACE).fetch_one(pool).await?;
                postgres_guard(&record)?;
                record.try_get::<String,_>("projection")?
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
                record.try_get::<String, _>("projection")?
            }
        };
        parse(&payload)
    }

    /// Fetch one durable job, including its evidence. A missing job is `None`;
    /// a missing/invalid workspace remains an error. Never consult an app cache.
    pub(crate) async fn read_job(&self, key: &str) -> ApiResult<Option<Value>> {
        match self {
            Self::Sqlite(pool) => {
                // SQLite retains its one-document format. JSON1 avoids decoding
                // unrelated evidence into Rust even though SQLite scans the JSON.
                let payload: Option<String> = sqlx::query_scalar(
                    "SELECT (SELECT j.value FROM json_each(w.payload,'$.jobs') j WHERE json_extract(CASE WHEN j.type='object' THEN j.value ELSE '{}' END,'$.id')=? LIMIT 1) FROM workspace w WHERE w.id=1",
                ).bind(key).fetch_one(pool).await?;
                payload.map(|s| parse(&s)).transpose()
            }
            Self::Postgres { reader: pool, .. } => {
                let record = sqlx::query(
                    "SELECT w.execution_enabled,(jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string' AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,j.id,j.kind,j.status,j.ref_id,j.payload::text AS payload FROM communityhero.workspaces w LEFT JOIN communityhero.jobs j ON j.workspace_id=w.id AND j.id=$2 WHERE w.id=$1",
                ).bind(WORKSPACE).bind(key).fetch_one(pool).await?;
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

#[cfg(test)]
#[path = "storage_reads_tests.rs"]
mod tests;
