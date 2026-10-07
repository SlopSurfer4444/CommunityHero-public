//! Genuine domain fixtures, executed only by the integrator's native test gate.
use super::*;
use crate::media_context_gate::tests::{fixture,video_fixture,transcript,failed_video_attempt};
async fn sqlite()->(Database,tempfile::TempDir){
    let folder=tempfile::tempdir().unwrap();let pool=crate::open_db(&folder.path().join("media-selected.sqlite")).await.unwrap();(Database::Sqlite(pool),folder)
}
async fn seed(db:&Database,view:&Value){let Database::Sqlite(pool)=db else{unreachable!()};sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(view.to_string()).execute(pool).await.unwrap();}
fn inspect(view:&Value,key:&str)->Result<Value,(u16,String)>{
    crate::row(view,"proposals",key).and_then(|p|crate::media_context_gate::inspect(&crate::prepare_bundle::EvidenceContext::new(view),p)).map_err(|e|(e.0.as_u16(),e.1))
}
#[tokio::test]
async fn media_gate_selected_read_matches_full_requirements_digest_and_attempts(){
    let(db,_folder)=sqlite().await;
    for mode in ["none","photos","video","speech","two_videos","comment_audio","same_key_post"]{
        let attachments=match mode {
            "none"|"comment_audio"=>json!([]),"photos"=>json!([{"type":"photo","url":"https://example.invalid/a.png"},{"type":"photo","url":"https://example.invalid/b.png"}]),
            "two_videos"=>json!([{"type":"video","url":"https://example.invalid/a.mp4"},{"type":"video","url":"https://example.invalid/b.mp4"}]),
            _=>json!([{"type":"video","url":"https://example.invalid/a.mp4"}]),
        };
        let mut full=fixture("close",attachments);
        if mode=="speech"{transcript(&mut full,false,false);}
        if mode=="comment_audio"{full["items"][0]["attachments"]=json!([{"type":"audio","url":"https://example.invalid/comment.mp3"}]);}
        if mode=="same_key_post" {let mut alias=full["posts"][0].clone();alias["id"]=json!("same-key-alias");alias["attachments"]=json!([{"type":"photo","url":"https://example.invalid/alias.png"}]);full["posts"].as_array_mut().unwrap().push(alias);}
        seed(&db,&full).await;let selected=db.read_media_gate_context("p",MediaGateReadBudget::default()).await.unwrap();
        assert_eq!(inspect(&selected,"p"),inspect(&full,"p"),"full inspector parity: {mode}");
        assert_eq!(selected["posts"],full["posts"],"every id/postKey source match retained");
        assert_eq!(selected["branches"],full["branches"],"complete branch ordering/roles retained");
    }
    db.close().await;
}
#[tokio::test]
async fn media_selected_multi_post_attempt_cached_audio_and_waiver_survive_history_growth(){
    let(db,_folder)=sqlite().await;let mut full=video_fixture("close");failed_video_attempt(&mut full);
    full["jobs"][0]["refId"]=json!("unrelated-first-post");
    let source=crate::media_fullframes::source_version(&full["posts"][0],full["account"].as_str().unwrap());
    let pin=json!({"originJobId":"origin","originEpoch":2,"progress":{
        "account":full["account"],"connectorBinding":full["connectorBinding"],"sourcePostId":"post","sourcePostKey":"post","sourceVersion":source,
        "source":{"sha256":"b".repeat(64)},"sourceIdentity":{"account":full["account"],"postKey":"post","mediaSha256":"b".repeat(64)}},
        "policy":{"account":full["account"],"connectorBinding":full["connectorBinding"],"sourceVersion":source}});
    let cached_failure=json!({"id":"cached-failure","kind":"media_audio","status":"failed","refId":"post",
        "account":full["account"],"connectorBinding":full["connectorBinding"],"audioPin":pin,"error":"audio_extraction_failed"});
    full["jobs"].as_array_mut().unwrap().push(cached_failure);
    let state=inspect(&full,"p").unwrap();assert_eq!(state["attempts"].as_array().unwrap().len(),2);
    let mut waiver=json!({"version":1,"proposalId":"p","proposalRevision":1,"contextDigest":state["contextDigest"],
        "actor":{"id":"test-operator"},"authorityGeneration":"a".repeat(64),"reason":"Exact failed acquisition","attempts":state["attempts"]});
    waiver["waiverSha256"]=json!(crate::media_fullframes::hash(&waiver));full["proposals"][0]["mediaContextWaiver"]=waiver;
    full["materials"]=json!([{"id":"bounded-test-reference","kind":"reference","text":"Synthetic current reference","revision":1}]);
    crate::knowledge::sync_catalog(&mut full,"2026-01-01T00:00:00Z").unwrap();
    let old_version=full["knowledge_versions"][0].clone();
    for n in 0..250 {
        full["jobs"].as_array_mut().unwrap().push(json!({"id":format!("cold-{n}"),"kind":"assistant","status":"completed","result":{"text":"COLD_HISTORY_PRIVATE".repeat(1000)}}));
        let mut history=old_version.clone();history["id"]=json!(format!("old-unselected-version-{n}"));
        history["text"]=json!("COLD_KNOWLEDGE_PRIVATE".repeat(1000));
        let mut unsigned=history.clone();for key in ["id","hash","createdAt"]{unsigned.as_object_mut().unwrap().remove(key);}
        history["hash"]=json!(format!("{:x}",<sha2::Sha256 as sha2::Digest>::digest(unsigned.to_string().as_bytes())));
        full["knowledge_versions"].as_array_mut().unwrap().push(history);
    }
    full["feedback"]=json!([{"id":"feedback","text":"OWNER_ONLY_FEEDBACK".repeat(1000)}]);
    seed(&db,&full).await;let selected=db.read_media_gate_context("p",MediaGateReadBudget::default()).await.unwrap();
    assert_eq!(inspect(&selected,"p"),inspect(&full,"p"),"current attempts and exact cached pin preserve waiver diagnostics");
    assert_eq!(inspect(&selected,"p").unwrap()["status"],"waived");
    assert_eq!(selected["jobs"].as_array().unwrap().len(),2);
    assert_eq!(crate::row(&selected,"jobs","cached-failure").unwrap()["audioPin"],pin);
    assert!(!selected.to_string().contains("COLD_HISTORY_PRIVATE"));assert!(!selected.to_string().contains("COLD_KNOWLEDGE_PRIVATE"));
    assert!(!selected.to_string().contains("OWNER_ONLY_FEEDBACK"));assert!(selected.to_string().len()*100<full.to_string().len());
    db.close().await;
}
#[tokio::test]
async fn media_selected_preserves_native_editorial_material_parents_and_duplicate_bundle_order(){
    let(db,_folder)=sqlite().await;let(mut full,batch,result,native)=super::super::hot_admission::tests::native_editorial_material_fixture();
    crate::editorial_review::admit(&mut full,&batch,&result,"2026-10-06T00:00:01Z").unwrap();
    let key=full["proposals"][0]["id"].as_str().unwrap().to_owned();
    crate::row_mut(&mut full,"jobs",&native).unwrap()["nativeSourceOriginJobId"]=json!("parent-material");
    for key in ["parent-material","duplicate-material-family"]{crate::list_mut(&mut full,"jobs").push(json!({"id":key,"kind":"material_acquisition","status":"completed",
        "prepareBundle":{"id":"shared-material-bundle"},"proofHistory":{"retained":"complete immutable native body"}}));}
    let expected=inspect(&full,&key);assert!(expected.is_ok());seed(&db,&full).await;
    let selected=db.read_media_gate_context(&key,MediaGateReadBudget::default()).await.unwrap();assert_eq!(inspect(&selected,&key),expected);
    for key in [native.as_str(),"parent-material","duplicate-material-family"]{assert_eq!(crate::row(&selected,"jobs",key).unwrap(),crate::row(&full,"jobs",key).unwrap());}
    assert!(crate::row(&selected,"jobs","cold-editorial-history").is_err());
    for mutation in ["missing_history","wrong_kind","changed_paid_pin"] {
        let mut changed=full.clone();let native=crate::row_mut(&mut changed,"jobs",&native).unwrap();
        match mutation {"missing_history"=>native["modelMaterialReceipts"]=json!([]),"wrong_kind"=>native["kind"]=json!("assistant"),_=>native["retainedEvidence"]=json!([])}
        seed(&db,&changed).await;let selected=db.read_media_gate_context(&key,MediaGateReadBudget::default()).await.unwrap();assert_eq!(inspect(&selected,&key),inspect(&changed,&key),"{mutation}");
    }
    db.close().await;
}
#[tokio::test]
async fn media_gate_selected_read_rejects_tamper_ambiguity_foreign_scope_and_budget(){
    let(db,_folder)=sqlite().await;let baseline=video_fixture("close");
    for mode in ["foreign_binding","missing_item","duplicate_proposal","malformed_job_ref","changed_head_hash"]{
        let mut full=baseline.clone();match mode {
            "foreign_binding"=>full["connectorBinding"]=crate::accounts::Profile::BawRussia.binding(),
            "missing_item"=>full["items"]=json!([]),
            "duplicate_proposal"=>{let p=full["proposals"][0].clone();full["proposals"].as_array_mut().unwrap().push(p);},
            "malformed_job_ref"=>full["proposals"][0]["prepareRunId"]=json!({"invalid":"reference"}),
            _=>{transcript(&mut full,false,false);full["knowledge_versions"][0]["hash"]=json!("0".repeat(64));},
        }
        seed(&db,&full).await;assert!(db.read_media_gate_context("p",MediaGateReadBudget::default()).await.is_err(),"{mode}");
    }
    seed(&db,&baseline).await;
    for budget in [MediaGateReadBudget{max_rows:1,max_bytes:16*1024*1024},MediaGateReadBudget{max_rows:3000,max_bytes:1}]{assert_eq!(db.read_media_gate_context("p",budget).await.unwrap_err().0,StatusCode::PAYLOAD_TOO_LARGE);}
    assert_eq!(db.read_media_gate_context("missing",MediaGateReadBudget::default()).await.unwrap_err().0,StatusCode::NOT_FOUND);
    let Database::Sqlite(pool)=&db else{unreachable!()};let after:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(pool).await.unwrap();
    assert_eq!(parse(&after).unwrap(),baseline,"diagnostic errors never mutate or silently fall back to workspace writes");db.close().await;
}
