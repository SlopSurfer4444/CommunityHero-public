//! Read-only PostgreSQL snapshot for an authenticated owner CLOSE.
//! Keep the complete current source/catalog and complete reservation controls.
//! Only unrelated cold operation receipts are projected. No cache, expiry,
//! selected-recipient filter, status filter or persistence path exists here.
use super::*;
use crate::storage::scope_context;

// Routing probe only. The authoritative proposal is read again inside the
// repeatable-read transaction; this earlier statement supplies no source proof.
const PROBE: &str = r#"
SELECT w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
  AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 EXISTS(SELECT 1 FROM communityhero.proposals p WHERE p.workspace_id=w.id
   AND (p.id=$2 OR p.payload->>'id'=$2)
   AND COALESCE(p.payload->'operatorCloseDecision'<>'null'::jsonb,false)) AS owner_close,
 EXISTS(SELECT 1 FROM communityhero.proposals p WHERE p.workspace_id=w.id
   AND (p.id=$2 OR p.payload->>'id'=$2) AND p.payload ? 'retainedPaidRecovery') AS paid_recovery
FROM communityhero.workspaces w WHERE w.id=$1
"#;

const OPERATION_FIELDS: &[&str] = &[
    "id", "itemId", "proposalId", "approvalId", "status", "prepareRunId",
    "account", "accountId", "connectorBinding", "target", "action", "evidence",
    "approvedOperatorCloseDecisionSha256", "approvedEditorialReceiptSha256",
    "approvedPhotoAcquisitionProof", "approvedMediaContextWaiver",
];

fn protected_operations(proposal: &Value) -> Option<Vec<String>> {
    let preserved = &proposal["operatorCloseDecision"]["preservedUnknownReplies"];
    if preserved.is_null() { return Some(Vec::new()); }
    let records = preserved.get("operations")?.as_array()?;
    if records.is_empty() || records.len() > 100 { return None; }
    let mut seen = HashSet::new();
    let mut protected = Vec::new();
    for record in records {
        let id = record.get("operationId")?.as_str()?;
        if id.is_empty() || id.len() > 160
            || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || !seen.insert(id.to_owned()) { return None; }
        protected.push(id.to_owned());
    }
    Some(protected)
}

fn operation_payload_sql() -> String {
    let fields = OPERATION_FIELDS.iter().map(|key| format!("'{key}'"))
        .collect::<Vec<_>>().join(",");
    // Full target/action values preserve immutable and current-alias routing,
    // malformed legacy values and foreign-company boundaries. Only evidence
    // requiresReadback is a non-receipt control read by reservations.
    format!(r#"CASE WHEN $3::boolean AND jsonb_typeof(payload)='object'
      AND NOT (id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]))
      AND NOT (proposal_id IS NOT DISTINCT FROM $4::text OR payload->>'proposalId' IS NOT DISTINCT FROM $4::text)
      THEN COALESCE((SELECT jsonb_object_agg(f.key,
        CASE WHEN f.key='evidence' AND jsonb_typeof(f.value)='object'
          THEN COALESCE((SELECT jsonb_object_agg(e.key,e.value)
            FROM jsonb_each(f.value) e WHERE e.key='requiresReadback'),'{{}}'::jsonb)
          ELSE f.value END)
        FROM jsonb_each(payload) f WHERE f.key IN ({fields})),'{{}}'::jsonb)
      ELSE payload END"#)
}

fn fetch_stage(table: &str) -> &'static str {
    match table {
        "posts" => "owner_close.snapshot.posts.fetch",
        "branches" => "owner_close.snapshot.branches.fetch",
        "items" => "owner_close.snapshot.items.fetch",
        "proposals" => "owner_close.snapshot.proposals.fetch",
        "operations" => "owner_close.snapshot.operations.fetch",
        "materials" => "owner_close.snapshot.materials.fetch",
        "jobs" => "owner_close.snapshot.jobs.fetch",
        "knowledge_entries" => "owner_close.snapshot.knowledge_entries.fetch",
        "knowledge_versions" => "owner_close.snapshot.knowledge_versions.fetch",
        _ => "owner_close.snapshot.unexpected.fetch",
    }
}

