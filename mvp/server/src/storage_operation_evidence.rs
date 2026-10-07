//! Durable evidence updates for one existing operation. This API cannot change
//! operation state, routing, approval authority or any other workspace record.
use super::*;

pub(crate) enum OperationEvidenceUpdate {
    Readback(Value),
    ExecuteReceipt(Value),
}

fn patch_operation(stored: &mut Value, expected: &Value, update: OperationEvidenceUpdate) -> ApiResult<bool> {
    let action = stored["action"].as_object().ok_or_else(|| internal("Invalid operation action"))?;
    let expected_action = expected["action"].as_object().ok_or_else(|| internal("Invalid expected operation action"))?;
    let key = text(expected, "id")?;
    if key.is_empty() || text(stored, "id")? != key
        || action.get("actionId").and_then(Value::as_str).is_none_or(str::is_empty)
        || !action.iter().filter(|(k, _)| k.as_str() != "readbackEvidence")
            .eq(expected_action.iter().filter(|(k, _)| k.as_str() != "readbackEvidence"))
        || ["target", "itemId", "proposalId", "approvalId", "attemptId", "dispatchAuthority", "approvedBy", "executedBy", "editorialPolicyVersion"]
            .iter().any(|field| stored.get(*field) != expected.get(*field))
    {
        return Err(crate::conflict("Evidence belongs to a different operation"));
    }
    for (_, field) in projection("operations") {
        if !stored[*field].is_null() && !stored[*field].is_string() {
            return Err(internal("Invalid operation projected field"));
        }
    }
    let immutable_receipt=matches!(&update,OperationEvidenceUpdate::ExecuteReceipt(_));
    let slot = match update {
        OperationEvidenceUpdate::Readback(value) => (stored["action"].as_object_mut().unwrap(), "readbackEvidence", value),
        OperationEvidenceUpdate::ExecuteReceipt(value) => (stored.as_object_mut().ok_or_else(|| internal("Invalid operation"))?, "executeReceipt", value),
    };
    // Distinguish an absent field from explicit null, just like the old writes.
    if slot.0.get(slot.1) == Some(&slot.2) { return Ok(false); }
    if immutable_receipt&&slot.0.contains_key(slot.1) {
        return Err(crate::conflict("Original execute receipt is immutable for this attempt"));
    }
    slot.0.insert(slot.1.to_owned(), slot.2);
    Ok(true)
}

fn validate_operation_record(payload: &Value, key: &str, ordinal: i32, columns: &[Option<String>]) -> ApiResult<()> {
    if !payload.is_object() || text(payload, "id")? != key || ordinal < 0
        || columns.len() != projection("operations").len() {
        return Err(internal("Operation record identity mismatch"));
    }
    for ((_, field), column) in projection("operations").iter().zip(columns) {
        if (!payload[*field].is_null() && !payload[*field].is_string())
            || payload[*field].as_str() != column.as_deref() {
            return Err(internal("Operation relational projection mismatch"));
        }
    }
    Ok(())
}

