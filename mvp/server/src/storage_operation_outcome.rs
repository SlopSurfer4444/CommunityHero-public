//! Atomic outcome unit: exact operation/proposal/item, one feedback event, and
//! append-only audit. The workspace barrier also serializes history ordinals.
use super::*;
use serde_json::json;

fn event_id(op: &Value, status: &str) -> ApiResult<String> {
    if !matches!(status, "succeeded" | "unknown" | "failed" | "stale") {
        return Err(internal("Invalid operation outcome status"));
    }
    Ok(format!("execution:{}:{status}", text(op,"id")?))
}

fn identity(stored: &Value, expected: &Value) -> ApiResult<()> {
    for field in ["id","itemId","proposalId","approvalId","attemptId","target","dispatchAuthority","approvedBy","executedBy","editorialPolicyVersion"] {
        if stored.get(field) != expected.get(field) { return Err(crate::conflict("Outcome belongs to a different operation")); }
    }
    let action_identity = |op: &Value| {
        let mut action = op.get("action").cloned();
        if let Some(Value::Object(fields)) = &mut action { fields.remove("readbackEvidence"); }
        action
    };
    if action_identity(stored) != action_identity(expected) {
        return Err(crate::conflict("Outcome belongs to a different operation"));
    }
    Ok(())
}

fn exactly_one<'a>(data: &'a Value, table: &str, key: &str) -> ApiResult<&'a Value> {
    let found: Vec<_> = rows(data,table)?.iter().filter(|row| row["id"] == key).collect();
    if found.len() != 1 { return Err(internal("Outcome record missing or duplicated")); }
    Ok(found[0])
}

fn ready_sibling(proposal:&Value,item:&Value,op:&Value)->bool {
    proposal["id"]!=op["proposalId"] && proposal["itemId"]==op["itemId"]
        && matches!(proposal["status"].as_str(),Some("draft"|"approved"))
        && proposal["itemRevision"]==item["revision"]
        && proposal["contextEvidenceDigest"]==item["contextEvidenceDigest"]
        && proposal["branchContextDigest"]==item["branchContextDigest"]
        && match proposal["kind"].as_str() {
            Some("reply_and_close")=>proposal["text"].as_str().is_some_and(|text|!text.trim().is_empty()),
            Some("close"|"hide"|"delete")=>true,
            _=>false,
        }
}

fn project(full: &Value, op: &Value, status: &str) -> ApiResult<Value> {
    let mut view = metadata(full);
    for table in TABLES { view[table] = json!([]); }
    for (table, field) in [("operations","id"),("proposals","proposalId"),("items","itemId")] {
        view[table] = json!([exactly_one(full,table,text(op,field)?)?]);
    }
    identity(&view["operations"][0],op)?;
    view["operationOutcomeHasReadySibling"]=json!(rows(full,"proposals")?.iter()
        .any(|proposal|ready_sibling(proposal,&view["items"][0],op)));
    let event = event_id(op,status)?;
    view["feedback"] = Value::Array(rows(full,"feedback")?.iter().filter(|e| e["id"] == event).cloned().collect());
    if rows(&view,"feedback")?.len() > 1 { return Err(internal("Duplicate outcome feedback identity")); }
    Ok(view)
}

fn same_except(before: &Value, after: &Value, allowed: &[&str]) -> bool {
    match (before.as_object(),after.as_object()) {
        (Some(a),Some(b)) => a.iter().filter(|(k,_)| !allowed.contains(&k.as_str()))
            .eq(b.iter().filter(|(k,_)| !allowed.contains(&k.as_str()))),
        _ => false,
    }
}

