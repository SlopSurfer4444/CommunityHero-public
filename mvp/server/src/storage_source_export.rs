//! Read-only source export: one coherent account/sync/comment snapshot.
//! Omitted histories are never a persistence input or publishing authority.
//! Validate the selected records; startup/writes continue to own full-workspace
//! integrity. Exporting comments must not materialize unrelated paid/media data.
use super::*;
use serde_json::json;

const SOURCE_TABLES: [&str; 3] = ["items", "posts", "branches"];
const SQLITE_EXPORT: &str = r#"
SELECT json_object(
 'account',json_extract(w.payload,'$.account'),
 'sync',json(COALESCE(w.payload -> '$.sync','null')),
 'items',json(COALESCE(w.payload -> '$.items','null')),
 'posts',json(COALESCE(w.payload -> '$.posts','null')),
 'branches',json(COALESCE(w.payload -> '$.branches','null'))) AS projection
FROM workspace w WHERE w.id=1
"#;

// Use JSON text aggregation so the response never becomes a single JSONB
// container. Every lateral aggregate is scoped by the selected workspace,
// validates persisted ordinal/relational identity, and shares this statement's
// snapshot with account and sync. No writer connection or workspace lock.
const PG_EXPORT: &str = r#"
SELECT w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
  AND w.account=w.metadata->>'account'
  AND NOT (w.metadata ?| ARRAY['posts','branches','items','conversations',
    'proposals','approvals','operations','materials','jobs','audit',
    'knowledge_entries','knowledge_versions','feedback'])) IS TRUE AS identity_valid,
 json_build_object('account',w.metadata->'account','sync',w.metadata->'sync',
   'items',i.payload,'posts',p.payload,'branches',b.payload)::text AS projection,
 (i.valid AND p.valid AND b.valid) AS source_valid
FROM communityhero.workspaces w
LEFT JOIN LATERAL (
 SELECT COALESCE(json_agg(r.payload ORDER BY r.ordinal),'[]'::json) AS payload,
   COALESCE(bool_and((r.ordinal=r.expected_ordinal
     AND jsonb_typeof(r.payload)='object' AND jsonb_typeof(r.payload->'id')='string'
     AND r.id=r.payload->>'id'
     AND r.post_id IS NOT DISTINCT FROM r.payload->>'postId'
     AND r.branch_id IS NOT DISTINCT FROM r.payload->>'branchId') IS TRUE),true) AS valid
 FROM (SELECT x.*,row_number() OVER (ORDER BY x.ordinal)-1 AS expected_ordinal
   FROM communityhero.items x WHERE x.workspace_id=w.id) r
) i ON true
LEFT JOIN LATERAL (
 SELECT COALESCE(json_agg(r.payload ORDER BY r.ordinal),'[]'::json) AS payload,
   COALESCE(bool_and((r.ordinal=r.expected_ordinal
     AND jsonb_typeof(r.payload)='object' AND jsonb_typeof(r.payload->'id')='string'
     AND r.id=r.payload->>'id') IS TRUE),true) AS valid
 FROM (SELECT x.*,row_number() OVER (ORDER BY x.ordinal)-1 AS expected_ordinal
   FROM communityhero.posts x WHERE x.workspace_id=w.id) r
) p ON true
LEFT JOIN LATERAL (
 SELECT COALESCE(json_agg(r.payload ORDER BY r.ordinal),'[]'::json) AS payload,
   COALESCE(bool_and((r.ordinal=r.expected_ordinal
     AND jsonb_typeof(r.payload)='object' AND jsonb_typeof(r.payload->'id')='string'
     AND r.id=r.payload->>'id'
     AND r.post_id IS NOT DISTINCT FROM r.payload->>'postId') IS TRUE),true) AS valid
 FROM (SELECT x.*,row_number() OVER (ORDER BY x.ordinal)-1 AS expected_ordinal
   FROM communityhero.branches x WHERE x.workspace_id=w.id) r
) b ON true
WHERE w.id=$1
"#;

fn validate_source(view: &Value) -> ApiResult<usize> {
    text(view,"account")?;
    let mut ids = HashMap::new();
    let mut count = 0;
    for table in SOURCE_TABLES {
        let records=rows(view,table)?;
        let mut seen=HashSet::with_capacity(records.len());
        for record in records {
            if !record.is_object() || !seen.insert(text(record,"id")?) {
                return Err(internal("Invalid or duplicate export record ID"));
            }
            for (_,key) in projection(table) {
                if !record[*key].is_null() && !record[*key].is_string() {
                    return Err(internal("Invalid export reference"));
                }
            }
        }
        count += records.len();
        ids.insert(table,seen);
    }
    for table in ["items","branches"] {
        for record in rows(view,table)? {
            for (field,target) in [("postId","posts"),("branchId","branches")] {
                if table=="branches" && field=="branchId" { continue; }
                if !record[field].is_null() && !ids[target].contains(text(record,field)?) {
                    return Err(internal("Dangling export reference"));
                }
            }
        }
    }
    Ok(count)
}

impl Database {
    pub(crate) async fn read_source_export_context(&self) -> ApiResult<Value> {
        let mut total=crate::performance::Span::new("source.export.read.total");
        let payload=match self {
            Self::Sqlite(pool)=>{
                let wait=crate::performance::Span::new("source.export.reader.wait");
                let mut connection=pool.acquire().await?;
                drop(wait);
                let sql=crate::performance::Span::new("source.export.sql");
                let payload=sqlx::query_scalar::<_,String>(SQLITE_EXPORT)
                    .fetch_one(&mut *connection).await?;
                drop(sql);
                payload
            },
            Self::Postgres{reader,..}=>{
                let wait=crate::performance::Span::new("source.export.reader.wait");
                let mut connection=reader.acquire().await?;
                drop(wait);
                let sql=crate::performance::Span::new("source.export.sql");
                let record=sqlx::query(PG_EXPORT).bind(WORKSPACE)
                    .fetch_one(&mut *connection).await?;
                drop(sql);
                if record.try_get::<bool,_>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                if !record.try_get::<bool,_>("identity_valid")? {
                    return Err(internal("Workspace identity mismatch"));
                }
                if !record.try_get::<bool,_>("source_valid")? {
                    return Err(internal("Export relational identity or order mismatch"));
                }
                record.try_get::<String,_>("projection")?
            },
        };
        let decode=crate::performance::Span::new("source.export.decode");
        let view=parse(&payload)?;
        drop(decode);
        let validation=crate::performance::Span::new("source.export.validation");
        let row_count=validate_source(&view)?;
        drop(validation);
        total.counts(row_count,payload.len(),SOURCE_TABLES.len());
        Ok(view)
    }
}

#[cfg(test)]
#[path="storage_source_export_tests.rs"]
mod tests;
