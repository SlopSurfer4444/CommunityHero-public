//! Cheap maintenance observations, never eligibility or admission authority.
//! One SQL statement captures settings, workflow totals and only job counter
//! fields. Paid bundles, media results, branch history and the ledger stay out.
use super::*;

const PG_SUMMARY:&str=r#"
WITH item_heads AS MATERIALIZED (
 SELECT payload->'workflow' AS workflow,payload#>'{autoPreparation,status}' AS preparation_status
 FROM communityhero.items WHERE workspace_id=$1
)
SELECT w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
  AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 json_build_object('settings',json_build_object('autoPreparation',json_build_object(
  'revalidation',w.metadata#>'{settings,autoPreparation,revalidation}',
  'publicFactFollowup',w.metadata#>'{settings,autoPreparation,publicFactFollowup}')),
  'maintenancePreparationWorkflow',json_build_object(
   'prepared',(SELECT count(*) FROM item_heads WHERE workflow='"prepared"'::jsonb),
   'needsAttention',(SELECT count(*) FROM item_heads WHERE workflow='"attention"'::jsonb),
   'stale',(SELECT count(*) FROM item_heads WHERE workflow='"attention"'::jsonb AND preparation_status='"stale"'::jsonb)),
  'jobs',COALESCE((SELECT json_agg(json_build_object(
   'kind',j.payload->'kind','status',j.payload->'status','purpose',j.payload->'purpose',
   'claimedAt',j.payload->'claimedAt','prepareOutcome',json_build_object('status',j.payload#>'{prepareOutcome,status}')) ORDER BY j.ordinal)
   FROM communityhero.jobs j WHERE j.workspace_id=w.id AND
    (j.payload->'purpose'='"auto_revalidate"'::jsonb OR
     (j.payload->'kind'='"assistant"'::jsonb AND j.payload->'status' IN ('"queued"'::jsonb,'"running"'::jsonb)))),'[]'::json))::text AS projection
FROM communityhero.workspaces w WHERE w.id=$1
"#;

const SQLITE_SUMMARY:&str=r#"
WITH item_heads AS MATERIALIZED (
 SELECT json_extract(i.value,'$.workflow') AS workflow,json_extract(i.value,'$.autoPreparation.status') AS preparation_status
 FROM workspace w,json_each(CASE WHEN json_type(w.payload,'$.items')='array' THEN json_extract(w.payload,'$.items') ELSE '[]' END) i
 WHERE w.id=1 AND i.type='object'
), job_heads AS MATERIALIZED (
 SELECT j.value,CAST(j.key AS INTEGER) AS ordinal FROM workspace w,
 json_each(CASE WHEN json_type(w.payload,'$.jobs')='array' THEN json_extract(w.payload,'$.jobs') ELSE '[]' END) j
 WHERE w.id=1 AND j.type='object'
)
SELECT json_object('settings',json_object('autoPreparation',json_object(
 'revalidation',json(CASE WHEN json_type(w.payload,'$.settings.autoPreparation.revalidation')='object'
  THEN json_extract(w.payload,'$.settings.autoPreparation.revalidation') ELSE 'null' END),
 'publicFactFollowup',json(CASE WHEN json_type(w.payload,'$.settings.autoPreparation.publicFactFollowup')='object'
  THEN json_extract(w.payload,'$.settings.autoPreparation.publicFactFollowup') ELSE 'null' END))),
 'maintenancePreparationWorkflow',json_object(
  'prepared',(SELECT count(*) FROM item_heads WHERE workflow='prepared'),
  'needsAttention',(SELECT count(*) FROM item_heads WHERE workflow='attention'),
  'stale',(SELECT count(*) FROM item_heads WHERE workflow='attention' AND preparation_status='stale')),
 'jobs',json(COALESCE((SELECT json_group_array(json_object(
  'kind',json_extract(value,'$.kind'),'status',json_extract(value,'$.status'),'purpose',json_extract(value,'$.purpose'),
  'claimedAt',json_extract(value,'$.claimedAt'),'prepareOutcome',json_object('status',json_extract(value,'$.prepareOutcome.status'))))
  FROM (SELECT * FROM job_heads WHERE json_extract(value,'$.purpose')='auto_revalidate' OR
   (json_extract(value,'$.kind')='assistant' AND json_extract(value,'$.status') IN ('queued','running')) ORDER BY ordinal)),'[]'))) AS projection
FROM workspace w WHERE w.id=1
"#;

impl Database {
    pub(crate) async fn read_preparation_status_summary(&self)->ApiResult<Value>{
        let payload=match self {
            Self::Sqlite(pool)=>sqlx::query_scalar::<_,String>(SQLITE_SUMMARY).fetch_one(pool).await?,
            Self::Postgres{reader,..}=>{
                let record=sqlx::query(PG_SUMMARY).bind(WORKSPACE).fetch_one(reader).await?;
                postgres_guard(&record)?;
                record.try_get::<String,_>("projection")?
            }
        };
        // fetch_one has returned the pooled connection before JSON decoding.
        parse(&payload)
    }
}

#[cfg(test)]
#[path="storage_preparation_status_tests.rs"]
mod tests;