impl Database {
    /// `None` means normal dispatch must obtain its normal fresh context.
    /// Paid-recovery proofs require full approvals/audit and retain their
    /// existing earlier route. SQLite retains its current admitted reader.
    pub(crate) async fn read_bounded_owner_close(&self, proposal_id: &str) -> ApiResult<Option<Value>> {
        let Self::Postgres { reader, .. } = self else { return Ok(None); };
        let probing = crate::performance::Span::new("owner_close.snapshot.probe");
        let probe = sqlx::query(PROBE).bind(WORKSPACE).bind(proposal_id).fetch_one(reader).await?;
        postgres_guard(&probe)?;
        let route = probe.try_get::<bool, _>("owner_close")?
            && !probe.try_get::<bool, _>("paid_recovery")?;
        drop(probing);
        if !route { return Ok(None); }

        let waiting = crate::performance::Span::new("owner_close.snapshot.pool_wait");
        let mut tx = reader.begin().await?;
        drop(waiting);
        let capture = crate::performance::Span::new("owner_close.snapshot.capture");
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx).await?;
        let record = sqlx::query("SELECT metadata::text,execution_enabled,(jsonb_typeof(metadata)='object' AND jsonb_typeof(metadata->'account')='string' AND account=metadata->>'account') IS TRUE AS identity_valid FROM communityhero.workspaces WHERE id=$1")
            .bind(WORKSPACE).fetch_one(&mut *tx).await?;
        postgres_guard(&record)?;
        let proposal_rows = sqlx::query("SELECT id,ordinal,payload::text AS payload,item_id,status FROM communityhero.proposals WHERE workspace_id=$1 AND (id=$2 OR payload->>'id'=$2) ORDER BY ordinal")
            .bind(WORKSPACE).bind(proposal_id).fetch_all(&mut *tx).await?;
        if proposal_rows.len() != 1 { return Err(internal("Owner close proposal identity mismatch")); }
        let proposal = parse(proposal_rows[0].try_get::<&str, _>("payload")?)?;
        if proposal_rows[0].try_get::<&str, _>("id")? != text(&proposal, "id")?
            || proposal["id"] != proposal_id {
            return Err(internal("Owner close proposal identity mismatch"));
        }
        // A changed route never combines the earlier probe with this snapshot.
        if proposal["operatorCloseDecision"].is_null() || proposal.get("retainedPaidRecovery").is_some() {
            tx.commit().await?;
            return Ok(None);
        }
        let protected = protected_operations(&proposal);
        let protected_ids = protected.clone().unwrap_or_default();
        let mut runs = Vec::new();
        for path in ["/prepareRunId", "/origin/prepareRunId", "/recovery/prepareRunId"] {
            if let Some(run) = proposal.pointer(path).and_then(Value::as_str) { runs.push(run.to_owned()); }
        }
        if let Some(context) = crate::conductor_authority::current_context() {
            if !runs.contains(&context.run_id) { runs.push(context.run_id); }
        }
        let mut selected_proposal_rows = Some(proposal_rows);
        let mut tables = Vec::new();
        for table in TABLES {
            if OMITTED.contains(&table) { continue; }
            if table == "proposals" {
                tables.push((table, selected_proposal_rows.take().ok_or_else(|| internal("Duplicate owner close proposal capture"))?));
                continue;
            }
            let condition = if table == "jobs" {
                "(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR kind IN ('media','media_audio') OR payload->>'kind' IN ('media','media_audio') OR ((kind='editorial_review' OR payload->>'kind'='editorial_review') AND (status IN ('running','queued') OR payload->>'status' IN ('running','queued'))) OR ((kind IN ('execute','reconcile') OR payload->>'kind' IN ('execute','reconcile')) AND (status IN ('running','queued','pending') OR payload->>'status' IN ('running','queued','pending'))))"
            } else { "TRUE" };
            // All current source/knowledge rows and the admitted media closure
            // remain full in this first cut. No semantic-media optimization is
            // inferred from a duration-only dispatch projection.
            let payload = if table == "operations" { operation_payload_sql() } else { "payload".to_owned() };
            let statement = format!("SELECT id,ordinal,({payload})::text AS payload{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal",
                projection(table).iter().map(|(column, _)| format!(",{column}")).collect::<String>());
            let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
            if table == "operations" { query = query.bind(&protected_ids).bind(protected.is_some()).bind(proposal_id); }
            else if table == "jobs" { query = query.bind(&runs); }
            let fetching = crate::performance::Span::new(fetch_stage(table));
            let captured = query.fetch_all(&mut *tx).await?;
            drop(fetching);
            tables.push((table, captured));
        }
        let control_span = crate::performance::Span::new("owner_close.snapshot.controls.capture");
        let controls = scope_context::capture(&mut tx).await?;
        drop(control_span);
        tx.commit().await?;
        drop(capture);
        // Connection release precedes bulk JSON/catalog validation and legacy
        // ownership reconstruction. All authoritative bytes share ONE snapshot.
        let mut value = parse_pg(record, tables)?;
        scope_context::decode(controls, &mut value)?;
        Ok(Some(value))
    }
}

#[cfg(test)]
#[path = "storage_owner_close_tests.rs"]
mod tests;
