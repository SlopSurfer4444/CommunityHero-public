//! Targeted durable local admission receipts. Reads never acquire the writer
//! lease or load comment, approval, job or unrelated audit history.
use super::*;
use serde_json::json;

pub(super) fn validate_prepare_receipt(receipt:&Value,job:&Value,workspace:&Value)->ApiResult<()> {
    let fields=receipt.as_object().ok_or_else(||internal("Invalid preparation admission receipt"))?;
    let allowed=["id","action","refId","requestId","kind","account","actorId","actorRole",
        "requestHash","payloadHash","result","createdAt"];
    let attribution=["conductorRunId","grantGeneration"];
    let attributed=attribution.iter().any(|key|fields.contains_key(*key));
    let key=text(receipt,"requestId")?;
    let hash=text(receipt,"requestHash")?;
    let payload_hash=text(receipt,"payloadHash")?;
    let result=receipt["result"].as_object().ok_or_else(||internal("Invalid preparation admission result"))?;
    if fields.len()!=allowed.len()+if attributed{attribution.len()}else{0}
        || allowed.iter().any(|key|!fields.contains_key(*key))
        || fields.keys().any(|key|!allowed.contains(&key.as_str())&&!attribution.contains(&key.as_str()))
        || (attributed&&attribution.iter().any(|key|!fields.contains_key(*key)))
        || key.trim().is_empty()
        || receipt["id"]!=crate::local_admission::receipt_id("prepare",key)
        || receipt["action"]!="local_admission.committed" || receipt["kind"]!="prepare"
        || receipt["refId"]!=key
        || receipt["account"]!=crate::accounts::Profile::from_workspace(workspace)?.key()
        || text(receipt,"actorId")?.trim().is_empty() || text(receipt,"actorRole")?.trim().is_empty()
        || text(receipt,"createdAt")?.trim().is_empty()
        || hash.len()!=64 || !hash.bytes().all(|byte|byte.is_ascii_hexdigit())
        || payload_hash.len()!=64 || !payload_hash.bytes().all(|byte|byte.is_ascii_hexdigit())
        || result.len()!=if result.contains_key("scopeReservation"){4}else{3}
        || result.keys().any(|key|!matches!(key.as_str(),"jobId"|"requestId"|"replayed"|"scopeReservation"))
        || receipt["result"]["jobId"]!=job["id"] || receipt["result"]["requestId"]!=key
        || receipt["result"]["replayed"]!=false {
        return Err(internal("Preparation scheduling appended an invalid admission receipt"));
    }
    if let Some(ctx)=crate::conductor_authority::current_context(){
        if !attributed||receipt["conductorRunId"]!=ctx.run_id||receipt["grantGeneration"].as_u64()!=Some(ctx.lease_generation)
            ||job["conductorRunId"]!=receipt["conductorRunId"]||job["grantGeneration"]!=receipt["grantGeneration"]
            ||receipt["actorId"]!=ctx.actor.id||receipt["actorRole"]!=ctx.actor.role {
            return Err(internal("Preparation admission receipt conductor attribution changed"));
        }
        crate::conductor_authority::fence_admission(workspace,"prepare",
            job["selectedItemIds"].as_array().ok_or_else(||internal("Preparation recipients missing"))?)?;
    }else if attributed||attribution.iter().any(|key|job.get(*key).is_some()){
        return Err(internal("Preparation admission receipt requires its conductor context"));
    }
    if let Some(proof)=result.get("scopeReservation") {
        let reservation=job.get("scopeReservation").ok_or_else(||internal("Preparation admission reservation missing"))?;
        if proof!=&json!({"version":1,"ownerJobId":reservation["ownerJobId"],"keysDigest":reservation["keysDigest"]}) {
            return Err(internal("Preparation admission reservation proof changed"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path="storage_local_admission_tests.rs"]
mod tests;

impl Database {
    /// The deterministic audit identity is the admission key. Validate the
    /// discriminator before exposing a record; a corrupt/colliding row is not
    /// equivalent to an absent receipt and must not permit a second admission.
    pub(crate) async fn read_local_admission_receipt(&self,kind:&str,key:&str)->ApiResult<Option<Value>> {
        let mut identities=vec![crate::local_admission::receipt_id(kind,key)];
        if kind=="execute" {identities.extend((1..=crate::local_admission::MAX_REJECTED_EVALUATIONS as u64).map(|index|crate::local_admission::negative_id(kind,key,index)));}
        let (account,records)=match self {
            Self::Sqlite(pool)=>{
                let record=sqlx::query(r#"SELECT json_extract(w.payload,'$.account') AS account,
                    (SELECT json_group_array(json(value)) FROM
                     (SELECT a.value AS value FROM json_each(w.payload,'$.audit') a
                      WHERE json_extract(a.value,'$.id') IN (SELECT value FROM json_each(?)) OR
                       (json_extract(a.value,'$.action') IN ('local_admission.committed','local_admission.rejected')
                        AND json_extract(a.value,'$.kind')=? AND json_extract(a.value,'$.requestId')=?)
                      LIMIT 18)) AS receipts FROM workspace w WHERE w.id=1"#)
                    .bind(json!(identities).to_string()).bind(kind).bind(key).fetch_one(pool).await?;
                (record.try_get::<String,_>("account")?,parse(record.try_get::<&str,_>("receipts")?)?)
            },
            Self::Postgres{reader,..}=>{
                let record=sqlx::query(r#"SELECT w.account,w.execution_enabled,
                    (jsonb_typeof(w.metadata->'account')='string' AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
                    COALESCE((SELECT jsonb_agg(jsonb_build_object('id',a.id,'action',a.action,'refId',a.ref_id,'payload',a.payload))
                      FROM (SELECT a.id,a.action,a.ref_id,a.payload FROM communityhero.audit a WHERE a.workspace_id=w.id
                       AND (a.id=ANY($2::text[]) OR a.payload->>'id'=ANY($2::text[]) OR
                        ((a.action IN ('local_admission.committed','local_admission.rejected') OR
                          a.payload->>'action' IN ('local_admission.committed','local_admission.rejected'))
                         AND a.payload->>'kind'=$3 AND a.payload->>'requestId'=$4))
                       ORDER BY a.ordinal LIMIT 18) a),'[]'::jsonb)::text AS receipts
                    FROM communityhero.workspaces w WHERE w.id=$1"#)
                    .bind(WORKSPACE).bind(&identities).bind(kind).bind(key).fetch_one(reader).await?;
                if record.try_get::<bool,_>("execution_enabled")? || !record.try_get::<bool,_>("identity_valid")? {
                    return Err(internal("Local admission workspace identity mismatch"));
                }
                let projected=parse(record.try_get::<&str,_>("receipts")?)?;
                let mut receipts=Vec::new();
                for entry in projected.as_array().ok_or_else(||internal("Invalid admission read projection"))? {
                    let receipt=&entry["payload"];
                    if entry["id"]!=receipt["id"]||entry["action"]!=receipt["action"]||entry["refId"]!=receipt["refId"] {
                        return Err(internal("Local admission receipt projection mismatch"));
                    }
                    receipts.push(receipt.clone());
                }
                (record.try_get::<String,_>("account")?,json!(receipts))
            },
        };
        let audit=records.as_array().ok_or_else(||internal("Invalid admission history"))?;
        if audit.len()>crate::local_admission::MAX_REJECTED_EVALUATIONS+1 {
            return Err(internal("Local admission history exceeds bounded projection"));
        }
        let view=json!({"account":account,"audit":records});
        // A committed positive always wins. A negative is evidence of one
        // rejected evaluation, never permission to repeat a provider action.
        let receipt=if let Some(receipt)=crate::local_admission::find_receipt(&view,kind,key)? {receipt}
            else if let Some(receipt)=crate::local_admission::find_rejection(&view,kind,key)? {receipt}
            else {return Ok(None);};
        if receipt["account"]!=crate::accounts::Profile::from_workspace(&view)?.key() {
            return Err(internal("Local admission receipt identity mismatch"));
        }
        Ok(Some(receipt.clone()))
    }
}
