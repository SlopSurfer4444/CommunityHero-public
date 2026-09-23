//! Current rule heads only. This projection is never a replacement workspace
//! and grants no approval/dispatch authority. Existing histories are untouched.
use super::*;
use serde_json::json;

const MAX_RULES: usize = 256;
const MAX_BYTES: i64 = 2 * 1024 * 1024;
const MAX_POST_KEYS: usize = 100;

// Join through the canonical current pointer and the versions primary key. No
// historical-version aggregation or full workspace JSON crosses this boundary.
// Pending/untrusted/expired heads stay available for the domain exclusion rules.
const PG_CANDIDATES: &str = r#"
SELECT e.id, e.current_version_id,
       octet_length(e.payload::text)::bigint + COALESCE(octet_length(v.payload::text),0) AS bytes
FROM communityhero.knowledge_entries e
LEFT JOIN communityhero.knowledge_versions v
  ON v.workspace_id=e.workspace_id AND v.id=e.current_version_id
WHERE e.workspace_id=$1
  AND (e.payload->>'kind' IN ('rule','policy') OR v.payload->>'kind' IN ('rule','policy'))
  AND (e.payload#>>'{scope,account}'=$4 OR v.payload#>>'{scope,account}'=$4)
  AND ($2 OR v.id IS NULL
    OR e.payload->'scope' IS DISTINCT FROM v.payload->'scope'
    OR e.payload->'status' IS DISTINCT FROM v.payload->'status'
    OR e.payload->'kind' IS DISTINCT FROM v.payload->'kind'
    OR e.payload->'sourceMaterialId' IS DISTINCT FROM v.payload->'sourceMaterialId'
    OR v.payload->>'entryId' IS DISTINCT FROM e.id OR
    CASE WHEN jsonb_typeof(v.payload#>'{companyImport,scope,postAliases}')='array'
               AND jsonb_array_length(v.payload#>'{companyImport,scope,postAliases}')>0
      THEN EXISTS (SELECT 1 FROM jsonb_array_elements(v.payload#>'{companyImport,scope,postAliases}') a
                   WHERE a->>'value'=ANY($3::text[]))
      WHEN jsonb_typeof(v.payload#>'{scope,postKeys}') IS DISTINCT FROM 'array' THEN true
      ELSE jsonb_array_length(v.payload#>'{scope,postKeys}')=0
        OR (v.payload#>'{scope,postKeys}') ?| $3::text[] END)
ORDER BY e.ordinal LIMIT $5
"#;
const SQLITE_CANDIDATES: &str = r#"
SELECT json_extract(e.value,'$.id') AS id,
       json_extract(e.value,'$.currentVersionId') AS current_version_id,
       length(CAST(e.value AS BLOB))+COALESCE(length(CAST(v.value AS BLOB)),0) AS bytes
FROM workspace w, json_each(w.payload,'$.knowledge_entries') e
LEFT JOIN json_each(w.payload,'$.knowledge_versions') v
  ON json_extract(v.value,'$.id')=json_extract(e.value,'$.currentVersionId')
WHERE w.id=1
  AND (json_extract(e.value,'$.kind') IN ('rule','policy') OR json_extract(v.value,'$.kind') IN ('rule','policy'))
  AND (json_extract(e.value,'$.scope.account')=?4 OR json_extract(v.value,'$.scope.account')=?4)
  AND (?2 OR v.value IS NULL
    OR json_extract(e.value,'$.scope') IS NOT json_extract(v.value,'$.scope')
    OR json_extract(e.value,'$.status') IS NOT json_extract(v.value,'$.status')
    OR json_extract(e.value,'$.kind') IS NOT json_extract(v.value,'$.kind')
    OR json_extract(e.value,'$.sourceMaterialId') IS NOT json_extract(v.value,'$.sourceMaterialId')
    OR json_extract(v.value,'$.entryId') IS NOT json_extract(e.value,'$.id') OR CASE
    WHEN json_type(v.value,'$.companyImport.scope.postAliases')='array'
         AND json_array_length(v.value,'$.companyImport.scope.postAliases')>0
      THEN EXISTS(SELECT 1 FROM json_each(v.value,'$.companyImport.scope.postAliases') a
                  JOIN json_each(?3) k ON json_extract(a.value,'$.value')=k.value)
    WHEN COALESCE(json_type(v.value,'$.scope.postKeys'),'')!='array' THEN 1
    ELSE json_array_length(v.value,'$.scope.postKeys')=0
      OR EXISTS(SELECT 1 FROM json_each(v.value,'$.scope.postKeys') p JOIN json_each(?3) k ON p.value=k.value)
    END)
ORDER BY CAST(e.key AS INTEGER) LIMIT ?5
"#;

fn keys(post_keys: Option<&[String]>) -> ApiResult<Vec<String>> {
    let keys = post_keys.unwrap_or(&[]);
    if keys.len()>MAX_POST_KEYS || keys.iter().any(|k|k.is_empty() || k.len()>512 || k.chars().any(char::is_control)) {
        return Err(internal("Invalid bounded instruction post scope"));
    }
    Ok(keys.to_vec())
}
fn root(account: Option<String>, metadata_account: Option<String>, binding: Option<String>) -> ApiResult<Value> {
    if account.as_deref()!=Some("LikeAvto") || metadata_account!=account {
        return Err(internal("Instruction workspace identity mismatch"));
    }
    let mut value=json!({"account":"LikeAvto","posts":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
    if let Some(binding)=binding {value["connectorBinding"]=parse(&binding)?;}
    crate::active_binding(&value)?;
    Ok(value)
}
fn check_candidates(candidates: &[(String,String,i64)]) -> ApiResult<()> {
    let mut entries=HashSet::new();
    if candidates.len()>MAX_RULES || candidates.iter().any(|(id,_,_)|!entries.insert(id)) {
        return Err(internal("Instruction head scope exceeds bounded record limit or has duplicate identities"));
    }
    let bytes=candidates.iter().try_fold(0i64,|sum,(_,_,bytes)|if *bytes<0 {None}else{sum.checked_add(*bytes)});
    if bytes.is_none_or(|bytes|bytes>MAX_BYTES) {return Err(internal("Instruction head scope exceeds bounded byte limit"));}
    Ok(())
}
fn append(value:&mut Value, entry:&str, version:Option<&str>, id:&str, version_id:&str) -> ApiResult<()> {
    let entry=parse(entry)?;
    let version=parse(version.ok_or_else(||internal("Missing current instruction version"))?)?;
    if entry["id"]!=id || entry["currentVersionId"]!=version_id || version["id"]!=version_id
        || version["entryId"]!=id || !matches!(version["kind"].as_str(),Some("rule"|"policy")) {
        return Err(internal("Instruction head identity mismatch"));
    }
    value["knowledge_entries"].as_array_mut().unwrap().push(entry);
    value["knowledge_versions"].as_array_mut().unwrap().push(version);
    Ok(())
}

impl Database {
    /// Shared UI catalog: current rule/policy versions for this bound account,
    /// across all post scopes. No fact/media/history/feedback payloads returned.
    pub(crate) async fn read_instruction_catalog(&self) -> ApiResult<Value> {
        let source=self.read_rule_source(None).await?;
        Ok(json!({"entries":source["knowledge_entries"],"versions":source["knowledge_versions"]}))
    }

    /// A trusted caller supplies exact post keys; input length and full rule
    /// count/bytes are bounded. Overflow fails, never truncates mandatory rules.
    pub(crate) async fn read_rule_selection(&self,post_keys:&[String],at:&str)->ApiResult<Value> {
        let source=self.read_rule_source(Some(post_keys)).await?;
        let items:Vec<Value>=post_keys.iter().map(|key|json!({"postKey":key})).collect();
        crate::knowledge::select(&source,&items,&[],at).map_err(internal)
    }

    async fn read_rule_source(&self,post_keys:Option<&[String]>)->ApiResult<Value> {
        let keys=keys(post_keys)?;
        let all=post_keys.is_none();
        let mut value=match self {
            Self::Postgres{reader,..}=>{
                let mut tx=reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let record=sqlx::query("SELECT account,metadata->>'account' AS metadata_account,(metadata->'connectorBinding')::text AS binding,execution_enabled FROM communityhero.workspaces WHERE id=$1")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                if record.try_get::<bool,_>("execution_enabled")? {return Err(internal("PostgreSQL pilot execution must remain disabled"));}
                let mut value=root(record.try_get("account")?,record.try_get("metadata_account")?,record.try_get("binding")?)?;
                let candidates:Vec<(String,String,i64)>=sqlx::query_as(PG_CANDIDATES)
                    .bind(WORKSPACE).bind(all).bind(&keys).bind("LikeAvto").bind((MAX_RULES+1) as i64)
                    .fetch_all(&mut *tx).await?;
                check_candidates(&candidates)?;
                let ids:Vec<&str>=candidates.iter().map(|(id,_,_)|id.as_str()).collect();
                let records=sqlx::query("SELECT e.id,e.current_version_id,e.payload::text AS entry,v.payload::text AS version FROM communityhero.knowledge_entries e LEFT JOIN communityhero.knowledge_versions v ON v.workspace_id=e.workspace_id AND v.id=e.current_version_id WHERE e.workspace_id=$1 AND e.id=ANY($2::text[]) ORDER BY e.ordinal")
                    .bind(WORKSPACE).bind(ids).fetch_all(&mut *tx).await?;
                if records.len()!=candidates.len(){return Err(internal("Instruction head set changed"));}
                for record in records {append(&mut value,record.try_get("entry")?,record.try_get("version")?,record.try_get("id")?,record.try_get("current_version_id")?)?;}
                if !all {
                    let posts:Vec<String>=sqlx::query_scalar("SELECT COALESCE((SELECT jsonb_object_agg(p.key,p.value) FROM jsonb_each(payload) p WHERE p.key IN ('id','postKey','account','accountId','scope','sourceMediaScope','connectorBinding')),'{}'::jsonb)::text FROM communityhero.posts WHERE workspace_id=$1 AND payload->>'postKey'=ANY($2::text[]) ORDER BY ordinal LIMIT 101")
                        .bind(WORKSPACE).bind(&keys).fetch_all(&mut *tx).await?;
                    if posts.len()>MAX_POST_KEYS {return Err(internal("Instruction post identity scope exceeds bounded limit"));}
                    value["posts"]=json!(posts.iter().map(|p|parse(p)).collect::<ApiResult<Vec<_>>>()?);
                }
                tx.commit().await?;value
            }
            Self::Sqlite(pool)=>{
                let mut tx=pool.begin().await?;
                let record=sqlx::query("SELECT json_extract(payload,'$.account') AS account,CASE WHEN json_type(payload,'$.connectorBinding') IS NULL THEN NULL ELSE COALESCE(json_extract(payload,'$.connectorBinding'),'null') END AS binding FROM workspace WHERE id=1")
                    .fetch_one(&mut *tx).await?;
                let account:Option<String>=record.try_get("account")?;
                let mut value=root(account.clone(),account,record.try_get("binding")?)?;
                let key_json=json!(keys).to_string();
                let candidates:Vec<(String,String,i64)>=sqlx::query_as(SQLITE_CANDIDATES)
                    .bind(WORKSPACE).bind(all).bind(&key_json).bind("LikeAvto").bind((MAX_RULES+1) as i64)
                    .fetch_all(&mut *tx).await?;
                check_candidates(&candidates)?;
                let ids=json!(candidates.iter().map(|(id,_,_)|id).collect::<Vec<_>>()).to_string();
                let records=sqlx::query("SELECT json_extract(e.value,'$.id') AS id,json_extract(e.value,'$.currentVersionId') AS current_version_id,e.value AS entry,v.value AS version FROM workspace w,json_each(w.payload,'$.knowledge_entries') e LEFT JOIN json_each(w.payload,'$.knowledge_versions') v ON json_extract(v.value,'$.id')=json_extract(e.value,'$.currentVersionId') WHERE w.id=1 AND json_extract(e.value,'$.id') IN (SELECT value FROM json_each(?)) ORDER BY CAST(e.key AS INTEGER)")
                    .bind(ids).fetch_all(&mut *tx).await?;
                if records.len()!=candidates.len(){return Err(internal("Instruction head set changed"));}
                for record in records {append(&mut value,record.try_get("entry")?,record.try_get("version")?,record.try_get("id")?,record.try_get("current_version_id")?)?;}
                if !all {
                    let posts:Vec<String>=sqlx::query_scalar("SELECT COALESCE((SELECT json_group_object(p.key,json(CASE WHEN p.type IN ('object','array') THEN p.value WHEN p.type='true' THEN 'true' WHEN p.type='false' THEN 'false' ELSE json_quote(p.value) END)) FROM json_each(post.value) p WHERE p.key IN ('id','postKey','account','accountId','scope','sourceMediaScope','connectorBinding')), '{}') FROM workspace w,json_each(w.payload,'$.posts') post WHERE w.id=1 AND json_extract(post.value,'$.postKey') IN (SELECT value FROM json_each(?)) ORDER BY CAST(post.key AS INTEGER) LIMIT 101")
                        .bind(&key_json).fetch_all(&mut *tx).await?;
                    if posts.len()>MAX_POST_KEYS {return Err(internal("Instruction post identity scope exceeds bounded limit"));}
                    value["posts"]=json!(posts.iter().map(|p|parse(p)).collect::<ApiResult<Vec<_>>>()?);
                }
                tx.commit().await?;value
            }
        };
        crate::knowledge::validate_catalog(&value).map_err(internal)?;
        // Do not normalize/rewrite any version or hash. The exact account scope
        // is checked after structural validation as well as in SQL predicates.
        if value["knowledge_versions"].as_array().unwrap().iter().any(|v|v["scope"]["account"]!="LikeAvto") {
            return Err(internal("Instruction version account mismatch"));
        }
        // Keep the empty collection explicit for the existing rule-only domain path.
        value["materials"]=json!([]);
        Ok(value)
    }
}

#[cfg(test)]
#[path="storage_rules_tests.rs"]
mod tests;