impl Database {
    /// Caller must retain the normal App writer gate and publish cache/event
    /// invalidation only when `changed` is true. No arbitrary mutation callback.
    pub(crate) async fn change_operation_evidence_observed(
        &self,
        expected: &Value,
        update: OperationEvidenceUpdate,
        expected_runtime: &crate::runtime_lifecycle::RuntimeIdentity,
    ) -> ApiResult<((), bool)> {
        let _total = crate::performance::Span::new("operation.evidence.total");
        let key = text(expected, "id")?;
        match self {
            Self::Sqlite(_) => self.change_observed(|workspace| {
                crate::runtime_lifecycle::current_owner(workspace,expected_runtime)?;
                let matches = rows(workspace, "operations")?.iter().filter(|op| op["id"] == key).count();
                if matches != 1 { return Err(internal("Operation identity missing or duplicated")); }
                patch_operation(crate::row_mut(workspace, "operations", key)?, expected, update)?;
                Ok(())
            }).await,
            Self::Postgres { writer, .. } => {
                let waiting = crate::performance::Span::new("operation.evidence.pool_wait");
                let mut tx = writer.begin().await?;
                drop(waiting);
                let lock = crate::performance::Span::new("operation.evidence.row_lock_wait");
                let record = sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                drop(lock);
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                let metadata = parse(record.try_get::<&str, _>("metadata")?)?;
                crate::runtime_lifecycle::current_owner(&metadata,expected_runtime)?;
                if !metadata.is_object() || metadata["account"].as_str().is_none()
                    || record.try_get::<Option<String>, _>("account")?.as_deref() != metadata["account"].as_str() {
                    return Err(internal("Workspace identity mismatch"));
                }
                if TABLES.iter().any(|table| metadata.get(*table).is_some()) {
                    return Err(internal("Workspace metadata contains entity collections"));
                }
                let load = crate::performance::Span::new("operation.evidence.load");
                let record = sqlx::query("SELECT id,ordinal,payload::text,item_id,proposal_id,approval_id,status FROM communityhero.operations WHERE workspace_id=$1 AND id=$2 FOR UPDATE")
                    .bind(WORKSPACE).bind(key).fetch_optional(&mut *tx).await?
                    .ok_or_else(|| internal("Operation not found"))?;
                let mut payload = parse(record.try_get::<&str, _>("payload")?)?;
                let columns = projection("operations").iter()
                    .map(|(column, _)| record.try_get::<Option<String>, _>(*column))
                    .collect::<Result<Vec<_>, _>>()?;
                validate_operation_record(&payload, record.try_get::<&str, _>("id")?, record.try_get("ordinal")?, &columns)?;
                drop(load);
                let changed = patch_operation(&mut payload, expected, update)?;
                if !changed { tx.commit().await?; return Ok(((), false)); }
                // Only JSON evidence changed; every typed relational column,
                // identity, ordinal and unrelated payload field stays untouched.
                validate_operation_record(&payload, key, record.try_get("ordinal")?, &columns)?;
                let _write = crate::performance::Span::new("operation.evidence.write");
                let result = sqlx::query("UPDATE communityhero.operations SET payload=$3::jsonb WHERE workspace_id=$1 AND id=$2")
                    .bind(WORKSPACE).bind(key).bind(payload.to_string()).execute(&mut *tx).await?;
                if result.rows_affected() != 1 { return Err(internal("Operation update identity mismatch")); }
                tx.commit().await?;
                Ok(((), true))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn operation() -> Value {
        json!({"id":"op","itemId":"item","proposalId":"proposal","approvalId":"approval","attemptId":"attempt",
            "status":"dispatching","target":{"connectorBinding":{"account":"likeavto"},"revision":1},
            "action":{"actionId":"action","itemId":"external","action":"reply_and_close","text":"Synthetic reply",
                "contextEvidenceDigest":"digest","readbackEvidence":{"baselineReplyIds":[]}},
            "dispatchAuthority":{"actor":"synthetic-reviewer"},"editorialPolicyVersion":1,"future":{"preserved":true}})
    }

    #[test]
    fn operation_evidence_changes_only_allowed_fields_and_is_idempotent() {
        for readback in [false, true] {
            let original = operation();
            let mut actual = original.clone();
            let mut expected_identity = original.clone();
            // A refreshed local baseline does not redefine action identity.
            expected_identity["action"]["readbackEvidence"] = json!({"baselineReplyIds":["local"]});
            let evidence = json!({"synthetic":"evidence"});
            let update = || if readback { OperationEvidenceUpdate::Readback(evidence.clone()) }
                else { OperationEvidenceUpdate::ExecuteReceipt(evidence.clone()) };
            assert!(patch_operation(&mut actual, &expected_identity, update()).unwrap());
            let mut expected = original;
            if readback { expected["action"]["readbackEvidence"] = evidence.clone(); } else { expected["executeReceipt"] = evidence.clone(); }
            assert_eq!(actual, expected);
            assert!(!patch_operation(&mut actual, &expected_identity, update()).unwrap());
        }
        let mut legacy = operation(); legacy.as_object_mut().unwrap().remove("attemptId");
        let expected = legacy.clone();
        assert!(patch_operation(&mut legacy, &expected, OperationEvidenceUpdate::ExecuteReceipt(Value::Null)).unwrap());
        let expected = operation();
        let mut later = expected.clone(); later["status"] = json!("unknown"); later["result"] = json!({"readback":"pending"});
        assert!(patch_operation(&mut later, &expected, OperationEvidenceUpdate::ExecuteReceipt(json!({"late":true}))).unwrap());
        assert_eq!(later["status"], "unknown");
        assert_eq!(later["result"], json!({"readback":"pending"}));
    }

    #[test]
    fn operation_evidence_rejects_retargeting_and_malformed_projections_before_mutation() {
        let original = operation();
        for pointer in ["/id","/itemId","/proposalId","/approvalId","/attemptId","/target/revision",
            "/target/connectorBinding/account","/dispatchAuthority/actor","/action/actionId","/action/itemId","/action/text",
            "/action/contextEvidenceDigest","/action/action","/editorialPolicyVersion"] {
            let mut expected = original.clone(); *expected.pointer_mut(pointer).unwrap() = json!("different");
            let mut stored = original.clone();
            assert!(patch_operation(&mut stored, &expected, OperationEvidenceUpdate::ExecuteReceipt(json!({}))).is_err(),"{pointer}");
            assert_eq!(stored, original);
        }
        let columns: Vec<Option<String>> = projection("operations").iter().map(|(_, field)| original[*field].as_str().map(str::to_owned)).collect();
        assert!(validate_operation_record(&original,"op",8,&columns).is_ok());
        assert!(validate_operation_record(&original,"other",8,&columns).is_err());
        assert!(validate_operation_record(&original,"op",-1,&columns).is_err());
        for i in 0..columns.len() {
            let mut wrong = columns.clone(); wrong[i] = Some("wrong".to_owned());
            assert!(validate_operation_record(&original,"op",8,&wrong).is_err());
        }
        let mut malformed = original.clone(); malformed["status"] = json!({"not":"a string"});
        assert!(validate_operation_record(&malformed,"op",8,&columns).is_err());
        let before = malformed.clone();
        assert!(patch_operation(&mut malformed, &before, OperationEvidenceUpdate::Readback(json!({}))).is_err());
        assert_eq!(malformed, before);
    }

    #[tokio::test]
    async fn operation_evidence_sqlite_matches_document_write_and_rolls_back_mismatch() {
        let folder = tempfile::tempdir().unwrap();
        let db = Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
        let original = operation();
        db.change(|data| {data["operations"] = json!([original]); Ok(())}).await.unwrap();
        let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
        let before = db.read().await.unwrap();
        let receipt = json!({"status":"synthetic","nested":{"retained":[1,2]}});
        let mut identity = original.clone(); identity["action"]["itemId"] = json!("other");
        assert!(db.change_operation_evidence_observed(&identity, OperationEvidenceUpdate::ExecuteReceipt(receipt.clone()), &runtime).await.is_err());
        assert_eq!(db.read().await.unwrap(), before);
        let ((), changed) = db.change_operation_evidence_observed(&original, OperationEvidenceUpdate::ExecuteReceipt(receipt.clone()), &runtime).await.unwrap();
        assert!(changed);
        let mut expected = before;
        expected["operations"][0]["executeReceipt"] = receipt.clone();
        assert_eq!(db.read().await.unwrap(), expected);
        assert!(!db.change_operation_evidence_observed(&original, OperationEvidenceUpdate::ExecuteReceipt(receipt), &runtime).await.unwrap().1);
        assert!(db.change_operation_evidence_observed(&original, OperationEvidenceUpdate::ExecuteReceipt(json!({"error":"local retirement failed"})), &runtime).await.is_err());
        assert_eq!(db.read().await.unwrap(),expected,"later local errors cannot overwrite a committed provider receipt");
        let baseline = json!({"baselineReplyIds":["prior"]});
        assert!(db.change_operation_evidence_observed(&original, OperationEvidenceUpdate::Readback(baseline.clone()), &runtime).await.unwrap().1);
        expected["operations"][0]["action"]["readbackEvidence"] = baseline;
        assert_eq!(db.read().await.unwrap(), expected);
        db.close().await;
        let reopened = Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
        assert_eq!(reopened.read().await.unwrap(), expected);
        reopened.close().await;
    }
}
