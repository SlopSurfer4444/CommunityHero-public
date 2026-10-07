//! Extra source-admission-only projection of settled assistant checkpoints.
//! This is a read-only witness view. Never use it for review, recovery, dispatch,
//! export, or model settlement. Every job row and all control/provenance fields
//! survive; persistence remains governed by the source snapshot write whitelist.
use serde_json::{Map, Value};
use std::collections::HashSet;

// Preserve runMetadata, editorialEvidence, factDependencies, unknown extensions,
// and every attempt/digest. Only repeated known model-output bodies are omitted.
const BODY_FIELDS: &[&str] = &["text", "sources", "assessments", "proposals"];
const STAGES: &[&str] = &["first", "review", "reviewChunks", "groupAdmission"];

fn hash(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.len() == 64
        && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}
fn result_shape(value: &Value) -> bool {
    value.is_object()
        && value.get("text").is_none_or(Value::is_string)
        && ["sources", "assessments", "proposals"].iter().all(|key|
            value.get(*key).is_none_or(Value::is_array))
        && value.get("runMetadata").is_none_or(Value::is_object)
}
fn settled_stage(value: &Value) -> bool {
    value.is_null() || (value.is_object() && value["status"] == "completed"
        && result_shape(&value["result"]))
}
fn settled_chunks(value: &Value) -> bool {
    value.is_null() || (value.is_object() && value["status"] == "completed"
        && value["chunks"].as_array().is_some_and(|chunks| !chunks.is_empty()
            && chunks.iter().all(|chunk| chunk.is_object()
                && result_shape(&chunk["result"])
                && chunk["attempts"].as_array().is_some_and(|attempts| !attempts.is_empty()
                    && attempts.iter().all(|attempt| attempt.is_object()
                        && attempt["status"] == "completed" && hash(&attempt["resultDigest"]))))))
}

/// A strict subset of the existing source_job assistant predicate. A referenced
/// job, unknown lineage set, legacy bundle, uncertain attempt, malformed shape,
/// or new stage kind takes the existing full-checkpoint fallback.
fn eligible(job: &Value, protected: Option<&HashSet<String>>) -> bool {
    let Some(protected) = protected else { return false; };
    let bundle = &job["prepareBundle"];
    let stages = &job["preparationStages"];
    job.is_object() && job["kind"] == "assistant" && job["status"] == "completed"
        && job["id"].as_str().is_some_and(|id| !id.is_empty() && !protected.contains(id))
        && bundle.is_object() && bundle["version"] == 1 && bundle["request"].is_object()
        && hash(&bundle["digest"]) && hash(&bundle["dependencyDigest"]) && bundle["itemIds"].is_array()
        && job["recovery"].is_null() && job.get("result").is_none_or(Value::is_object)
        && (job["scopeModelAttempt"].is_null() || (job["scopeModelAttempt"].is_object()
            && job["scopeModelAttempt"]["status"] == "completed"))
        && stages.as_object().is_some_and(|fields|
            fields.keys().all(|key| STAGES.contains(&key.as_str()))
            && settled_stage(&stages["first"]) && settled_stage(&stages["review"])
            && settled_chunks(&stages["reviewChunks"])
            && (stages["groupAdmission"].is_null() || stages["groupAdmission"].as_array()
                .is_some_and(|groups| groups.iter().all(Value::is_object)))
            && ["first", "review", "reviewChunks"].iter().any(|key| stages[*key].is_object()))
}

// A complete source-maintenance inventory, not a job/recovery/model view.
// Global legacy assessment recovery reads these exact fields even without an
// item->job pointer. Only strictly settled known checkpoints may use controls;
// every malformed/uncertain/new repair or material shape retains its full job.
const SOURCE_CONTROL_FIELDS: &[&str] = &["id", "kind", "purpose", "status", "refId",
    "prepareOutcome", "autoPreparationInputs", "account", "accountId", "connectorBinding"];
