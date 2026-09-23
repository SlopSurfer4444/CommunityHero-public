//! History contracts shared by the SQLite write path and PostgreSQL schema v3.
use crate::{ApiResult, internal};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashSet;

pub(crate) const REQUIRED_SCHEMA: i32 = 3;

/// Read-only startup gate: upgrades are always an explicit offline operation.
pub(crate) async fn require_schema(pool: &PgPool) -> ApiResult<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('communityhero.schema_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if !exists {
        return Err(internal(
            "Database history guards require offline upgrade-guards --apply",
        ));
    }
    let applied: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communityhero.schema_migrations WHERE version=$1)",
    )
    .bind(REQUIRED_SCHEMA)
    .fetch_one(pool)
    .await?;
    if !applied {
        return Err(internal(
            "Database history guards require offline upgrade-guards --apply",
        ));
    }
    Ok(())
}

/// Omitted collections are allowed only on both sides of a bounded projection.
/// Present collections must be complete, ordered history, never a filtered slice.
pub(crate) fn validate_change(before: &Value, after: &Value) -> ApiResult<()> {
    for table in ["audit", "approvals"] {
        if before.get(table).is_none() && after.get(table).is_none() {
            continue;
        }
        let old = records(before, table)?;
        let new = records(after, table)?;
        let mut ids = HashSet::new();
        for record in new {
            let valid_id = record["id"].as_str().filter(|s| !s.trim().is_empty());
            if !record.is_object() || valid_id.is_none() || !ids.insert(valid_id.unwrap()) {
                return Err(internal("Invalid or duplicate history identity"));
            }
            let projection: &[&str] = if table == "audit" {
                &["action", "refId"]
            } else {
                &["status"]
            };
            if projection
                .iter()
                .any(|key| !record[*key].is_null() && !record[*key].is_string())
            {
                return Err(internal("Invalid history projection"));
            }
        }
        if new.len() < old.len() {
            return Err(internal("Approval and audit history cannot be deleted"));
        }
        for (original, updated) in old.iter().zip(new) {
            if table == "audit" {
                if original != updated {
                    return Err(internal("Audit history cannot be changed or reordered"));
                }
            } else {
                let original = original
                    .as_object()
                    .ok_or_else(|| internal("Invalid approval history"))?;
                let updated = updated
                    .as_object()
                    .ok_or_else(|| internal("Invalid approval history"))?;
                // Compare by reference: approvals can embed large branch context.
                if !original
                    .iter()
                    .filter(|(key, _)| key.as_str() != "status")
                    .eq(updated.iter().filter(|(key, _)| key.as_str() != "status"))
                {
                    return Err(internal(
                        "Approval identity, context and authority are immutable",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn records<'a>(workspace: &'a Value, table: &str) -> ApiResult<&'a Vec<Value>> {
    workspace[table]
        .as_array()
        .ok_or_else(|| internal("Missing or invalid history collection"))
}

#[cfg(test)]
#[path = "db_guards_tests.rs"]
mod tests;
