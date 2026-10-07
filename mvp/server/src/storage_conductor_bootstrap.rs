//! Read-only conductor policy input. A campaign needs complete materials and
//! metadata here, not the planner's unrelated source and paid-history corpus.
use super::*;
use serde_json::json;

const PG_POLICY: &str = r#"
SELECT w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
  AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 w.metadata::text AS metadata,
 COALESCE((SELECT json_agg(json_build_object('id',m.id,'kind',m.kind,'payload',m.payload) ORDER BY m.ordinal)
  FROM communityhero.materials m WHERE m.workspace_id=w.id),'[]'::json)::text AS materials
FROM communityhero.workspaces w WHERE w.id=$1
"#;

const SQLITE_POLICY: &str = r#"
SELECT json_remove(w.payload,'$.posts','$.branches','$.items','$.conversations',
 '$.proposals','$.approvals','$.operations','$.materials','$.jobs','$.audit',
 '$.knowledge_entries','$.knowledge_versions','$.feedback') AS metadata,
 CASE WHEN json_type(w.payload,'$.materials') IS NULL THEN '[]'
 WHEN json_type(w.payload,'$.materials')='array' THEN
  COALESCE((SELECT json_group_array(json_object('id',json_extract(m.value,'$.id'),
   'kind',json_extract(m.value,'$.kind'),'payload',json(m.value)))
   FROM (SELECT value FROM json_each(w.payload,'$.materials') ORDER BY CAST(key AS INTEGER)) m),'[]')
 ELSE 'null' END AS materials
FROM workspace w WHERE w.id=1
"#;

fn decode_policy(metadata_payload: &str, materials_payload: &str) -> ApiResult<Value> {
    let mut view = parse(metadata_payload)?;
    if !view.is_object() || view["account"].as_str().is_none()
        || TABLES.iter().any(|table| view.get(*table).is_some()) {
        return Err(internal("Invalid conductor policy metadata"));
    }
    let records = parse(materials_payload)?;
    let mut ids = HashSet::new();
    let mut materials = Vec::new();
    for record in records.as_array().ok_or_else(|| internal("Invalid conductor materials"))? {
        let payload = &record["payload"];
        let id = text(payload, "id")?;
        if record["id"].as_str() != Some(id) || !ids.insert(id.to_owned()) {
            return Err(internal("Conductor material identity mismatch"));
        }
        for (_, field) in projection("materials") {
            if (!payload[*field].is_null() && !payload[*field].is_string())
                || record[*field] != payload[*field] {
                return Err(internal("Conductor material projection mismatch"));
            }
        }
        materials.push(payload.clone());
    }
    view["materials"] = json!(materials);
    Ok(view)
}

impl Database {
    pub(crate) async fn read_conductor_bootstrap_policy(&self) -> ApiResult<Value> {
        // Both columns share one statement snapshot. The pool connection is
        // released before parsing; neither read takes the workspace writer.
        let (metadata, materials) = match self {
            Self::Sqlite(pool) => {
                let record = sqlx::query(SQLITE_POLICY).fetch_one(pool).await?;
                (record.try_get::<String, _>("metadata")?, record.try_get::<String, _>("materials")?)
            }
            Self::Postgres { reader, .. } => {
                let record = sqlx::query(PG_POLICY).bind(WORKSPACE).fetch_one(reader).await?;
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                if !record.try_get::<bool, _>("identity_valid")? {
                    return Err(internal("Workspace identity mismatch"));
                }
                (record.try_get::<String, _>("metadata")?, record.try_get::<String, _>("materials")?)
            }
        };
        decode_policy(&metadata, &materials)
    }
}

#[cfg(test)]
#[path = "storage_conductor_bootstrap_tests.rs"]
mod tests;
