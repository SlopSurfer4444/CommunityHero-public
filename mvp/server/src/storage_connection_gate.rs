//! One bounded active dispatch cohort under the ordinary workspace writer.
//! Old operation history is not used as an approximation of active permits.
use super::*;
use crate::connection_gate::{self,Scope};
use serde_json::json;

const MAX_ROWS:usize=2*connection_gate::MAX_MUTATION_PERMITS+1;
const MAX_BYTES:usize=8*1024*1024;
fn malformed_permit(op:&Value)->bool {
    op.get(connection_gate::PERMIT_FIELD).is_some()&&!connection_gate::valid_permit(op)
}

// Exclude only provably valid native settled rows. Armed, unknown and malformed
// rows stay selected. Noncanonical timestamp/string forms are conservatively
// selected too; this predicate uses no unsafe numeric or timestamp casts.
const COHORT_SQL:&str=r#"WITH selected AS MATERIALIZED (
    SELECT id,item_id,proposal_id,approval_id,status,payload,ordinal
    FROM communityhero.operations WHERE workspace_id=$1 AND (
      id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[])
      OR payload#>>'{dispatchPermit,phase}'='dispatch_armed'
      OR (payload ? 'dispatchPermit' AND NOT COALESCE(
        jsonb_typeof(payload->'dispatchPermit')='object'
        AND (payload->'dispatchPermit') ?& ARRAY['version','id','operationId','attemptId','account','connectionBinding','owner','gateEpoch','archiveFenceSha256','phase','armedAt','providerAttemptObserved','cessation','settledAt']
        AND (payload#>'{dispatchPermit,version}')::text='1'
        AND jsonb_typeof(payload#>'{dispatchPermit,id}')='string' AND payload#>>'{dispatchPermit,id}'<>''
        AND jsonb_typeof(payload->'id')='string' AND payload->>'id'<>''
        AND jsonb_typeof(payload->'attemptId')='string' AND payload->>'attemptId'<>''
        AND payload#>'{dispatchPermit,operationId}'=payload->'id'
        AND payload#>'{dispatchPermit,attemptId}'=payload->'attemptId'
        AND NOT EXISTS (
          SELECT 1 FROM (VALUES (payload#>'{dispatchPermit,gateEpoch}'),
            (payload#>'{dispatchPermit,owner,epoch}'),(payload#>'{dispatchPermit,connectionBinding,revision}')) AS native_u64(value)
          WHERE NOT COALESCE(jsonb_typeof(value)='number' AND value::text ~ '^[1-9][0-9]{0,19}$'
            AND (length(value::text)<20 OR value::text COLLATE "C"<='18446744073709551615'),false))
        AND jsonb_typeof(payload#>'{dispatchPermit,owner}')='object'
        AND payload#>'{dispatchPermit,owner}'=jsonb_build_object(
          'account',payload#>'{dispatchPermit,owner,account}','runtimeId',payload#>'{dispatchPermit,owner,runtimeId}',
          'releaseSha256',payload#>'{dispatchPermit,owner,releaseSha256}','epoch',payload#>'{dispatchPermit,owner,epoch}')
        AND jsonb_typeof(payload#>'{dispatchPermit,owner,account}')='string'
        AND ((payload#>'{dispatchPermit,account}'='"likeavto"'::jsonb AND payload#>'{dispatchPermit,owner,account}'='"LikeAvto"'::jsonb)
          OR (payload#>'{dispatchPermit,account}'='"baw-russia"'::jsonb AND payload#>'{dispatchPermit,owner,account}'='"BAW Russia"'::jsonb))
        AND jsonb_typeof(payload#>'{dispatchPermit,owner,runtimeId}')='string'
        AND payload#>>'{dispatchPermit,owner,runtimeId}' ~ '^[A-Za-z0-9_-]{1,80}$'
        AND jsonb_typeof(payload#>'{dispatchPermit,owner,releaseSha256}')='string'
        AND payload#>>'{dispatchPermit,owner,releaseSha256}' ~ '^[a-f0-9]{64}$'
        AND jsonb_typeof(payload#>'{dispatchPermit,connectionBinding}')='object'
        AND payload#>'{dispatchPermit,connectionBinding}'=jsonb_build_object(
          'id',payload#>'{dispatchPermit,connectionBinding,id}','workspaceId',payload#>'{dispatchPermit,connectionBinding,workspaceId}',
          'accountId',payload#>'{dispatchPermit,connectionBinding,accountId}','connector',payload#>'{dispatchPermit,connectionBinding,connector}',
          'revision',payload#>'{dispatchPermit,connectionBinding,revision}','providerAccountId',payload#>'{dispatchPermit,connectionBinding,providerAccountId}')
        AND payload#>'{dispatchPermit,connectionBinding}'=payload#>'{target,connectorBinding}'
        AND payload#>'{dispatchPermit,connectionBinding,workspaceId}'='"local-pilot"'::jsonb
        AND payload#>'{dispatchPermit,connectionBinding,accountId}'=payload#>'{dispatchPermit,owner,account}'
        AND payload#>>'{dispatchPermit,connectionBinding,connector}' IN ('angryspace','vk','instagram','youtube','tiktok')
        AND NOT EXISTS (
          SELECT 1 FROM (VALUES (payload#>'{dispatchPermit,connectionBinding,id}'),
            (payload#>'{dispatchPermit,connectionBinding,workspaceId}'),(payload#>'{dispatchPermit,connectionBinding,accountId}'),
            (payload#>'{dispatchPermit,connectionBinding,connector}'),(payload#>'{dispatchPermit,connectionBinding,providerAccountId}')) AS binding_string(value)
          WHERE NOT COALESCE(jsonb_typeof(value)='string' AND octet_length(value#>>'{}') BETWEEN 1 AND 4096
            AND value#>>'{}' ~ '[!-~]',false))
        AND ((payload#>'{dispatchPermit,archiveFenceSha256}')='null'::jsonb
          OR (jsonb_typeof(payload#>'{dispatchPermit,archiveFenceSha256}')='string'
            AND payload#>>'{dispatchPermit,archiveFenceSha256}' ~ '^[a-fA-F0-9]{64}$'))
        AND payload#>'{dispatchPermit,providerAttemptObserved}'='false'::jsonb
        AND payload#>>'{dispatchPermit,phase}'='transport_settled'
        AND jsonb_typeof(payload#>'{dispatchPermit,cessation}')='object'
        AND payload#>'{dispatchPermit,cessation}'=jsonb_build_object(
          'kind',payload#>'{dispatchPermit,cessation,kind}','evidenceSha256',payload#>'{dispatchPermit,cessation,evidenceSha256}',
          'owner',payload#>'{dispatchPermit,cessation,owner}','operationId',payload#>'{dispatchPermit,cessation,operationId}',
          'attemptId',payload#>'{dispatchPermit,cessation,attemptId}','permitId',payload#>'{dispatchPermit,cessation,permitId}')
        AND payload#>>'{dispatchPermit,cessation,kind}' IN ('returned','contained','proven_unsent')
        AND jsonb_typeof(payload#>'{dispatchPermit,cessation,evidenceSha256}')='string'
        AND payload#>>'{dispatchPermit,cessation,evidenceSha256}' ~ '^[a-fA-F0-9]{64}$'
        AND payload#>'{dispatchPermit,cessation,owner}'=payload#>'{dispatchPermit,owner}'
        AND payload#>'{dispatchPermit,cessation,operationId}'=payload->'id'
        AND payload#>'{dispatchPermit,cessation,attemptId}'=payload->'attemptId'
        AND payload#>'{dispatchPermit,cessation,permitId}'=payload#>'{dispatchPermit,id}'
        AND NOT EXISTS (
          SELECT 1 FROM (VALUES (payload#>'{dispatchPermit,armedAt}'),(payload#>'{dispatchPermit,settledAt}')) AS native_time(value)
          WHERE NOT COALESCE(jsonb_typeof(value)='string'
            AND value#>>'{}' ~ '^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9]([.][0-9]{1,9})?(Z|[+]00:00)$'
            AND substring(value#>>'{}',9,2) COLLATE "C" <= CASE substring(value#>>'{}',6,2)
              WHEN '02' THEN CASE WHEN substring(value#>>'{}',1,4) ~ '^([0-9]{2}(0[48]|[2468][048]|[13579][26])|([02468][048]|[13579][26])00)$' THEN '29' ELSE '28' END
              WHEN '04' THEN '30' WHEN '06' THEN '30' WHEN '09' THEN '30' WHEN '11' THEN '30' ELSE '31' END,false))
        AND (substring(payload#>>'{dispatchPermit,settledAt}',1,19)
          ||rpad(COALESCE(substring(payload#>>'{dispatchPermit,settledAt}' FROM '[.]([0-9]{1,9})'),'0'),9,'0')) COLLATE "C"
          >= (substring(payload#>>'{dispatchPermit,armedAt}',1,19)
          ||rpad(COALESCE(substring(payload#>>'{dispatchPermit,armedAt}' FROM '[.]([0-9]{1,9})'),'0'),9,'0')) COLLATE "C",false))
    ) ORDER BY ordinal LIMIT 10
), sized AS (SELECT *,sum(octet_length(payload::text)) OVER () AS projection_bytes FROM selected)
SELECT id,item_id,proposal_id,approval_id,status,projection_bytes,
    CASE WHEN projection_bytes<=$3 THEN payload::text ELSE NULL END AS payload
FROM sized ORDER BY ordinal"#;

fn selected_ids(d:&Value,scope:Scope<'_>)->ApiResult<Vec<String>> {
    let mut ids=Vec::new();
    if let Scope::Operation(op)=scope {ids.push(text(op,"id")?.to_owned());}
    let cohort=&d[connection_gate::FIELD]["cohort"];
    if !cohort.is_null() {
        let cohort=cohort.as_array().filter(|c|c.len()<=connection_gate::MAX_MUTATION_PERMITS)
            .ok_or_else(||internal("Dispatch cohort exceeds its admitted bound"))?;
        for captured in cohort {let id=text(captured,"operationId")?.to_owned();if !ids.contains(&id){ids.push(id);}}
    }
    Ok(ids)
}
fn verify_view(d:&Value,ids:&[String])->ApiResult<()> {
    let ops=rows(d,"operations")?;
    if ops.len()>MAX_ROWS||ops.iter().map(|op|op.to_string().len()).sum::<usize>()>MAX_BYTES {
        return Err(internal("Dispatch cohort projection exceeds its byte/row bound"));
    }
    let mut unique=HashSet::new();
    for op in ops {
        if malformed_permit(op){return Err(internal("Malformed dispatch permit prevents a complete cohort"));}
        if !unique.insert(text(op,"id")?){return Err(internal("Duplicate dispatch cohort identity"));}
    }
    if ids.iter().any(|id|!unique.contains(id.as_str())) {return Err(internal("Captured dispatch cohort is incomplete"));}
    Ok(())
}
fn project(d:&Value,scope:Scope<'_>)->ApiResult<Value> {
    let ids=selected_ids(d,scope)?;
    let mut view=metadata(d);
    view.as_object_mut().unwrap().remove("preparationResearch");
    view["operations"]=json!(rows(d,"operations")?.iter().filter(|op|
        ids.iter().any(|id|op["id"]==*id)||op[connection_gate::PERMIT_FIELD]["phase"]=="dispatch_armed"||malformed_permit(op)).collect::<Vec<_>>());
    view["audit"]=json!([]); // one exact native append intent, never history
    verify_view(&view,&ids)?;Ok(view)
}
fn validate_delta(before:&Value,after:&Value)->ApiResult<()> {
    let mut old=before.clone();let mut new=after.clone();
    for field in [connection_gate::FIELD,crate::external_reconciliation::FIELD,"operations","audit"] {
        old.as_object_mut().ok_or_else(||internal("Invalid gate metadata"))?.remove(field);
        new.as_object_mut().ok_or_else(||internal("Invalid gate metadata"))?.remove(field);
    }
    if old!=new{return Err(internal("Connection gate writer changed unrelated metadata"));}
    let old=rows(before,"operations")?;let new=rows(after,"operations")?;
    if old.len()!=new.len(){return Err(internal("Connection gate writer changed operation history"));}
    for (old,new) in old.iter().zip(new) {
        let mut a=old.clone();let mut b=new.clone();
        a.as_object_mut().ok_or_else(||internal("Invalid gate operation"))?.remove(connection_gate::PERMIT_FIELD);
        b.as_object_mut().ok_or_else(||internal("Invalid gate operation"))?.remove(connection_gate::PERMIT_FIELD);
        if a!=b{return Err(internal("Connection gate writer changed operation identity or effects"));}
    }
    connection_gate::validate_change(before,after)?;
    crate::external_reconciliation::validate_change(before,after)?;
    validate_archive_append(before,after)?;
    Ok(())
}

fn validate_archive_append(before:&Value,after:&Value)->ApiResult<()> {
    if !rows(before,"audit")?.is_empty(){return Err(internal("Scoped gate audit is an append intent"));}
    let append=rows(after,"audit")?;let changed=before.get(crate::external_reconciliation::FIELD)!=after.get(crate::external_reconciliation::FIELD);
    if !changed {return if append.is_empty(){Ok(())}else{Err(internal("Unrelated gate audit append"))};}
    if append.len()!=1||append[0].to_string().len()>65536{return Err(internal("Archive installation needs one bounded audit event"));}
    let safety=&after[crate::external_reconciliation::FIELD];let receipt=json!({"status":"installed","replayed":false,"fenceSha256":connection_gate::digest(safety),"gateEpoch":before[connection_gate::FIELD]["gateEpoch"],"archiveManifestHash":safety["safetyFence"]["archiveManifestHash"]});
    let at=&append[0]["createdAt"];if at.as_str().is_none_or(|s|chrono::DateTime::parse_from_rfc3339(s).is_err()){return Err(internal("Archive event timestamp invalid"));}
    let mut expected=receipt.clone();expected["id"]=json!(format!("external-safety:{}",connection_gate::digest(&receipt)));expected["action"]=json!("external_safety.installed");expected["createdAt"]=at.clone();
    if append[0]!=expected||before[connection_gate::FIELD]["state"]!="blocked"||before[connection_gate::FIELD]["finalReceipt"]["providerCapablePermits"]!=0{return Err(internal("Archive audit event differs from exact native installation"));}
    crate::predecessor_recovery::validate_reserved_change(before,after)?;Ok(())
}

impl Database {
    pub(crate) async fn change_connection_gate_observed<T>(&self,scope:Scope<'_>,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)> {
        let _span=crate::performance::Span::new("connection.gate.scoped.total");
        match self {
            Self::Sqlite(_)=>self.change_observed(|d| {
                let before=project(d,scope)?;let mut after=before.clone();let result=f(&mut after)?;
                validate_delta(&before,&after)?;
                for field in [connection_gate::FIELD,crate::external_reconciliation::FIELD] {
                    if let Some(value)=after.get(field){d[field]=value.clone();}
                }
                for op in rows(&after,"operations")? {*crate::row_mut(d,"operations",text(op,"id")?)?=op.clone();}
                for event in rows(&after,"audit")? {if rows(d,"audit")?.iter().any(|r|r["id"]==event["id"]){return Err(internal("Archive audit identity already exists"));}d["audit"].as_array_mut().ok_or_else(||internal("Audit history missing"))?.push(event.clone());}
                Ok(result)
            }).await,
            Self::Postgres{writer,..}=>{
                let mut tx=writer.begin().await?;
                let record=sqlx::query("SELECT account,CASE WHEN octet_length((metadata-'preparationResearch')::text)<=$2 THEN (metadata-'preparationResearch')::text ELSE NULL END AS metadata,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).bind(MAX_BYTES as i64).fetch_one(&mut *tx).await?;
                let raw=record.try_get::<Option<&str>,_>("metadata")?.ok_or_else(||internal("Connection gate metadata byte bound exceeded"))?;
                let mut before=parse(raw)?;
                if record.try_get::<bool,_>("execution_enabled")?||!before.is_object()
                    ||record.try_get::<String,_>("account")?!=before["account"].as_str().unwrap_or("")
                    ||TABLES.iter().any(|t|before.get(*t).is_some()) {return Err(internal("Connection gate workspace identity mismatch"));}
                let ids=selected_ids(&before,scope)?;
                let records=sqlx::query(COHORT_SQL)
                    .bind(WORKSPACE).bind(&ids).bind(MAX_BYTES as i64).fetch_all(&mut *tx).await?;
                if records.len()>MAX_ROWS{return Err(internal("Dispatch cohort exceeds its admitted bound"));}
                let mut bytes=0usize;let mut ops=Vec::new();
                for record in records {
                    let raw=record.try_get::<Option<&str>,_>("payload")?.ok_or_else(||internal("Dispatch cohort byte bound exceeded before transfer"))?;bytes+=raw.len();
                    if bytes>MAX_BYTES{return Err(internal("Dispatch cohort byte bound exceeded"));}
                    let op=parse(raw)?;
                    if record.try_get::<&str,_>("id")?!=text(&op,"id")?{return Err(internal("Dispatch cohort identity mismatch"));}
                    for (column,field) in projection("operations") {
                        if record.try_get::<Option<String>,_>(*column)?.as_deref()!=op[*field].as_str()
                            ||!op[*field].is_null()&&!op[*field].is_string(){return Err(internal("Dispatch cohort projection mismatch"));}
                    }
                    ops.push(op);
                }
                before["operations"]=json!(ops);before["audit"]=json!([]);verify_view(&before,&ids)?;
                let mut after=before.clone();let result=f(&mut after)?;validate_delta(&before,&after)?;
                if before==after {tx.commit().await?;return Ok((result,false));}
                for field in [connection_gate::FIELD,crate::external_reconciliation::FIELD] {
                    if before.get(field)!=after.get(field) {
                        let value=after.get(field).ok_or_else(||internal("Gate control cannot disappear"))?;
                        sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,ARRAY[$2]::text[],$3::jsonb,true) WHERE id=$1")
                            .bind(WORKSPACE).bind(field).bind(value.to_string()).execute(&mut *tx).await?;
                    }
                }
                for (old,new) in rows(&before,"operations")?.iter().zip(rows(&after,"operations")?) {
                    if old!=new {
                        let changed=sqlx::query("UPDATE communityhero.operations SET payload=$3::jsonb WHERE workspace_id=$1 AND id=$2")
                            .bind(WORKSPACE).bind(text(new,"id")?).bind(new.to_string()).execute(&mut *tx).await?;
                        if changed.rows_affected()!=1{return Err(internal("Dispatch cohort operation disappeared"));}
                    }
                }
                for event in rows(&after,"audit")? {
                    let ordinal:i64=sqlx::query_scalar("SELECT COALESCE(max(ordinal)::bigint,-1)+1 FROM communityhero.audit WHERE workspace_id=$1").bind(WORKSPACE).fetch_one(&mut *tx).await?;
                    let ordinal=i32::try_from(ordinal).map_err(|_|internal("Audit ordinal bound exceeded"))?;
                    sqlx::query("INSERT INTO communityhero.audit(workspace_id,id,ordinal,action,ref_id,payload) VALUES($1,$2,$3,$4,$5,$6::jsonb)")
                        .bind(WORKSPACE).bind(text(event,"id")?).bind(ordinal).bind(event["action"].as_str()).bind(event["refId"].as_str()).bind(event.to_string()).execute(&mut *tx).await?;
                }
                tx.commit().await?;Ok((result,true))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_archive_install_append_is_exact_and_does_not_project_history() {
        let mut d=connection_gate::tests::workspace();normalize(&mut d);
        d["audit"]=json!([{"id":"historical","action":"keep","privateEvidence":"outside scoped projection"}]);
        connection_gate::request_close(&mut d,"archive-append","clean_start").unwrap();connection_gate::finalize_close(&mut d,"archive-append").unwrap();
        let binding=d["connectorBinding"].clone();let safety=json!({"mode":"archive_fence","epoch":1,"safetyFence":{"companyId":"likeavto","connectionScope":binding,"archiveManifestHash":"c".repeat(64),"sourceBindingRefs":[{"sourceSha256":"b".repeat(64),"archiveManifestHash":"c".repeat(64),"lineSha256":"d".repeat(64)}],"predicate":"all_nonempty_raw_aliases_702","coverage":{"complete":true,"declaredRows":1648},"sealedAt":crate::now()},"sealedSourceRefs":[{"sha256":"b".repeat(64)}],"archiveRefs":[{"sha256":"c".repeat(64)}],"recipientReservations":[],"quarantineCoverage":{"complete":true,"namespaces":[]}});
        let before=project(&d,Scope::Control).unwrap();assert!(rows(&before,"audit").unwrap().is_empty());
        let mut after=before.clone();let epoch=after[connection_gate::FIELD]["gateEpoch"].as_u64().unwrap();crate::external_reconciliation::install(&mut after,&safety,epoch).unwrap();validate_delta(&before,&after).unwrap();assert_eq!(rows(&after,"audit").unwrap().len(),1);
        for field in ["action","id","fenceSha256","gateEpoch","archiveManifestHash"]{let mut wrong=after.clone();wrong["audit"][0][field]=json!("foreign");assert!(validate_delta(&before,&wrong).is_err());}
        let mut replay=after.clone();replay["audit"]=json!([]);crate::external_reconciliation::install(&mut replay,&safety,epoch).unwrap();validate_delta(&replay,&replay).unwrap();
        let mut missing=after.clone();missing["audit"]=json!([]);assert!(validate_delta(&before,&missing).is_err());
        let mut extra=after.clone();let event=extra["audit"][0].clone();extra["audit"].as_array_mut().unwrap().push(event);assert!(validate_delta(&before,&extra).is_err());
        assert_eq!(d["audit"][0]["id"],"historical");
    }
    fn fixture()->Value {
        let mut d=connection_gate::tests::workspace();normalize(&mut d);
        let mut active=connection_gate::tests::operation(&d,1);active["id"]=json!("active-other");
        let mut settled=connection_gate::tests::operation(&d,2);settled["id"]=json!("old-settled");
        let mut requested=connection_gate::tests::operation(&d,3);requested["id"]=json!("requested");
        d["operations"]=json!([active.clone(),settled.clone(),requested,
            {"id":"cold","status":"succeeded","evidence":"keep outside selected view"}]);
        connection_gate::prearm(&mut d,&active).unwrap();
        let permit=connection_gate::prearm(&mut d,&settled).unwrap();
        connection_gate::request_close(&mut d,"storage-fixture-drain","handoff").unwrap();
        connection_gate::settle(&mut d,&connection_gate::tests::witness(&permit,connection_gate::CessationKind::Returned)).unwrap();
        d["operations"][0]["status"]=json!("unknown");d["operations"][1]["status"]=json!("unknown");d
    }
    #[test]
    fn closing_projection_keeps_other_armed_and_captured_settled_permits() {
        let d=fixture();let selected=project(&d,Scope::Operation(&d["operations"][2])).unwrap();
        assert_eq!(rows(&selected,"operations").unwrap().iter().map(|op|op["id"].as_str().unwrap()).collect::<Vec<_>>(),
            vec!["active-other","old-settled","requested"]);
        let mut missing=d.clone();missing["operations"].as_array_mut().unwrap().remove(1);
        assert!(project(&missing,Scope::Control).is_err(),"missing captured settled permit cannot prove final zero");
        let mut foreign=d;foreign["operations"][1]["id"]=json!("active-other");
        assert!(project(&foreign,Scope::Control).is_err(),"duplicate cohort identity is not a complete set");
    }
    #[test]
    fn gate_writer_cannot_append_or_alter_external_effects_or_unrelated_metadata() {
        let before=project(&fixture(),Scope::Control).unwrap();
        for fault in ["append","target","settings","status"] {
            let mut after=before.clone();
            match fault {
                "append"=>after["operations"].as_array_mut().unwrap().push(json!({"id":"new"})),
                "settings"=>after["settings"]["invented"]=json!(true),
                "target"=>after["operations"][0]["target"]=json!({"itemId":"other"}),
                _=>after["operations"][0]["status"]=json!("succeeded"),
            }
            assert!(validate_delta(&before,&after).is_err(),"{fault}");
        }
    }
    #[test]
    fn uncaptured_settled_permit_corruption_cannot_disappear_from_final_zero() {
        let mut d=fixture();let active=d["operations"][0][connection_gate::PERMIT_FIELD].clone();
        connection_gate::settle(&mut d,&connection_gate::tests::witness(&active,connection_gate::CessationKind::Returned)).unwrap();
        connection_gate::finalize_close(&mut d,"storage-fixture-drain").unwrap();connection_gate::fixture_open(&mut d).unwrap();
        assert!(rows(&project(&d,Scope::Control).unwrap(),"operations").unwrap().is_empty(),"actual native settled history may be omitted");
        for field in ["version","id","operationId","attemptId","account","connectionBinding","gateEpoch","owner",
            "archiveFenceSha256","providerAttemptObserved","armedAt","phase","cessation","settledAt"] {
            let mut corrupt=d.clone();corrupt["operations"][1][connection_gate::PERMIT_FIELD].as_object_mut().unwrap().remove(field);
            assert!(project(&corrupt,Scope::Control).is_err(),"missing {field} must not prove an empty cohort");
        }
        for value in [Value::Null,json!({"phase":"transport_settled"}),json!({"phase":"foreign"})] {
            let mut corrupt=d.clone();corrupt["operations"][1][connection_gate::PERMIT_FIELD]=value;
            assert!(project(&corrupt,Scope::Control).is_err());
        }
        let mut corrupt=d.clone();corrupt["operations"][1][connection_gate::PERMIT_FIELD]["owner"]=json!({});
        corrupt["operations"][1][connection_gate::PERMIT_FIELD]["cessation"]["owner"]=json!({});
        assert!(project(&corrupt,Scope::Control).is_err(),"matching owner:{{}} cannot make a malformed original disappear");
        for (field,value) in [("providerAttemptObserved",json!(true)),("archiveFenceSha256",json!("not-a-hash")),
            ("armedAt",json!("not-a-time")),("settledAt",json!("2026-02-30T00:00:00Z"))] {
            let mut corrupt=d.clone();corrupt["operations"][1][connection_gate::PERMIT_FIELD][field]=value;
            assert!(project(&corrupt,Scope::Control).is_err(),"invalid {field}");
        }
        let mut corrupt=d.clone();corrupt["operations"][1][connection_gate::PERMIT_FIELD]["armedAt"]=json!("2026-10-07T00:00:01Z");
        corrupt["operations"][1][connection_gate::PERMIT_FIELD]["settledAt"]=json!("2026-10-07T00:00:00Z");
        assert!(project(&corrupt,Scope::Control).is_err(),"reversed timestamps cannot prove final zero");
    }

    // Corrupt only the disposable, identity-guarded fixture. Production writers
    // must never bypass the immutable permit reducer to inject these states.
    async fn pg_fixture_payload(writer:&PgPool,id:&str,value:&Value) {
        let mut tx=writer.begin().await.unwrap();
        assert_eq!(sqlx::query("UPDATE communityhero.operations SET payload=$3::jsonb WHERE workspace_id=$1 AND id=$2")
            .bind(WORKSPACE).bind(id).bind(value.to_string()).execute(&mut *tx).await.unwrap().rows_affected(),1);
        tx.commit().await.unwrap();
    }
    async fn pg_fixture_insert(writer:&PgPool,value:&Value,ordinal:i32) {
        let mut tx=writer.begin().await.unwrap();
        sqlx::query("INSERT INTO communityhero.operations(workspace_id,id,ordinal,item_id,proposal_id,approval_id,status,payload) VALUES($1,$2,$3,$4,$5,$6,$7,$8::jsonb)")
            .bind(WORKSPACE).bind(text(value,"id").unwrap()).bind(ordinal)
            .bind(value["itemId"].as_str()).bind(value["proposalId"].as_str()).bind(value["approvalId"].as_str())
            .bind(value["status"].as_str()).bind(value.to_string()).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
    #[tokio::test]
    #[ignore="ROOT only: pristine isolated communityhero_writer_v51_test_ PostgreSQL fixture; run alone"]
    async fn postgres_native_cohort_sql_keeps_corruption_and_bounds_transfer() {
        let db=crate::storage::writer_v51_fixture_db().await;
        let Database::Postgres{writer,..}=&db else {unreachable!()};
        let d=fixture();
        let mut tx=writer.begin().await.unwrap();
        sqlx::query("UPDATE communityhero.workspaces SET metadata=$2::jsonb WHERE id=$1")
            .bind(WORKSPACE).bind(metadata(&d).to_string()).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        for (ordinal,op) in rows(&d,"operations").unwrap().iter().enumerate() {pg_fixture_insert(writer,op,ordinal as i32).await;}
        let expected=project(&d,Scope::Operation(&d["operations"][2])).unwrap();
        let (_,changed)=db.change_connection_gate_observed(Scope::Operation(&d["operations"][2]),|view| {
            assert_eq!(*view,expected,"actual COHORT_SQL includes other armed/captured settled/requested rows and omits cold history");Ok(())
        }).await.unwrap();assert!(!changed);

        let active=d["operations"][0][connection_gate::PERMIT_FIELD].clone();
        let (_,changed)=db.change_connection_gate_observed(Scope::Control,|view| {
            connection_gate::settle(view,&connection_gate::tests::witness(&active,connection_gate::CessationKind::Returned))?;
            connection_gate::finalize_close(view,"storage-fixture-drain")?;connection_gate::fixture_open(view)
        }).await.unwrap();assert!(changed);
        let (_,changed)=db.change_connection_gate_observed(Scope::Control,|view| {
            assert!(rows(view,"operations")?.is_empty(),"actual SQL omits valid uncaptured native settled history");Ok(())
        }).await.unwrap();assert!(!changed);
        let original=d["operations"][1].clone();
        let mut corruptions=vec![];
        for field in ["version","id","operationId","attemptId","account","connectionBinding","gateEpoch","owner",
            "archiveFenceSha256","providerAttemptObserved","armedAt","phase","cessation","settledAt"] {
            let mut broken=original.clone();broken[connection_gate::PERMIT_FIELD].as_object_mut().unwrap().remove(field);
            corruptions.push((format!("missing {field}"),broken));
        }
        let mut broken=original.clone();broken[connection_gate::PERMIT_FIELD]["owner"]=json!({});
        broken[connection_gate::PERMIT_FIELD]["cessation"]["owner"]=json!({});corruptions.push(("matching owner:{}".into(),broken));
        for (field,value) in [("providerAttemptObserved",json!(true)),("archiveFenceSha256",json!("not-a-hash")),
            ("armedAt",json!("not-a-time")),("settledAt",json!("2026-02-30T00:00:00Z")),("gateEpoch",json!(0))] {
            let mut broken=original.clone();broken[connection_gate::PERMIT_FIELD][field]=value;
            corruptions.push((format!("invalid {field}"),broken));
        }
        let mut broken=original.clone();broken[connection_gate::PERMIT_FIELD]["connectionBinding"]["revision"]=json!(2);
        corruptions.push(("original operation binding differs".into(),broken));
        let mut broken=original.clone();broken["id"]=json!("foreign-payload-id");
        corruptions.push(("payload operation identity differs".into(),broken));
        for (field,value) in [("epoch",json!(0)),("releaseSha256",json!("A".repeat(64))),("extra",json!(true))] {
            let mut broken=original.clone();broken[connection_gate::PERMIT_FIELD]["owner"][field]=value;
            broken[connection_gate::PERMIT_FIELD]["cessation"]["owner"]=broken[connection_gate::PERMIT_FIELD]["owner"].clone();
            corruptions.push((format!("invalid exact owner {field}"),broken));
        }
        let mut broken=original.clone();broken[connection_gate::PERMIT_FIELD]["armedAt"]=json!("2026-10-07T00:00:00.01+00:00");
        broken[connection_gate::PERMIT_FIELD]["settledAt"]=json!("2026-10-07T00:00:00.000000001Z");
        corruptions.push(("reversed canonical nanosecond timestamps".into(),broken));
        let clean_metadata:String=sqlx::query_scalar("SELECT metadata::text FROM communityhero.workspaces WHERE id=$1")
            .bind(WORKSPACE).fetch_one(writer).await.unwrap();
        for (label,broken) in corruptions {
            assert!(!connection_gate::valid_permit(&broken),"fixture must corrupt {label}");
            pg_fixture_payload(writer,"old-settled",&broken).await;
            let mut invoked=false;
            let result=db.change_connection_gate_observed(Scope::Control,|_| {invoked=true;Ok(())}).await;
            assert!(result.is_err(),"actual COHORT_SQL must retain and reject {label}");assert!(!invoked);
            let preserved:String=sqlx::query_scalar("SELECT payload::text FROM communityhero.operations WHERE workspace_id=$1 AND id='old-settled'")
                .bind(WORKSPACE).fetch_one(writer).await.unwrap();
            assert_eq!(parse(&preserved).unwrap(),broken,"rejection cannot rewrite the original malformed evidence");
            let unchanged:String=sqlx::query_scalar("SELECT metadata::text FROM communityhero.workspaces WHERE id=$1")
                .bind(WORKSPACE).fetch_one(writer).await.unwrap();assert_eq!(unchanged,clean_metadata);
            pg_fixture_payload(writer,"old-settled",&original).await;
        }
        // A valid native identity still cannot hide relational projection drift
        // when the original operation is explicitly selected for settlement.
        sqlx::query("UPDATE communityhero.operations SET status='succeeded' WHERE workspace_id=$1 AND id='old-settled'")
            .bind(WORKSPACE).execute(writer).await.unwrap();
        assert!(db.change_connection_gate_observed(Scope::Operation(&original),|_|Ok(())).await.is_err());
        sqlx::query("UPDATE communityhero.operations SET status='unknown' WHERE workspace_id=$1 AND id='old-settled'")
            .bind(WORKSPACE).execute(writer).await.unwrap();
        let mut broken=original.clone();broken["itemId"]=json!(7);pg_fixture_payload(writer,"old-settled",&broken).await;
        assert!(db.change_connection_gate_observed(Scope::Operation(&original),|_|Ok(())).await.is_err());
        pg_fixture_payload(writer,"old-settled",&original).await;

        // Each overflow row gets a real prearm in its own synthetic workspace;
        // this isolated DB corruption models more rows than the transfer cap.
        let mut overflow_ids=vec![];
        for n in 0..MAX_ROWS+1 {
            let mut workspace=connection_gate::tests::workspace();let mut op=connection_gate::tests::operation(&workspace,n);
            op["id"]=json!(format!("overflow-{n}"));op["attemptId"]=json!(format!("overflow-attempt-{n}"));
            crate::list_mut(&mut workspace,"operations").push(op.clone());connection_gate::prearm(&mut workspace,&op).unwrap();
            let op=workspace["operations"][0].clone();overflow_ids.push(text(&op,"id").unwrap().to_owned());
            pg_fixture_insert(writer,&op,4+n as i32).await;
        }
        let records=sqlx::query(COHORT_SQL).bind(WORKSPACE).bind(Vec::<String>::new()).bind(MAX_BYTES as i64)
            .fetch_all(writer).await.unwrap();assert_eq!(records.len(),MAX_ROWS+1,"LIMIT10 exposes overflow instead of returning false zero");
        let mut invoked=false;
        assert!(db.change_connection_gate_observed(Scope::Control,|_|{invoked=true;Ok(())}).await.is_err());assert!(!invoked);
        sqlx::query("DELETE FROM communityhero.operations WHERE workspace_id=$1 AND id=ANY($2::text[])")
            .bind(WORKSPACE).bind(&overflow_ids).execute(writer).await.unwrap();
        let mut workspace=connection_gate::tests::workspace();let mut oversized=connection_gate::tests::operation(&workspace,99);
        oversized["id"]=json!("oversized-native");crate::list_mut(&mut workspace,"operations").push(oversized.clone());
        connection_gate::prearm(&mut workspace,&oversized).unwrap();oversized=workspace["operations"][0].clone();
        oversized["syntheticReceiptPadding"]=json!("x".repeat(MAX_BYTES+1024));pg_fixture_insert(writer,&oversized,4).await;
        let records=sqlx::query(COHORT_SQL).bind(WORKSPACE).bind(Vec::<String>::new()).bind(MAX_BYTES as i64)
            .fetch_all(writer).await.unwrap();assert_eq!(records.len(),1);
        assert!(records[0].try_get::<i64,_>("projection_bytes").unwrap()>MAX_BYTES as i64);
        assert!(records[0].try_get::<Option<String>,_>("payload").unwrap().is_none(),"oversized bytes must be withheld before client transfer");
        let mut invoked=false;
        assert!(db.change_connection_gate_observed(Scope::Control,|_|{invoked=true;Ok(())}).await.is_err());assert!(!invoked);
        db.close().await;
    }
}