const MATERIAL_REPAIR_FIELDS: &[&str] = &["mandatoryMaterialContract", "postContextBundle",
    "materialReadiness", "materialArtifacts", "modelMaterialReceipt", "modelMaterialReceipts", "answeringRepairPlan",
    "originatingAnsweringAttemptId", "requestingPaidAttemptId", "roundOrdinal", "repairPaidIntent",
    "frameNeed", "framePlan", "frameResult", "frameLease", "videoFrameNeeds", "frameNeedOutcome",
    "sourceOriginJobId", "nativeSourceOriginJobId", "sourceProofRef", "videoSpeechAssetPin", "repairMergeReceipt", "pendingWorkResume"];

pub(super) fn requires_full_material_job(job:&Value)->bool {
    MATERIAL_REPAIR_FIELDS.iter().any(|key|job.get(*key).is_some())
}
pub(super) fn material_fields_sql()->String {
    MATERIAL_REPAIR_FIELDS.iter().map(|key|format!("'{key}'")).collect::<Vec<_>>().join(",")
}

pub(super) fn source_control(job:&Value)->(Value,bool) {
    let empty=HashSet::new();
    let compact=eligible(job,Some(&empty))
        && job["prepareBundle"]["id"].as_str().is_some_and(|id|!id.is_empty())
        && MATERIAL_REPAIR_FIELDS.iter().all(|key|job.get(*key).is_none());
    let mut control=Map::new();
    if let Some(fields)=job.as_object() {
        for key in SOURCE_CONTROL_FIELDS {
            if let Some(value)=fields.get(*key) {control.insert((*key).to_owned(),value.clone());}
        }
        if let Some(bundle)=fields.get("prepareBundle") {
            let selected=if let Some(fields)=bundle.as_object(){
                Value::Object(fields.iter().filter(|(key,_)|
                    ["id","version","digest","dependencyDigest","itemIds"].contains(&key.as_str()))
                    .map(|(key,value)|(key.clone(),value.clone())).collect())
            }else{bundle.clone()};
            control.insert("prepareBundle".to_owned(),selected);
        }
    }
    (Value::Object(control),!compact)
}

// Reuse the exact settled predicate without constructing a large projected
// body merely to prove eligibility. Flags stay outside canonical job payloads.
pub(super) fn source_control_sql()->String {
    let eligible=eligibility_sql();
    let fields=SOURCE_CONTROL_FIELDS.iter().map(|key|format!("'{key}'")).collect::<Vec<_>>().join(",");
    let repair=MATERIAL_REPAIR_FIELDS.iter().map(|key|format!("'{key}'")).collect::<Vec<_>>().join(",");
    format!("(jsonb_build_object('fullRequired',NOT COALESCE(({eligible}) \
        AND jsonb_typeof(payload#>'{{prepareBundle,id}}')='string' AND length(payload#>>'{{prepareBundle,id}}')>0 \
        AND NOT (payload ?| ARRAY[{repair}]),false), \
        'job',COALESCE((SELECT jsonb_object_agg(e.key,e.value) FROM jsonb_each( \
            CASE WHEN jsonb_typeof(payload)='object' THEN payload ELSE '{{}}'::jsonb END) e \
            WHERE e.key IN ({fields})),'{{}}'::jsonb) || CASE WHEN payload ? 'prepareBundle' THEN \
            jsonb_build_object('prepareBundle',CASE WHEN jsonb_typeof(payload->'prepareBundle')='object' THEN \
              COALESCE((SELECT jsonb_object_agg(b.key,b.value) FROM jsonb_each(payload->'prepareBundle') b \
                WHERE b.key IN ('id','version','digest','dependencyDigest','itemIds')),'{{}}'::jsonb) \
            ELSE payload->'prepareBundle' END) ELSE '{{}}'::jsonb END) \
        )")
}