fn validate_patch(before: &Value, after: &Value, op: &Value, status: &str) -> ApiResult<()> {
    if !after.is_object() { return Err(internal("Outcome projection must remain an object")); }
    if metadata(before) != metadata(after) { return Err(internal("Outcome cannot change workspace metadata")); }
    for table in TABLES {
        if !["operations","proposals","items","feedback","audit"].contains(&table) && before[table] != after[table] {
            return Err(internal("Outcome changed an unrelated collection"));
        }
    }
    for (table,allowed) in [
        ("operations", &["status","evidence","updatedAt"][..]),
        ("proposals", &["status"][..]),
        ("items", &["providerStatus","workflow","hidden","revision"][..]),
    ] {
        let old = rows(before,table)?;
        let new = rows(after,table)?;
        if old.len()!=1 || new.len()!=1 || !same_except(&old[0],&new[0],allowed) {
            return Err(internal("Outcome changed protected record fields"));
        }
        for (_,field) in projection(table) {
            if !new[0][*field].is_null() && !new[0][*field].is_string() {
                return Err(internal("Invalid outcome relational field"));
            }
        }
    }
    if after["operations"][0]["status"] != status || after["proposals"][0]["status"] != status
        || !after["operations"][0]["updatedAt"].is_string() {
        return Err(internal("Outcome status or clock mismatch"));
    }
    // Foreign/rebound items may receive a historical operation receipt, but
    // cannot be locally closed/hidden/deleted by that receipt.
    let old_item = &before["items"][0];
    let new_item = &after["items"][0];
    if status != "succeeded" || !crate::outcome_matches_item(before,op) {
        let mut expected=old_item.clone();
        if crate::operation_outcome_needs_attention(before,op,status) {
            expected["workflow"]=json!("attention");
        }
        if new_item != &expected { return Err(internal("Outcome cannot change this item")); }
    } else {
        let mut expected = old_item.clone();
        let deleted = op["action"]["action"] == "delete";
        expected["providerStatus"] = json!(if deleted {"deleted"} else {"closed"});
        if op["action"]["action"] == "hide" { expected["hidden"] = json!(true); }
        if expected["workflow"] != "waiting" { expected["workflow"] = json!(if deleted {"deleted"} else {"closed"}); }
        crate::bump(&mut expected);
        if new_item != &expected { return Err(internal("Outcome item transition mismatch")); }
    }
    let old_feedback = rows(before,"feedback")?;
    let new_feedback = rows(after,"feedback")?;
    if !new_feedback.starts_with(old_feedback) || new_feedback.len() != 1 {
        return Err(internal("Outcome feedback must preserve the exact event"));
    }
    let event = &new_feedback[0];
    if event["id"] != event_id(op,status)? || event["itemId"] != op["itemId"] {
        return Err(internal("Outcome feedback identity mismatch"));
    }
    let audits = rows(after,"audit")?;
    if !rows(before,"audit")?.is_empty() || audits.len()!=1
        || audits[0]["refId"] != op["id"] || audits[0]["action"] != format!("operation.{status}")
        || text(&audits[0],"id")?.is_empty() || !audits[0]["createdAt"].is_string() {
        return Err(internal("Outcome audit append mismatch"));
    }
    crate::db_guards::validate_change(before,after)?;
    Ok(())
}

async fn load(connection: &mut PgConnection, table: &str, key: &str) -> ApiResult<Vec<Value>> {
    let statement = format!("SELECT id,ordinal,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND id=$2 FOR UPDATE",
        projection(table).iter().map(|(column,_)| format!(",{column}")).collect::<String>());
    let records = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(key).fetch_all(connection).await?;
    records.into_iter().map(|record| {
        let payload = parse(record.try_get::<&str,_>("payload")?)?;
        if !payload.is_object() || text(&payload,"id")? != key
            || record.try_get::<&str,_>("id")? != key || record.try_get::<i32,_>("ordinal")? < 0 {
            return Err(internal("Outcome record identity mismatch"));
        }
        for (column,field) in projection(table) {
            if (!payload[*field].is_null() && !payload[*field].is_string())
                || record.try_get::<Option<String>,_>(*column)?.as_deref() != payload[*field].as_str() {
                return Err(internal("Outcome relational projection mismatch"));
            }
        }
        Ok(payload)
    }).collect()
}

async fn save(connection: &mut PgConnection, table: &str, payload: &Value, append: bool) -> ApiResult<()> {
    let columns = projection(table);
    let statement = if append {
        format!("INSERT INTO communityhero.{table}(workspace_id,id,payload,ordinal{}) VALUES($1,$2,$3::jsonb,(SELECT COALESCE(MAX(ordinal),-1)+1 FROM communityhero.{table} WHERE workspace_id=$1){})",
            columns.iter().map(|(column,_)|format!(",{column}")).collect::<String>(),
            (0..columns.len()).map(|n|format!(",${}",n+4)).collect::<String>())
    } else {
        format!("UPDATE communityhero.{table} SET payload=$3::jsonb{} WHERE workspace_id=$1 AND id=$2",
            columns.iter().enumerate().map(|(n,(column,_))|format!(",{column}=${}",n+4)).collect::<String>())
    };
    let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(text(payload,"id")?).bind(payload.to_string());
    for (_,field) in columns { query = query.bind(payload[*field].as_str()); }
    if query.execute(connection).await?.rows_affected() != 1 { return Err(internal("Outcome write identity mismatch")); }
    Ok(())
}

