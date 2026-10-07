//! One-statement read-only media status. Both engines filter before transferring
//! a bounded page; no bootstrap history cap, writer lease, or provider call.
use super::*;
use serde_json::json;

const PAGE:u64=100;
// Exact source-owned error codes only. Never infer a category from free-form
// diagnostics: a suffix containing a URL, path or secret must remain unknown.
// Both SQL engines consume this same closed list; classification grants no retry.
const FAILURE_CODES:&[&str]=&[
    "source_download_failed","source_download_failed_network","source_download_failed_rate_limited",
    "source_download_failed_auth","source_download_failed_challenge","source_download_failed_private",
    "source_download_failed_geoblocked","source_download_failed_url_expired","source_download_failed_tls",
    "source_download_failed_dependency","source_download_failed_impersonation","source_download_failed_js_runtime",
    "source_download_failed_format_unavailable","source_download_failed_unsupported_url","source_download_failed_http_forbidden",
    "source_download_failed_unavailable","source_download_failed_extractor","source_download_failed_disk_full",
    "source_download_failed_permission","source_download_failed_integrity","source_download_failed_timeout",
    "source_download_failed_process_unknown","source_download_failed_spawn_failed","source_download_failed_wait_failed",
    "source_download_failed_unknown","source_file_missing","source_file_too_large",
    "audio_duration_unknown","audio_duration_limit","audio_inventory_failed","audio_coverage_incomplete",
    "audio_transcript_limit","audio_scratch_cleanup_failed","whisper_failed","ffmpeg_failed",
    "audio_reuse_account_invalid","audio_reuse_account_changed","audio_reuse_post_missing",
    "audio_reuse_source_changed","audio_reuse_source_invalid","audio_reuse_evidence_changed",
    "media_source_identity_mismatch","media_source_artifact_unavailable","media_cached_audio_source_unavailable",
    "media_source_projection_missing","media_source_artifact_failed","media_evidence_artifact_unavailable",
    "media_artifact_path_changed","media_artifact_size_changed","media_artifact_changed_during_verification",
    "visual_source_identity_mismatch","visual_aggregate_overflow","visual_video_stream_missing",
    "visual_duration_unknown","visual_finalize_failed","media_completion_not_proven",
];
fn failure_codes_sql()->String{
    FAILURE_CODES.iter().map(|code|format!("'{code}'")).collect::<Vec<_>>().join(",")
}
fn sqlite_foreign(value:&str)->String{
    let mut checks=vec![format!("(json_type({value},'$.account') IS NOT NULL AND json_extract({value},'$.account') IS NOT json_extract(w.payload,'$.account'))")];
    for path in ["connectorBinding","result.visualProgress.connectorBinding"] {
        let fields=["id","workspaceId","accountId","connector","revision","providerAccountId"].map(|key|
            format!("json_extract({value},'$.{path}.{key}') IS NOT json_extract(w.payload,'$.connectorBinding.{key}')")).join(" OR ");
        checks.push(format!("(json_type({value},'$.{path}') IS NOT NULL AND json_type({value},'$.{path}')!='null' AND (json_type({value},'$.{path}')!='object' OR {fields}))"));
    }
    checks.push(format!("(json_type({value},'$.result.visualProgress.account') IS NOT NULL AND json_extract({value},'$.result.visualProgress.account') IS NOT json_extract(w.payload,'$.account'))"));
    checks.join(" OR ")
}
// These fields preserve source_version, normalized title reuse, company imports
// and observed identity conflicts exactly. No post/dialogue history is needed.
const POST_FIELDS:&[&str]=&["id","postKey","account","accountId","scope","sourceMediaScope","connectorBinding","title","text","body","attachments","sourceUrl","attachmentSourceUrl","url","canonicalMediaId","contentSha256","mediaSha256","durationMs","durationSeconds"];
const DURATION_JOB_FIELDS:&[&str]=&["kind","purpose","visualContractVersion","account","connectorBinding","status","refId","createdAt"];
const DURATION_PROGRESS_FIELDS:&[&str]=&["phase","resumePhase","schemaVersion","sourcePostId","sourcePostKey","sourceVersion","account","connectorBinding","sourceIdentity","source"];
const POLICY_FIELDS:&[&str]=&["postMediaPolicies","mediaAudioEquivalences","mediaPolicyDefaults"];
fn field_sql(fields:&[&str])->String{fields.iter().map(|field|format!("'{field}'")).collect::<Vec<_>>().join(",")}
fn sqlite_object(value:&str,fields:&[&str])->String{
    format!("json(COALESCE((SELECT json_group_object(k.key,json(CASE WHEN k.type IN ('object','array') THEN k.value WHEN k.type='true' THEN 'true' WHEN k.type='false' THEN 'false' ELSE json_quote(k.value) END)) FROM json_each({value}) k WHERE k.key IN ({})),'{{}}'))",field_sql(fields))
}
fn pg_object(value:&str,fields:&[&str])->String{
    format!("COALESCE((SELECT jsonb_object_agg(k.key,k.value) FROM jsonb_each(CASE WHEN jsonb_typeof({value})='object' THEN {value} ELSE '{{}}'::jsonb END) k WHERE k.key IN ({})),'{{}}'::jsonb)",field_sql(fields))
}
fn sqlite_query()->String{
    let codes=failure_codes_sql();
    let worker_codes=field_sql(crate::media_queue::WORKER_BLOCK_CODES);
    let attempt_foreign=sqlite_foreign("a.value");
    let foreign=format!("({}) OR EXISTS(SELECT 1 FROM json_each(m.value,'$.sourceAttempts') a WHERE json_extract(a.value,'$.postId')=?1 AND ({attempt_foreign}))",sqlite_foreign("m.value"));
    let post_foreign=sqlite_foreign("p.value");
    let error="COALESCE(json_extract(value,'$.error'),json_extract(value,'$.result.visualProgress.error'))";
    let attempt_error="(SELECT json_extract(a.value,'$.error') FROM json_each(page.value,'$.sourceAttempts') a WHERE json_extract(a.value,'$.postId')=?1 AND json_extract(a.value,'$.error') IS NOT NULL ORDER BY CAST(a.key AS INTEGER) DESC LIMIT 1)";
    let post=sqlite_object("p.value",POST_FIELDS);
    let settings=sqlite_object("json_extract(w.payload,'$.settings')",POLICY_FIELDS);
    let duration_job=sqlite_object("j.value",DURATION_JOB_FIELDS);
    let duration_progress=sqlite_object("json_extract(j.value,'$.result.visualProgress')",DURATION_PROGRESS_FIELDS);
    format!(r#"
WITH selected AS (SELECT p.value FROM workspace w,json_each(w.payload,'$.posts') p WHERE w.id=1 AND json_extract(p.value,'$.id')=?1),
heads AS (SELECT e.value,CAST(e.key AS INTEGER) ordinal FROM workspace w,json_each(w.payload,'$.knowledge_entries') e WHERE w.id=1),
versions AS (SELECT v.value FROM workspace w,json_each(w.payload,'$.knowledge_versions') v WHERE w.id=1),
matched AS (SELECT j.value,CAST(j.key AS INTEGER) ordinal FROM workspace w,json_each(w.payload,'$.jobs') j
 WHERE w.id=1 AND json_extract(j.value,'$.kind') IN ('media','media_audio') AND
 (json_extract(j.value,'$.refId')=?1 OR json_extract(j.value,'$.result.visualProgress.sourcePostId')=?1 OR
 EXISTS(SELECT 1 FROM json_each(j.value,'$.sourceAttempts') a WHERE json_extract(a.value,'$.postId')=?1))),
page AS (SELECT * FROM matched ORDER BY ordinal LIMIT 100 OFFSET ?2)
SELECT json_object('account',json_extract(w.payload,'$.account'),'connectorBinding',json_extract(w.payload,'$.connectorBinding'),
 'catalogInvalid',(SELECT count(*) FROM heads e WHERE (SELECT count(*) FROM versions v WHERE json_extract(v.value,'$.id')=json_extract(e.value,'$.currentVersionId') AND json_extract(v.value,'$.entryId')=json_extract(e.value,'$.id') AND json_extract(v.value,'$.sourceMaterialId') IS json_extract(e.value,'$.sourceMaterialId') AND json_extract(v.value,'$.kind') IS json_extract(e.value,'$.kind') AND json_extract(v.value,'$.status') IS json_extract(e.value,'$.status')
 AND NOT EXISTS(SELECT fullkey,type,atom FROM json_tree(e.value,'$.scope') EXCEPT SELECT fullkey,type,atom FROM json_tree(v.value,'$.scope'))
 AND NOT EXISTS(SELECT fullkey,type,atom FROM json_tree(v.value,'$.scope') EXCEPT SELECT fullkey,type,atom FROM json_tree(e.value,'$.scope')))!=1 OR (SELECT count(*) FROM heads other WHERE json_extract(other.value,'$.id')=json_extract(e.value,'$.id'))!=1 OR (SELECT count(*) FROM heads other WHERE json_extract(other.value,'$.sourceMaterialId') IS json_extract(e.value,'$.sourceMaterialId'))!=1),
 'readinessInput',json_object('account',json_extract(w.payload,'$.account'),'connectorBinding',json_extract(w.payload,'$.connectorBinding'),
 'posts',json(COALESCE((SELECT json_group_array({post}) FROM json_each(w.payload,'$.posts') p),'[]')),
 'knowledge_entries',json(COALESCE((SELECT json_group_array(json(e.value)) FROM heads e ORDER BY ordinal),'[]')),
 'knowledge_versions',json(COALESCE((SELECT json_group_array(json(v.value)) FROM versions v WHERE EXISTS(SELECT 1 FROM heads e WHERE json_extract(e.value,'$.currentVersionId')=json_extract(v.value,'$.id'))),'[]')),
 'settings',{settings},'jobs',json(COALESCE((SELECT json_group_array(json_patch({duration_job},json_object('result',json_object('visualProgress',{duration_progress})))) FROM json_each(w.payload,'$.jobs') j WHERE json_extract(j.value,'$.kind')='media' AND json_extract(j.value,'$.purpose')='auto_media' AND json_extract(j.value,'$.refId')=?1),'[]'))),
 'postCount',(SELECT count(*) FROM selected),'postId',?1,
 'foreignCount',(SELECT count(*) FROM matched m WHERE {foreign})+(SELECT count(*) FROM selected p WHERE {post_foreign}),
 'total',(SELECT count(*) FROM matched),
 'jobs',json(COALESCE((SELECT json_group_array(json(summary)) FROM (SELECT json_object(
 'id',json_extract(value,'$.id'),'kind',json_extract(value,'$.kind'),'status',json_extract(value,'$.status'),
 'createdAt',json_extract(value,'$.createdAt'),'startedAt',json_extract(value,'$.startedAt'),'finishedAt',json_extract(value,'$.finishedAt'),
 'workerBlockStage',CASE WHEN json_extract(value,'$.workerBlock.stage') IN ('download','inventory','select','scan','finalize','audio','unknown') THEN json_extract(value,'$.workerBlock.stage') ELSE NULL END,
 'workerBlockCode',CASE WHEN json_extract(value,'$.workerBlock.code') IN ({worker_codes}) THEN json_extract(value,'$.workerBlock.code') WHEN json_type(value,'$.workerBlock')='object' THEN 'media_runtime_not_ready' ELSE NULL END,
 'resourceWaitResource',CASE WHEN json_extract(value,'$.resourceWait.resource')='gpu' THEN 'gpu' ELSE NULL END,
 'resourceWaitState',CASE WHEN json_extract(value,'$.resourceWait.state') IN ('waiting','exhausted') THEN json_extract(value,'$.resourceWait.state') ELSE NULL END,
 'resourceWaitReason',CASE WHEN json_extract(value,'$.resourceWait.reason') IN ('gpu_busy','gpu_gate_resource_wait_exhausted') THEN json_extract(value,'$.resourceWait.reason') ELSE NULL END,
 'resourceWaitOperation',CASE WHEN json_extract(value,'$.resourceWait.operation') IN ('media_vision_chunk','whisper_asr','media_vision') THEN json_extract(value,'$.resourceWait.operation') ELSE NULL END,
 'resourceWaitStartedAt',json_extract(value,'$.resourceWait.startedAtUtc'),
 'resourceWaitMaxSeconds',CASE WHEN json_type(value,'$.resourceWait.maxWaitSeconds')='integer' THEN json_extract(value,'$.resourceWait.maxWaitSeconds') ELSE NULL END,
 'phase',CASE WHEN json_extract(value,'$.result.visualProgress.phase') IN ('download','inventory','select','scan','finalize','held','complete') THEN json_extract(value,'$.result.visualProgress.phase') ELSE NULL END,
 'nextSelectionIndex',CASE WHEN json_type(value,'$.result.visualProgress.nextSelectionIndex')='integer' THEN json_extract(value,'$.result.visualProgress.nextSelectionIndex') ELSE NULL END,
 'completedSelectedFrames',CASE WHEN json_type(value,'$.result.visualProgress.completedSelectedFrames')='integer' THEN json_extract(value,'$.result.visualProgress.completedSelectedFrames') ELSE NULL END,
 'failureClass',CASE WHEN {error} IN ({codes}) THEN {error} WHEN {error} IS NULL THEN NULL ELSE 'unknown' END,
 'sourceAttemptFailureClass',CASE WHEN {attempt_error} IN ({codes}) THEN {attempt_error} WHEN {attempt_error} IS NULL THEN NULL ELSE 'unknown' END
 ) summary FROM page ORDER BY ordinal)),'[]'))) FROM workspace w WHERE w.id=1
"#)
}
fn pg_query()->String{
    let codes=failure_codes_sql();
    let worker_codes=field_sql(crate::media_queue::WORKER_BLOCK_CODES);
    let error="COALESCE(NULLIF(payload->'error','null'::jsonb),NULLIF(payload#>'{result,visualProgress,error}','null'::jsonb))";
    let attempt_error="(SELECT a.value->'error' FROM jsonb_array_elements(CASE WHEN jsonb_typeof(page.payload->'sourceAttempts')='array' THEN page.payload->'sourceAttempts' ELSE '[]'::jsonb END) WITH ORDINALITY a(value,n) WHERE a.value->>'postId'=$2 AND a.value->'error' IS NOT NULL AND a.value->'error'!='null'::jsonb ORDER BY n DESC LIMIT 1)";
    let post=pg_object("p.payload",POST_FIELDS);
    let settings=pg_object("COALESCE(w.metadata->'settings','{}'::jsonb)",POLICY_FIELDS);
    let duration_job=pg_object("j.payload",DURATION_JOB_FIELDS);
    let duration_progress=pg_object("COALESCE(j.payload#>'{result,visualProgress}','{}'::jsonb)",DURATION_PROGRESS_FIELDS);
    format!(r#"
WITH selected AS (SELECT payload FROM communityhero.posts WHERE workspace_id=$1 AND id=$2 AND payload->>'id'=$2),
heads AS (SELECT e.* FROM communityhero.knowledge_entries e WHERE e.workspace_id=$1),
versions AS (SELECT v.* FROM communityhero.knowledge_versions v WHERE v.workspace_id=$1),
matched AS (SELECT j.payload,j.ordinal FROM communityhero.jobs j WHERE j.workspace_id=$1 AND j.kind IN ('media','media_audio') AND
 (j.payload->>'refId'=$2 OR j.payload#>>'{{result,visualProgress,sourcePostId}}'=$2 OR EXISTS
 (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(j.payload->'sourceAttempts')='array' THEN j.payload->'sourceAttempts' ELSE '[]'::jsonb END) a WHERE a->>'postId'=$2))),
page AS (SELECT * FROM matched ORDER BY ordinal LIMIT 100 OFFSET $3)
SELECT w.execution_enabled,(w.account=$4 AND w.metadata->>'account'=w.account) IS TRUE AS identity_valid,
jsonb_build_object('account',w.account,'connectorBinding',w.metadata->'connectorBinding','postId',$2::text,
'postCatalogInvalid',(SELECT count(*) FROM communityhero.posts p WHERE p.workspace_id=w.id AND p.id IS DISTINCT FROM p.payload->>'id'),
'catalogInvalid',(SELECT count(*) FROM heads e WHERE e.id IS DISTINCT FROM e.payload->>'id' OR e.source_material_id IS DISTINCT FROM e.payload->>'sourceMaterialId' OR e.current_version_id IS DISTINCT FROM e.payload->>'currentVersionId' OR (SELECT count(*) FROM versions v WHERE v.id=e.current_version_id AND v.payload->>'id'=v.id AND v.entry_id=e.id AND v.payload->>'entryId'=e.id AND v.source_material_id=e.source_material_id AND v.payload->>'sourceMaterialId'=v.source_material_id AND v.payload->'kind' IS NOT DISTINCT FROM e.payload->'kind' AND v.payload->'status' IS NOT DISTINCT FROM e.payload->'status' AND v.payload->'scope' IS NOT DISTINCT FROM e.payload->'scope')!=1),
'readinessInput',jsonb_build_object('account',w.account,'connectorBinding',w.metadata->'connectorBinding',
'posts',COALESCE((SELECT jsonb_agg({post} ORDER BY p.ordinal) FROM communityhero.posts p WHERE p.workspace_id=w.id),'[]'::jsonb),
'knowledge_entries',COALESCE((SELECT jsonb_agg(e.payload ORDER BY e.ordinal) FROM heads e),'[]'::jsonb),
'knowledge_versions',COALESCE((SELECT jsonb_agg(v.payload ORDER BY v.ordinal) FROM versions v WHERE EXISTS(SELECT 1 FROM heads e WHERE e.current_version_id=v.id)),'[]'::jsonb),
'settings',{settings},'jobs',COALESCE((SELECT jsonb_agg({duration_job} || jsonb_build_object('result',jsonb_build_object('visualProgress',{duration_progress})) ORDER BY j.ordinal) FROM communityhero.jobs j WHERE j.workspace_id=w.id AND j.kind='media' AND j.payload->>'purpose'='auto_media' AND j.payload->>'refId'=$2),'[]'::jsonb)),
'postCount',(SELECT count(*) FROM selected),'total',(SELECT count(*) FROM matched),
'foreignCount',(SELECT count(*) FROM (SELECT payload FROM matched UNION ALL SELECT payload FROM selected) x WHERE
 (x.payload ? 'account' AND x.payload->>'account' IS DISTINCT FROM w.account) OR
 (x.payload->'connectorBinding' IS NOT NULL AND x.payload->'connectorBinding'!='null'::jsonb AND x.payload->'connectorBinding' IS DISTINCT FROM w.metadata->'connectorBinding') OR
 (x.payload#>'{{result,visualProgress,connectorBinding}}' IS NOT NULL AND x.payload#>'{{result,visualProgress,connectorBinding}}'!='null'::jsonb AND x.payload#>'{{result,visualProgress,connectorBinding}}' IS DISTINCT FROM w.metadata->'connectorBinding') OR
 (x.payload#>'{{result,visualProgress,account}}' IS NOT NULL AND x.payload#>>'{{result,visualProgress,account}}' IS DISTINCT FROM w.account) OR EXISTS
 (SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(x.payload->'sourceAttempts')='array' THEN x.payload->'sourceAttempts' ELSE '[]'::jsonb END) a WHERE a->>'postId'=$2 AND
 ((a ? 'account' AND a->>'account' IS DISTINCT FROM w.account) OR (a->'connectorBinding' IS NOT NULL AND a->'connectorBinding'!='null'::jsonb AND a->'connectorBinding' IS DISTINCT FROM w.metadata->'connectorBinding')))),
'jobs',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',payload->'id','kind',payload->'kind','status',payload->'status',
'createdAt',payload->'createdAt','startedAt',payload->'startedAt','finishedAt',payload->'finishedAt',
'workerBlockStage',CASE WHEN payload#>>'{{workerBlock,stage}}' IN ('download','inventory','select','scan','finalize','audio','unknown') THEN payload#>'{{workerBlock,stage}}' ELSE NULL END,
'workerBlockCode',CASE WHEN payload#>>'{{workerBlock,code}}' IN ({worker_codes}) THEN payload#>'{{workerBlock,code}}' WHEN jsonb_typeof(payload->'workerBlock')='object' THEN '"media_runtime_not_ready"'::jsonb ELSE NULL END,
'resourceWaitResource',CASE WHEN payload#>>'{{resourceWait,resource}}'='gpu' THEN '"gpu"'::jsonb ELSE NULL END,
'resourceWaitState',CASE WHEN payload#>>'{{resourceWait,state}}' IN ('waiting','exhausted') THEN payload#>'{{resourceWait,state}}' ELSE NULL END,
'resourceWaitReason',CASE WHEN payload#>>'{{resourceWait,reason}}' IN ('gpu_busy','gpu_gate_resource_wait_exhausted') THEN payload#>'{{resourceWait,reason}}' ELSE NULL END,
'resourceWaitOperation',CASE WHEN payload#>>'{{resourceWait,operation}}' IN ('media_vision_chunk','whisper_asr','media_vision') THEN payload#>'{{resourceWait,operation}}' ELSE NULL END,
'resourceWaitStartedAt',payload#>'{{resourceWait,startedAtUtc}}',
'resourceWaitMaxSeconds',CASE WHEN jsonb_typeof(payload#>'{{resourceWait,maxWaitSeconds}}')='number' THEN payload#>'{{resourceWait,maxWaitSeconds}}' ELSE NULL END,
'phase',CASE WHEN payload#>>'{{result,visualProgress,phase}}' IN ('download','inventory','select','scan','finalize','held','complete') THEN payload#>'{{result,visualProgress,phase}}' ELSE NULL END,
'nextSelectionIndex',CASE WHEN jsonb_typeof(payload#>'{{result,visualProgress,nextSelectionIndex}}')='number' THEN payload#>'{{result,visualProgress,nextSelectionIndex}}' ELSE NULL END,
'completedSelectedFrames',CASE WHEN jsonb_typeof(payload#>'{{result,visualProgress,completedSelectedFrames}}')='number' THEN payload#>'{{result,visualProgress,completedSelectedFrames}}' ELSE NULL END,
'failureClass',CASE WHEN ({error})#>>'{{}}' IN ({codes}) THEN {error} WHEN {error} IS NULL THEN NULL ELSE '"unknown"'::jsonb END,
'sourceAttemptFailureClass',CASE WHEN ({attempt_error})#>>'{{}}' IN ({codes}) THEN {attempt_error} WHEN {attempt_error} IS NULL THEN NULL ELSE '"unknown"'::jsonb END) ORDER BY ordinal) FROM page),'[]'::jsonb))::text AS projection
FROM communityhero.workspaces w WHERE w.id=$1
"#)
}
fn safe_id(id:&str)->bool{!id.is_empty()&&id.len()<=128&&id.chars().all(|c|c.is_alphanumeric()||matches!(c,'-'|'_'|'.'|':'))}
fn timestamp(value:&Value)->Value{
    value.as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).map(|t|
        json!(t.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs,true))).unwrap_or(Value::Null)
}
impl Database {
    pub(crate) async fn read_post_media_status(&self,post_id:&str,account:&str,offset:u32)->ApiResult<Value>{
        if !safe_id(post_id){return Err(crate::bad("Invalid canonical post ID"));}
        let raw=match self {
            Self::Sqlite(pool)=>{
                // Only fixed source-owned SQL fragments are composed; every request value is bound.
                let statement=sqlite_query();
                sqlx::query_scalar::<_,String>(sqlx::AssertSqlSafe(statement.as_str())).bind(post_id).bind(i64::from(offset)).fetch_one(pool).await?
            },
            Self::Postgres{reader,..}=>{
                let statement=pg_query();
                let row=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(post_id).bind(i64::from(offset)).bind(account).fetch_one(reader).await?;
                if row.try_get::<bool,_>("execution_enabled")?{return Err(internal("PostgreSQL pilot execution must remain disabled"));}
                if !row.try_get::<bool,_>("identity_valid")?{return Err(internal("Workspace identity mismatch"));}
                row.try_get::<String,_>("projection")?
            }
        };
        let view=parse(&raw)?;
        if view["account"]!=account{return Err(internal("Workspace account mismatch"));}
        crate::active_binding(&view)?;
        if view["postCount"]!=1{return Err(crate::ApiError(crate::StatusCode::NOT_FOUND,"Canonical post missing or duplicated".into()));}
        if view["foreignCount"]!=0{return Err(crate::conflict("Media source binding differs from workspace"));}
        if view["catalogInvalid"]!=0{return Err(crate::conflict("Media knowledge catalog integrity mismatch"));}
        if !view["postCatalogInvalid"].is_null()&&view["postCatalogInvalid"]!=0{return Err(crate::conflict("Media post catalog identity mismatch"));}
        let input=&view["readinessInput"];
        let mut post_ids=std::collections::BTreeSet::new();
        for post in crate::list(input,"posts") {
            if !post["id"].as_str().is_some_and(|id|safe_id(id)&&post_ids.insert(id)) {
                return Err(crate::conflict("Media post catalog identity mismatch"));
            }
            check_binding(post,input)?;
            for attachment in post["attachments"].as_array().into_iter().flatten(){check_binding(attachment,input)?;}
        }
        for field in ["knowledge_entries","knowledge_versions"] {
            for record in crate::list(input,field){check_binding(record,input)?;}
        }
        let post=crate::row(input,"posts",post_id)?;
        let video=crate::knowledge::is_video_post(post);
        let visual_required=video&&crate::post_media_policy::visual_required(input,post)?;
        let preparation_policy=if video{crate::post_media_policy::effective_for_preparation(input,post)?}else{Value::Null};
        let lookup=crate::knowledge::TranscriptLookup::new(input,&crate::now()).map_err(crate::conflict)?;
        let readiness=lookup.readiness_for_policy(post,visual_required).map_err(crate::conflict)?;
        let preparation_readiness=lookup.readiness_for_policy(post,preparation_policy["visualRequired"]==true).map_err(crate::conflict)?;
        let total=view["total"].as_u64().ok_or_else(||internal("Invalid media job count"))?;
        if u64::from(offset)>total{return Err(crate::bad("Media job offset exceeds total"));}
        let mut jobs=view["jobs"].as_array().ok_or_else(||internal("Invalid media status projection"))?.clone();
        if jobs.len()>PAGE as usize{return Err(internal("Media status page exceeded limit"));}
        for job in &mut jobs {
            if !job["id"].as_str().is_some_and(safe_id){return Err(internal("Invalid media job identity"));}
            if !matches!(job["status"].as_str(),Some("queued"|"running"|"completed"|"failed"|"cancelled"|"held"|"paused"|"interrupted")){job["status"]=json!("unknown");}
            for field in ["createdAt","startedAt","finishedAt"]{job[field]=timestamp(&job[field]);}
            job["workerBlock"]=if job["workerBlockStage"].is_string(){
                let code=job["workerBlockCode"].as_str().filter(|code|crate::media_queue::worker_block_code(code))
                    .unwrap_or("media_runtime_not_ready");
                json!({"stage":job["workerBlockStage"],"code":code})
            }else{Value::Null};
            job["resourceWait"]=if job["resourceWaitResource"]=="gpu"&&job["resourceWaitOperation"].is_string()
                &&((job["resourceWaitState"]=="waiting"&&job["resourceWaitReason"]=="gpu_busy")
                    ||(job["resourceWaitState"]=="exhausted"&&job["resourceWaitReason"]=="gpu_gate_resource_wait_exhausted")) {
                json!({"resource":"gpu","state":job["resourceWaitState"],"active":job["status"]=="running"&&job["resourceWaitState"]=="waiting","reason":job["resourceWaitReason"],"operation":job["resourceWaitOperation"],
                    "startedAtUtc":timestamp(&job["resourceWaitStartedAt"]),"maxWaitSeconds":job["resourceWaitMaxSeconds"].as_u64().filter(|n|*n<=86400)})
            }else{Value::Null};
            for field in ["workerBlockStage","workerBlockCode","resourceWaitResource","resourceWaitState","resourceWaitReason","resourceWaitOperation","resourceWaitStartedAt","resourceWaitMaxSeconds"] {
                job.as_object_mut().unwrap().remove(field);
            }
            let checkpoint=json!({"phase":job["phase"],"nextSelectionIndex":job["nextSelectionIndex"].as_u64(),"completedSelectedFrames":job["completedSelectedFrames"].as_u64()});
            for field in ["phase","nextSelectionIndex","completedSelectedFrames"]{job.as_object_mut().unwrap().remove(field);}
            job["checkpoint"]=checkpoint;
        }
        let next=u64::from(offset)+jobs.len() as u64;
        Ok(json!({"schemaVersion":1,"account":account,"post":{"id":post_id},"jobs":jobs,"readiness":readiness,
            "preparationPolicy":preparation_policy,"preparationReadiness":preparation_readiness,
            "pagination":{"offset":offset,"limit":PAGE,"total":total,"nextOffset":if next<total {Some(next)}else{None}},"readOnly":true}))
    }
}
fn check_binding(record:&Value,input:&Value)->ApiResult<()> {
    if !crate::knowledge::in_account(record,input["account"].as_str().unwrap_or(""))
        || (!record["connectorBinding"].is_null()&&record["connectorBinding"]!=input["connectorBinding"]) {
        return Err(crate::conflict("Media source binding differs from workspace"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_failure_codes_are_closed_unique_and_shared_by_sql_engines(){
        let mut seen=std::collections::BTreeSet::new();
        for code in FAILURE_CODES {
            assert!(!code.is_empty()&&code.bytes().all(|b|b.is_ascii_lowercase()||b==b'_'));
            assert!(seen.insert(*code),"Duplicate failure code {code}");
        }
        let clause=format!("IN ({})",failure_codes_sql());
        assert_eq!(sqlite_query().matches(clause.as_str()).count(),2);
        assert_eq!(pg_query().matches(clause.as_str()).count(),2);
    }
    async fn fixture()->(Database,tempfile::TempDir,Value){
        let folder=tempfile::tempdir().unwrap();let pool=crate::open_db(&folder.path().join("status.sqlite")).await.unwrap();
        let mut d=crate::empty();d["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();
        d["posts"]=json!([{"id":"p","title":"SECRET","sourceUrl":"https://private/?token=SECRET"},{"id":"other","title":"SECRET"}]);
        let mut jobs=Vec::new();for n in 0..105 {jobs.push(json!({"id":format!("job-{n}"),"kind":"media","refId":"p","status":"failed","createdAt":"2026-09-27T12:00:00Z",
            "error":"SECRET https://private C:/private","result":{"visualProgress":{"phase":"held","nextSelectionIndex":n,"completedSelectedFrames":n,"source":{"path":"SECRET"}},"transcript":"SECRET"}}));}
        jobs.push(json!({"id":"audio","kind":"media_audio","refId":"other","status":"completed","sourceAttempts":[{"postId":"p","error":"source_download_failed_network"},{"postId":"other","error":"SECRET"}]}));
        jobs.push(json!({"id":"progress","kind":"media","refId":"other","status":"running","result":{"visualProgress":{"sourcePostId":"p","phase":"scan"}}}));
        jobs.push(json!({"id":"unrelated","kind":"media","refId":"other","title":"SECRET","status":"failed"}));
        jobs[0]["status"]=json!("interrupted");
        jobs[0]["workerBlock"]=json!({"stage":"scan","code":"SECRET https://private C:/private"});
        jobs[1]["workerBlock"]=json!({"stage":"download","code":"media_runtime_not_ready"});
        jobs[2]["resourceWait"]=json!({"resource":"gpu","reason":"gpu_busy","state":"waiting","operation":"media_vision_chunk","startedAtUtc":"2026-09-27T12:00:00Z","maxWaitSeconds":7200});
        jobs[3]["resourceWait"]=json!({"resource":"gpu","reason":"gpu_gate_resource_wait_exhausted","state":"exhausted","operation":"whisper_asr","startedAtUtc":"2026-09-27T12:00:00Z","maxWaitSeconds":7200});
        jobs[4]["resourceWait"]=json!({"resource":"gpu","reason":"SECRET","state":"waiting","operation":"media_vision","startedAtUtc":"SECRET","maxWaitSeconds":7200});
        assert!(FAILURE_CODES.len()<100);
        // Exercise each exact code through the actual SQL projection, cycling
        // job, checkpoint and source-attempt origins. Keep the first unknown.
        for (index,code) in FAILURE_CODES.iter().enumerate(){
            let job=&mut jobs[index+1];job.as_object_mut().unwrap().remove("error");
            match index%3 {
                0=>job["error"]=json!(code),
                1=>job["result"]["visualProgress"]["error"]=json!(code),
                _=>job["sourceAttempts"]=json!([
                    {"postId":"p","error":code},{"postId":"other","error":"SECRET foreign-source diagnostic"}]),
            }
        }
        jobs[101]["error"]=json!("audio_coverage_incomplete SECRET https://private/?token=SECRET");
        jobs[102]["error"]=json!({"code":"media_source_identity_mismatch","detail":"SECRET"});
        jobs[103]["error"]=json!(false);jobs[104]["error"]=Value::Null;
        d["jobs"]=json!(jobs);crate::storage::normalize(&mut d);
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(&pool).await.unwrap();
        (Database::Sqlite(pool),folder,d)
    }
    async fn store_sqlite(db:&Database,d:&Value){
        if let Database::Sqlite(pool)=db {
            sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(pool).await.unwrap();
        }
    }
    fn readiness_source()->Value {
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
        d["posts"]=json!([
            {"id":"p","postKey":"provider:p","title":"Video","body":"Reuse title #tag","text":"","sourceUrl":"https://vk.com/video-1_1","attachments":[{"type":"video"}]},
            {"id":"source","postKey":"provider:source","title":"reuse title","sourceUrl":"https://www.youtube.com/watch?v=AbCdEf123_-","attachments":[{"type":"video"}]},
            {"id":"conflict","postKey":"provider:conflict","title":"unrelated title","canonicalMediaId":"different","attachments":[{"type":"video"}]}]);
        d["materials"]=json!([
            {"id":"target-transcript","kind":"transcript","account":"LikeAvto","postKey":"provider:p","text":"SECRET full transcript","sourceUrl":d["posts"][0]["sourceUrl"],
                "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":crate::media_fullframes::source_version(&d["posts"][0],"LikeAvto"),"mediaDurationSeconds":200.0,"audioDurationSeconds":200.0}},
            // Title reuse must start from proven full audio for the exact donor,
            // not a legacy partial=false label with unknown coverage.
            {"id":"source-transcript","kind":"transcript","account":"LikeAvto","postKey":"provider:source","text":"SECRET reused transcript","sourceUrl":d["posts"][1]["sourceUrl"],
                "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":crate::media_fullframes::source_version(&d["posts"][1],"LikeAvto"),"mediaDurationSeconds":200.0,"audioDurationSeconds":200.0}}
        ]);
        d["jobs"]=json!([{"id":"done","kind":"media_audio","refId":"p","status":"completed","result":{"transcript":"SECRET"},"sourceAttempts":[{"postId":"p","error":"source_download_failed_network"}]}]);
        d["settings"]["unrelatedPrivateSetting"]=json!("SECRET");
        d["branches"]=json!([{"id":"private-dialogue","text":"SECRET"}]);
        crate::knowledge::sync_catalog(&mut d,"2026-09-27T00:00:00Z").unwrap();
        crate::storage::normalize(&mut d);d
    }
    fn audio_only(d:&mut Value) {
        d["settings"]["postMediaPolicies"]["p"]=json!({"version":1,"revision":1,"status":"active","postId":"p","account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":crate::media_fullframes::source_version(&d["posts"][0],"LikeAvto"),"mode":"full_audio_only"});
    }
    fn add_non_media_reference(d:&mut Value) {
        d["materials"].as_array_mut().unwrap().push(json!({"id":"private-rule","kind":"reference","account":"LikeAvto","text":"SECRET unrelated reference"}));
        crate::knowledge::sync_catalog(d,"2026-09-27T00:00:00Z").unwrap();
    }
    fn expected_readiness(d:&Value)->Value {
        let post=&d["posts"][0];let visual=crate::post_media_policy::visual_required(d,post).unwrap();
        crate::knowledge::TranscriptLookup::new(d,&crate::now()).unwrap().readiness_for_policy(post,visual).unwrap()
    }
    fn expected_preparation_readiness(d:&Value)->Value {
        let post=&d["posts"][0];let policy=crate::post_media_policy::effective_for_preparation(d,post).unwrap();
        crate::knowledge::TranscriptLookup::new(d,&crate::now()).unwrap()
            .readiness_for_policy(post,policy["visualRequired"]==true).unwrap()
    }
    #[tokio::test]
    async fn media_status_sqlite_readiness_uses_current_complete_semantic_corpus(){
        let(db,_folder,_)=fixture().await;let mut d=readiness_source();
        // Same-title peer is after many unrelated records: dependency corpus is
        // deliberately not paginated with jobs or truncated at a bootstrap cap.
        for n in 0..150 {d["posts"].as_array_mut().unwrap().push(json!({"id":format!("peer-{n}"),"postKey":format!("provider:peer-{n}"),"title":"unrelated","attachments":[{"type":"video"}]}));}
        store_sqlite(&db,&d).await;
        let view=db.read_post_media_status("p","LikeAvto",0).await.unwrap();
        assert_eq!(view["readiness"],expected_readiness(&d));
        assert_eq!(view["readiness"]["audioReady"],true);
        // Default text acquisition has no visual prerequisite. visualReady
        // means that prerequisite is satisfied, not that pixels were observed.
        assert_eq!(view["readiness"]["visualRequired"],false);
        assert_eq!(view["readiness"]["visualReady"],true);assert_eq!(view["readiness"]["ready"],true);
        assert_eq!(view["readiness"]["currentHeadCounts"]["visual"],0);
        assert_eq!(view["preparationPolicy"]["mode"],"full_audio_only");
        assert_eq!(view["preparationReadiness"],expected_preparation_readiness(&d));
        assert_eq!(view["preparationReadiness"]["ready"],true);
        assert_eq!(view["jobs"][0]["status"],"completed");assert!(view["jobs"][0]["failureClass"].is_null());assert_eq!(view["jobs"][0]["sourceAttemptFailureClass"],"source_download_failed_network");
        let statement=sqlite_query();let raw=if let Database::Sqlite(pool)=&db{sqlx::query_scalar::<_,String>(sqlx::AssertSqlSafe(statement.as_str())).bind("p").bind(0i64).fetch_one(pool).await.unwrap()}else{unreachable!()};
        let projected:Value=serde_json::from_str(&raw).unwrap();let input=&projected["readinessInput"];
        assert_eq!(input["posts"],d["posts"]);assert_eq!(input["knowledge_versions"],d["knowledge_versions"]);
        assert!(input.get("branches").is_none());assert!(input["settings"].get("unrelatedPrivateSetting").is_none());
        assert!(!view.to_string().contains("SECRET")&&!view.to_string().contains("https:")&&!view.to_string().contains("visualEvidence")&&!view.to_string().contains("sourceUrl"));
        audio_only(&mut d);store_sqlite(&db,&d).await;
        let ready=db.read_post_media_status("p","LikeAvto",0).await.unwrap();assert_eq!(ready["readiness"],expected_readiness(&d));assert_eq!(ready["readiness"]["ready"],true);
        assert_eq!(ready["preparationReadiness"],expected_preparation_readiness(&d));
        d["settings"]["postMediaPolicies"]["p"]["mode"]=json!("full_audio_visual");store_sqlite(&db,&d).await;
        let visual=db.read_post_media_status("p","LikeAvto",0).await.unwrap();
        assert_eq!(visual["preparationPolicy"]["mode"],"full_audio_visual");
        assert_eq!(visual["readiness"]["visualRequired"],true);
        assert_eq!(visual["readiness"]["visualReady"],false);
        assert_eq!(visual["preparationReadiness"]["ready"],false);
        audio_only(&mut d);store_sqlite(&db,&d).await;
        // Current source revision drift must invalidate the explicit policy and
        // strict full-audio proof even though the durable job remains completed.
        d["posts"][0]["text"]=json!("changed source");audio_only(&mut d);store_sqlite(&db,&d).await;
        let stale=db.read_post_media_status("p","LikeAvto",0).await.unwrap();assert_eq!(stale["readiness"],expected_readiness(&d));assert_eq!(stale["readiness"]["audioReady"],false);assert_eq!(stale["readiness"]["ready"],false);
        assert_eq!(stale["preparationReadiness"],expected_preparation_readiness(&d));
        assert_eq!(stale["preparationReadiness"]["ready"],false);
        assert_eq!(db.read().await.unwrap(),d,"diagnostics must remain read-only");
        // A title-only donor remains missing input. Exact source reuse is still
        // available, and diagnostics retain the complete peer corpus.
        let mut shared=readiness_source();
        for field in ["materials","knowledge_entries","knowledge_versions"] {
            shared[field].as_array_mut().unwrap().retain(|v|v["id"]!="target-transcript"&&v["sourceMaterialId"]!="target-transcript");
        }
        store_sqlite(&db,&shared).await;
        let missing=db.read_post_media_status("p","LikeAvto",0).await.unwrap();assert_eq!(missing["readiness"],expected_readiness(&shared));assert_eq!(missing["readiness"]["audioReady"],false);
        shared["posts"][0]["sourceUrl"]=shared["posts"][1]["sourceUrl"].clone();
        store_sqlite(&db,&shared).await;
        let reused=db.read_post_media_status("p","LikeAvto",0).await.unwrap();assert_eq!(reused["readiness"],expected_readiness(&shared));assert_eq!(reused["readiness"]["audioReady"],true);
        for n in 0..150 {shared["posts"].as_array_mut().unwrap().push(json!({"id":format!("unrelated-{n}"),"postKey":format!("unrelated:{n}"),"title":"unrelated"}));}
        shared["posts"][2]["title"]=json!("Reuse title");
        shared["posts"].as_array_mut().unwrap().push(json!({"id":"conflicting-copy","postKey":"provider:conflicting-copy","title":"reuse title","canonicalMediaId":"another-copy","attachments":[{"type":"video"}]}));
        shared["posts"][0]["sourceUrl"]=json!("https://vk.com/video-1_1");
        store_sqlite(&db,&shared).await;
        let conflict=db.read_post_media_status("p","LikeAvto",0).await.unwrap();assert_eq!(conflict["readiness"],expected_readiness(&shared));assert_eq!(conflict["readiness"]["audioReady"],false);
    }
    #[tokio::test]
    async fn media_status_sqlite_rejects_missing_duplicate_tampered_and_foreign_current_heads(){
        let(db,_folder,_)=fixture().await;let base=readiness_source();
        for fault in ["missing","duplicate","tamper","foreign-version","foreign-post","foreign-attachment","head-metadata"] {
            let mut d=base.clone();
            match fault {
                "missing"=>d["knowledge_entries"][0]["currentVersionId"]=json!("missing"),
                "duplicate"=>{let entry=d["knowledge_entries"][0].clone();d["knowledge_entries"].as_array_mut().unwrap().push(entry);},
                "tamper"=>d["knowledge_versions"][0]["text"]=json!("SECRET changed hash"),
                "foreign-version"=>d["knowledge_versions"][0]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding(),
                "foreign-post"=>d["posts"][2]["account"]=json!("BAW Russia"),
                "foreign-attachment"=>d["posts"][2]["attachments"][0]["account"]=json!("BAW Russia"),
                _=>d["knowledge_entries"][0]["sourceMaterialId"]=json!("wrong-source"),
            }
            store_sqlite(&db,&d).await;
            assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err(),"{fault} must fail closed");
        }
    }
    #[tokio::test]
    async fn media_status_sqlite_checks_non_media_current_scope_and_hash(){
        let(db,_folder,_)=fixture().await;let mut d=readiness_source();
        add_non_media_reference(&mut d);
        let index=d["knowledge_entries"].as_array().unwrap().iter().position(|e|e["sourceMaterialId"]=="private-rule").unwrap();
        assert_eq!(d["knowledge_entries"][index]["kind"],"reference");
        let raw=d.to_string();
        let scope=d["knowledge_entries"][index]["scope"].to_string();
        let reordered=format!("{{\"postKeys\":{},\"account\":\"LikeAvto\"}}",d["knowledge_entries"][index]["scope"]["postKeys"]);
        assert_ne!(scope,reordered,"fixture must actually change raw object order");
        assert!(raw.matches(&scope).count()>=2,"fixture must contain both entry and version scopes");
        let reordered_raw=raw.replacen(&scope,&reordered,1);
        assert!(reordered_raw.contains(&scope)&&reordered_raw.contains(&reordered),"only one scope object must change raw order");
        assert_eq!(serde_json::from_str::<Value>(&reordered_raw).unwrap(),d,"object order must not change domain values");
        if let Database::Sqlite(pool)=&db {sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(reordered_raw).execute(pool).await.unwrap();}
        let valid=db.read_post_media_status("p","LikeAvto",0).await.unwrap();assert_eq!(valid["readiness"],expected_readiness(&d));
        assert!(!valid.to_string().contains("private-rule")&&!valid.to_string().contains("SECRET"));
        for wrong_scope in [json!({"account":"BAW Russia","postKeys":[]}),json!({"account":"LikeAvto","postKeys":["wrong"]}),Value::Null] {
            let mut bad_scope=d.clone();bad_scope["knowledge_entries"][index]["scope"]=wrong_scope;
            store_sqlite(&db,&bad_scope).await;
            assert!(crate::knowledge::validate_catalog(&bad_scope).is_err());
            assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err(),"non-media scope mismatch must fail closed");
        }
        let mut bad_hash=d.clone();
        let version=bad_hash["knowledge_versions"].as_array_mut().unwrap().iter_mut().find(|v|v["sourceMaterialId"]=="private-rule").unwrap();
        version["text"]=json!("SECRET current reference modified without rehash");
        store_sqlite(&db,&bad_hash).await;
        assert!(crate::knowledge::validate_catalog(&bad_hash).is_err());
        assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err(),"current non-media hash mismatch must fail closed");
    }
    #[tokio::test]
    async fn media_status_sqlite_exact_source_pagination_is_safe_and_read_only(){
        let(db,_folder,before)=fixture().await;
        let a=db.read_post_media_status("p","LikeAvto",0).await.unwrap();
        assert_eq!(a["jobs"].as_array().unwrap().len(),100);assert_eq!(a["pagination"]["total"],107);assert_eq!(a["pagination"]["nextOffset"],100);
        assert_eq!(a["jobs"][0]["failureClass"],"unknown");assert_eq!(a["jobs"][99]["checkpoint"]["completedSelectedFrames"],99);
        assert_eq!(a["jobs"][0]["status"],"interrupted");
        assert_eq!(a["jobs"][0]["workerBlock"],json!({"stage":"scan","code":"media_runtime_not_ready"}));
        assert_eq!(a["jobs"][1]["workerBlock"],json!({"stage":"download","code":"media_runtime_not_ready"}));
        assert_eq!(a["jobs"][2]["resourceWait"]["reason"],"gpu_busy");assert_eq!(a["jobs"][2]["resourceWait"]["maxWaitSeconds"],7200);
        assert_eq!(a["jobs"][3]["resourceWait"]["state"],"exhausted");assert!(a["jobs"][4]["resourceWait"].is_null());
        for (index,code) in FAILURE_CODES.iter().enumerate(){
            let field=if index%3==2{"sourceAttemptFailureClass"}else{"failureClass"};
            assert_eq!(a["jobs"][index+1][field],*code);
            if index%3==2{assert!(a["jobs"][index+1]["failureClass"].is_null());}
        }
        for code in ["whisper_failed","audio_coverage_incomplete","media_source_identity_mismatch","media_source_artifact_unavailable",
            "media_cached_audio_source_unavailable","source_download_failed_spawn_failed","source_download_failed_wait_failed"] {
            assert!(a["jobs"].as_array().unwrap().iter().any(|job|job["failureClass"]==code||job["sourceAttemptFailureClass"]==code),"Missing emitted category {code}");
        }
        let b=db.read_post_media_status("p","LikeAvto",100).await.unwrap();assert_eq!(b["jobs"].as_array().unwrap().len(),7);assert!(b["pagination"]["nextOffset"].is_null());
        assert!(b["jobs"][5]["failureClass"].is_null());
        assert_eq!(b["jobs"][5]["sourceAttemptFailureClass"],"source_download_failed_network");
        for index in [1,2,3]{assert_eq!(b["jobs"][index]["failureClass"],"unknown");}
        assert!(b["jobs"][4]["failureClass"].is_null());
        let all=format!("{a}{b}");assert!(!all.contains("SECRET")&&!all.contains("https:")&&!all.contains("C:/private")&&!all.contains("\"transcript\":")&&!all.contains("unrelated"));
        assert_eq!(db.read().await.unwrap(),before);
        assert!(db.read_post_media_status("p","LikeAvto",108).await.is_err());assert!(db.read_post_media_status("absent","LikeAvto",0).await.is_err());
        assert!(db.read_post_media_status("p","BAW Russia",0).await.is_err());
    }
    #[tokio::test]
    async fn media_status_rejects_foreign_job_even_outside_page_and_foreign_post(){
        let(db,_folder,mut d)=fixture().await;
        d["jobs"][104]["account"]=json!("BAW Russia");
        if let Database::Sqlite(pool)=&db {sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(pool).await.unwrap();}
        assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err());
        d["jobs"][104].as_object_mut().unwrap().remove("account");d["posts"][0]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding();
        if let Database::Sqlite(pool)=&db {sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(pool).await.unwrap();}
        assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err());
    }
    #[tokio::test]
    #[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
    async fn postgres_media_status_exact_source_safe_projection_parity(){
        let db=crate::storage::preparation::writer_v51_fixture_db().await;
        let(sqlite,_folder,mut source)=fixture().await;
        let mut ready_source=readiness_source();add_non_media_reference(&mut ready_source);
        let other=source["posts"][1].clone();
        for field in ["posts","materials","knowledge_entries","knowledge_versions","settings"] {source[field]=ready_source[field].clone();}
        source["posts"].as_array_mut().unwrap().push(other);
        // Normal queued media jobs may have absent or JSON-null progress. They
        // cannot create a duration waiver and must not break either SQL engine.
        for (id,progress) in [("duration-absent",None),("duration-null",Some(Value::Null))] {
            let mut job=json!({"id":id,"kind":"media","purpose":"auto_media","refId":"p","status":"queued","account":"LikeAvto","connectorBinding":source["connectorBinding"],"visualContractVersion":2});
            if let Some(progress)=progress{job["result"]=json!({"visualProgress":progress});}
            source["jobs"].as_array_mut().unwrap().push(job);
        }
        crate::storage::normalize(&mut source);
        store_sqlite(&sqlite,&source).await;
        db.change(|state|{*state=source.clone();Ok(())}).await.unwrap();
        let before=db.read().await.unwrap();
        for offset in [0,100] {
            let actual=db.read_post_media_status("p","LikeAvto",offset).await.unwrap();
            assert_eq!(actual,sqlite.read_post_media_status("p","LikeAvto",offset).await.unwrap());
        }
        assert_eq!(db.read().await.unwrap(),before,"read-only projection must not alter persisted domain state");
        for scenario in ["audio-only","changed-source","restored","title-conflict"] {
            match scenario {
                "audio-only"=>audio_only(&mut source),
                "changed-source"=>{source["posts"][0]["text"]=json!("changed source");audio_only(&mut source);},
                "restored"=>{source["posts"][0]=ready_source["posts"][0].clone();source["settings"]=ready_source["settings"].clone();},
                _=>source["posts"][2]["title"]=json!("Reuse title"),
            }
            store_sqlite(&sqlite,&source).await;
            db.change(|state|{*state=source.clone();Ok(())}).await.unwrap();
            for offset in [0,100] {
                let actual=db.read_post_media_status("p","LikeAvto",offset).await.unwrap();
                assert_eq!(actual,sqlite.read_post_media_status("p","LikeAvto",offset).await.unwrap(),"{scenario}");
                assert_eq!(actual["readiness"],expected_readiness(&source),"{scenario}");
            }
            assert_eq!(db.read().await.unwrap(),source,"{scenario} changed persisted state");
        }
        assert!(db.read_post_media_status("absent","LikeAvto",0).await.is_err());
        assert!(db.read_post_media_status("p","BAW Russia",0).await.is_err());
        db.change(|state|{state["jobs"][104]["account"]=json!("BAW Russia");Ok(())}).await.unwrap();
        assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err(),"foreign matching job outside page must reject");
        db.change(|state|{state["jobs"][104].as_object_mut().unwrap().remove("account");Ok(())}).await.unwrap();
        assert_eq!(db.read_post_media_status("p","LikeAvto",0).await.unwrap()["readiness"],expected_readiness(&source));
        // Fixture-only SQL can mutate a head or append synthetic history. It
        // must never disable the trigger protecting existing version payloads.
        if let Database::Postgres{writer,..}=&db {
            let entry=source["knowledge_entries"].as_array().unwrap().iter().find(|e|e["sourceMaterialId"]=="private-rule").unwrap();
            let version=source["knowledge_versions"].as_array().unwrap().iter().find(|v|v["id"]==entry["currentVersionId"]).unwrap();
            let mut bad_scope=entry.clone();bad_scope["scope"]=json!({"account":"LikeAvto","postKeys":["wrong"]});
            sqlx::query("UPDATE communityhero.knowledge_entries SET payload=$1::jsonb WHERE workspace_id=$2 AND id=$3")
                .bind(bad_scope.to_string()).bind(WORKSPACE).bind(entry["id"].as_str().unwrap()).execute(writer).await.unwrap();
            assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err(),"non-media scope mismatch must fail closed in PostgreSQL");
            sqlx::query("UPDATE communityhero.knowledge_entries SET payload=$1::jsonb WHERE workspace_id=$2 AND id=$3")
                .bind(entry.to_string()).bind(WORKSPACE).bind(entry["id"].as_str().unwrap()).execute(writer).await.unwrap();
            let mut bad_hash=version.clone();bad_hash["text"]=json!("SECRET current reference modified without rehash");
            let rewrite=sqlx::query("UPDATE communityhero.knowledge_versions SET payload=$1::jsonb WHERE workspace_id=$2 AND id=$3")
                .bind(bad_hash.to_string()).bind(WORKSPACE).bind(version["id"].as_str().unwrap()).execute(writer).await;
            assert!(rewrite.as_ref().err().is_some_and(|error|error.as_database_error().is_some_and(|error|error.message()=="Immutable history cannot be changed or deleted")),"immutable-history trigger must reject the attempted rewrite");
            assert_eq!(db.read_post_media_status("p","LikeAvto",0).await.unwrap()["readiness"],expected_readiness(&source),"rejected rewrite must leave original current head usable");
            let original:Value=serde_json::from_str(&sqlx::query_scalar::<_,String>("SELECT payload::text FROM communityhero.knowledge_versions WHERE workspace_id=$1 AND id=$2")
                .bind(WORKSPACE).bind(version["id"].as_str().unwrap()).fetch_one(writer).await.unwrap()).unwrap();
            assert_eq!(original,*version,"existing immutable payload must remain exact");
            bad_hash["id"]=json!("diagnostic-invalid-current-version");
            let invalid_id=bad_hash["id"].as_str().unwrap();
            sqlx::query("INSERT INTO communityhero.knowledge_versions(workspace_id,id,ordinal,payload,entry_id,source_material_id) SELECT $1,$2,COALESCE(MAX(ordinal),-1)+1,$3::jsonb,$4,$5 FROM communityhero.knowledge_versions WHERE workspace_id=$1")
                .bind(WORKSPACE).bind(invalid_id).bind(bad_hash.to_string()).bind(entry["id"].as_str().unwrap()).bind(entry["sourceMaterialId"].as_str().unwrap()).execute(writer).await.unwrap();
            let mut bad_head=entry.clone();bad_head["currentVersionId"]=json!(invalid_id);
            sqlx::query("UPDATE communityhero.knowledge_entries SET payload=$1::jsonb,current_version_id=$2 WHERE workspace_id=$3 AND id=$4")
                .bind(bad_head.to_string()).bind(invalid_id).bind(WORKSPACE).bind(entry["id"].as_str().unwrap()).execute(writer).await.unwrap();
            assert!(db.read_post_media_status("p","LikeAvto",0).await.is_err(),"non-media hash mismatch must fail closed in PostgreSQL");
            sqlx::query("UPDATE communityhero.knowledge_entries SET payload=$1::jsonb,current_version_id=$2 WHERE workspace_id=$3 AND id=$4")
                .bind(entry.to_string()).bind(version["id"].as_str().unwrap()).bind(WORKSPACE).bind(entry["id"].as_str().unwrap()).execute(writer).await.unwrap();
            assert_eq!(db.read_post_media_status("p","LikeAvto",0).await.unwrap()["readiness"],expected_readiness(&source),"restored current head must be usable with malformed history excluded");
            let retained:Value=serde_json::from_str(&sqlx::query_scalar::<_,String>("SELECT payload::text FROM communityhero.knowledge_versions WHERE workspace_id=$1 AND id=$2")
                .bind(WORKSPACE).bind(invalid_id).fetch_one(writer).await.unwrap()).unwrap();
            assert_eq!(retained,bad_hash,"synthetic malformed history must remain immutable and retained");
        }
    }
}