fn without(value: &Value, omitted: &[&str]) -> Value {
    Value::Object(value.as_object().expect("projection shape checked").iter()
        .filter(|(key, _)| !omitted.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone())).collect())
}
fn stage(value: &Value) -> Value {
    let mut projected = Map::new();
    for (key, value) in value.as_object().expect("settled stage checked") {
        projected.insert(key.clone(), if key == "result" { without(value, BODY_FIELDS) } else { value.clone() });
    }
    Value::Object(projected)
}
fn chunks(value: &Value) -> Value {
    let mut projected = Map::new();
    for (key, value) in value.as_object().expect("settled chunks checked") {
        let value = if key == "chunks" {
            Value::Array(value.as_array().unwrap().iter().map(stage).collect())
        } else { value.clone() };
        projected.insert(key.clone(), value);
    }
    Value::Object(projected)
}

/// Returns None to select the unchanged source_job fallback. Build selectively
/// instead of cloning the large original request/results before deleting them.
pub(super) fn project_assistant(job: &Value, protected: Option<&HashSet<String>>) -> Option<Value> {
    if !eligible(job, protected) { return None; }
    let mut projected = Map::new();
    for (key, value) in job.as_object().unwrap() {
        if key == "result" && value.is_object() { continue; }
        let value = match key.as_str() {
            "prepareBundle" => without(value, &["request"]),
            "preparationStages" => Value::Object(value.as_object().unwrap().iter().map(|(key, value)| {
                let value = if value.is_null() { value.clone() }
                    else if key == "reviewChunks" { chunks(value) }
                    else if key == "first" || key == "review" { stage(value) }
                    else { value.clone() };
                (key.clone(), value)
            }).collect()),
            _ => value.clone(),
        };
        projected.insert(key.clone(), value);
    }
    Some(Value::Object(projected))
}

fn sql_null(input: &str) -> String { format!("({input} IS NULL OR {input}='null'::jsonb)") }
fn sql_result(input: &str) -> String {
    format!("jsonb_typeof({input})='object' AND NOT EXISTS (SELECT 1 FROM jsonb_each({input}) r WHERE \
        (r.key='text' AND jsonb_typeof(r.value)<>'string') OR \
        (r.key IN ('sources','assessments','proposals') AND jsonb_typeof(r.value)<>'array') OR \
        (r.key='runMetadata' AND jsonb_typeof(r.value)<>'object'))")
}
fn sql_stage(input: &str) -> String {
    let result = sql_result(&format!("({input}->'result')"));
    // CASE shields set-returning JSON functions from malformed input regardless
    // of PostgreSQL's boolean expression evaluation order.
    format!("({} OR CASE WHEN jsonb_typeof({input})='object' AND \
        jsonb_typeof({input}->'result')='object' THEN {input}->>'status'='completed' AND ({result}) ELSE false END)", sql_null(input))
}
fn strip_result(input: &str) -> String {
    format!("jsonb_set({input},'{{result}}',({input}->'result')-'text'-'sources'-'assessments'-'proposals')")
}