impl Database {
    pub(crate) async fn change_operation_outcome_observed<T>(&self, op: &Value, status: &str, f: impl FnOnce(&mut Value)->ApiResult<T>) -> ApiResult<(T,bool)> {
        let _total = crate::performance::Span::new("operation.outcome.total");
        let event = event_id(op,status)?;
        match self {
            Self::Sqlite(_) => self.change_observed(|workspace| {
                let before = project(workspace,op,status)?;
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_patch(&before,&after,op,status)?;
                for (table,field) in [("operations","id"),("proposals","proposalId"),("items","itemId")] {
                    *crate::row_mut(workspace,table,text(op,field)?)? = after[table][0].clone();
                }
                for table in ["feedback","audit"] {
                    for record in &rows(&after,table)?[rows(&before,table)?.len()..] {
                        if rows(workspace,table)?.iter().any(|old|old["id"]==record["id"]) { return Err(internal("Outcome append reused identity")); }
                        workspace[table].as_array_mut().unwrap().push(record.clone());
                    }
                }
                Ok(result)
            }).await,
            Self::Postgres {writer,..} => {
                let waiting = crate::performance::Span::new("operation.outcome.pool_wait");
                let mut tx = writer.begin().await?;
                drop(waiting);
                let lock = crate::performance::Span::new("operation.outcome.row_lock_wait");
                let record = sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                drop(lock);
                if record.try_get::<bool,_>("execution_enabled")? { return Err(internal("PostgreSQL pilot execution must remain disabled")); }
                let mut before = parse(record.try_get::<&str,_>("metadata")?)?;
                if !before.is_object() || before["account"].as_str().is_none()
                    || record.try_get::<Option<String>,_>("account")?.as_deref()!=before["account"].as_str() {
                    return Err(internal("Workspace identity mismatch"));
                }
                for table in TABLES {
                    if before.get(table).is_some() { return Err(internal("Workspace metadata contains entity collections")); }
                    before[table]=json!([]);
                }
                let loading = crate::performance::Span::new("operation.outcome.load");
                for (table,key) in [("operations",text(op,"id")?),("proposals",text(op,"proposalId")?),("items",text(op,"itemId")?),("feedback",event.as_str())] {
                    before[table] = json!(load(&mut tx,table,key).await?);
                    if table!="feedback" { exactly_one(&before,table,key)?; }
                }
                identity(&before["operations"][0],op)?;
                // Keep sibling drafts outside the mutable projection. The
                // workspace row lock binds this read to the outcome transaction.
                let siblings:Vec<String>=sqlx::query_scalar("SELECT payload::text FROM communityhero.proposals WHERE workspace_id=$1 AND item_id=$2 AND id<>$3 AND status IN ('draft','approved') ORDER BY ordinal")
                    .bind(WORKSPACE).bind(text(op,"itemId")?).bind(text(op,"proposalId")?)
                    .fetch_all(&mut *tx).await?;
                let mut has_ready=false;
                for payload in siblings {
                    let sibling=parse(&payload)?;
                    if ready_sibling(&sibling,&before["items"][0],op) {has_ready=true;}
                }
                before["operationOutcomeHasReadySibling"]=json!(has_ready);
                drop(loading);
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_patch(&before,&after,op,status)?;
                let _writing = crate::performance::Span::new("operation.outcome.write");
                for table in ["operations","proposals","items"] {
                    if before[table]!=after[table] { save(&mut tx,table,&after[table][0],false).await?; }
                }
                if rows(&before,"feedback")?.is_empty() { save(&mut tx,"feedback",&after["feedback"][0],true).await?; }
                save(&mut tx,"audit",&after["audit"][0],true).await?;
                tx.commit().await?;
                Ok((result,true))
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_operation_outcome_tests.rs"]
mod tests;