/// Wrap the previous media/assistant payload expression. Bindings stay exactly
/// $1 workspace, $2 protected IDs, $3 reference-set validity. Input is trusted
/// fixed SQL authored by storage_source_snapshot, never stored/user text.
fn eligibility_sql() -> String {
    let stages = "(payload->'preparationStages')";
    let first = format!("({stages}->'first')");
    let review = format!("({stages}->'review')");
    let chunks = format!("({stages}->'reviewChunks')");
    let group = format!("({stages}->'groupAdmission')");
    let chunk_result = sql_result("(c.value->'result')");
    let settled_chunks = format!("({null} OR CASE WHEN jsonb_typeof({chunks})='object' \
        AND jsonb_typeof({chunks}->'chunks')='array' THEN {chunks}->>'status'='completed' \
        AND jsonb_array_length({chunks}->'chunks')>0 AND NOT EXISTS \
        (SELECT 1 FROM jsonb_array_elements({chunks}->'chunks') c(value) WHERE NOT \
            COALESCE(CASE WHEN jsonb_typeof(c.value)='object' AND jsonb_typeof(c.value->'result')='object' \
                AND jsonb_typeof(c.value->'attempts')='array' THEN ({chunk_result}) \
                AND jsonb_array_length(c.value->'attempts')>0 AND NOT EXISTS \
                (SELECT 1 FROM jsonb_array_elements(c.value->'attempts') a(value) WHERE NOT COALESCE( \
                    jsonb_typeof(a.value)='object' AND a.value->>'status'='completed' \
                    AND jsonb_typeof(a.value->'resultDigest')='string' AND a.value->>'resultDigest' ~ '^[a-f0-9]{{64}}$',false)) \
                ELSE false END,false)) ELSE false END)", null=sql_null(&chunks));
    let groups = format!("({null} OR CASE WHEN jsonb_typeof({group})='array' THEN NOT EXISTS \
        (SELECT 1 FROM jsonb_array_elements({group}) g(value) WHERE jsonb_typeof(g.value)<>'object') ELSE false END)", null=sql_null(&group));
    let keys = STAGES.iter().map(|key| format!("'{key}'")).collect::<Vec<_>>().join(",");
    format!("($3::boolean AND jsonb_typeof(payload)='object' \
        AND payload->>'kind'='assistant' AND payload->>'status'='completed' \
        AND jsonb_typeof(payload->'id')='string' AND length(payload->>'id')>0 AND NOT (payload->>'id'=ANY($2::text[])) \
        AND jsonb_typeof(payload->'prepareBundle')='object' AND jsonb_typeof(payload#>'{{prepareBundle,request}}')='object' \
        AND jsonb_typeof(payload#>'{{prepareBundle,version}}')='number' AND payload#>>'{{prepareBundle,version}}'='1' \
        AND jsonb_typeof(payload#>'{{prepareBundle,digest}}')='string' AND payload#>>'{{prepareBundle,digest}}' ~ '^[a-f0-9]{{64}}$' \
        AND jsonb_typeof(payload#>'{{prepareBundle,dependencyDigest}}')='string' AND payload#>>'{{prepareBundle,dependencyDigest}}' ~ '^[a-f0-9]{{64}}$' \
        AND jsonb_typeof(payload#>'{{prepareBundle,itemIds}}')='array' AND {recovery} \
        AND (NOT (payload ? 'result') OR jsonb_typeof(payload->'result')='object') \
        AND ({attempt_null} OR (jsonb_typeof(payload->'scopeModelAttempt')='object' AND payload#>>'{{scopeModelAttempt,status}}'='completed')) \
        AND CASE WHEN jsonb_typeof({stages})='object' THEN \
            NOT EXISTS (SELECT 1 FROM jsonb_each({stages}) s WHERE s.key NOT IN ({keys})) \
            AND {settled_first} AND {settled_review} AND {settled_chunks} AND {groups} \
            AND (jsonb_typeof({first})='object' OR jsonb_typeof({review})='object' OR jsonb_typeof({chunks})='object') \
            ELSE false END)",
        recovery=sql_null("(payload->'recovery')"), attempt_null=sql_null("(payload->'scopeModelAttempt')"),
        settled_first=sql_stage(&first), settled_review=sql_stage(&review))
}

pub(super) fn payload_sql(fallback:&str)->String {
    let eligible=eligibility_sql();
    let stages="(payload->'preparationStages')";
    let projected_stages=format!("(SELECT jsonb_object_agg(s.key,CASE \
        WHEN s.key IN ('first','review') AND jsonb_typeof(s.value)='object' THEN {} \
        WHEN s.key='reviewChunks' AND jsonb_typeof(s.value)='object' THEN \
            jsonb_set(s.value,'{{chunks}}',(SELECT jsonb_agg({} ORDER BY c.ordinality) \
                FROM jsonb_array_elements(s.value->'chunks') WITH ORDINALITY c(value,ordinality))) \
        ELSE s.value END) FROM jsonb_each({stages}) s)",strip_result("s.value"),strip_result("c.value"));
    format!("CASE WHEN ({eligible}) THEN jsonb_set(CASE WHEN jsonb_typeof(payload->'result')='object' \
        THEN (payload#-'{{prepareBundle,request}}')-'result' ELSE payload#-'{{prepareBundle,request}}' END, \
        '{{preparationStages}}',{projected_stages}) ELSE ({fallback}) END")
}

#[cfg(test)]
#[path="storage_source_jobs_tests.rs"]
mod tests;
