//! Offline regressions for account, source revision and alias binding parity.
use super::*;

const AT: &str = "2026-09-27T00:00:00Z";
const URL: &str = "https://youtu.be/AbCdEf123_-";

fn title_compat_fixture()->(Value,Value,Value){
    let old=json!({"id":"title-source","postKey":"title-source","account":"LikeAvto",
        "title":"Original provider title","text":"Unchanged context","sourceUrl":URL,
        "durationMs":1000,"mediaSha256":sha(b"synthetic source LikeAvto title-source"),
        "attachments":[{"type":"video","url":URL}]});
    let proof=crate::media_fullframes::fixture_for_post("LikeAvto",&old);
    let visual=json!({"id":"title-visual","title":"Visual context: Original provider title",
        "kind":"visual_context","account":"LikeAvto","postKey":old["postKey"],
        "sourceUrl":old["sourceUrl"],"mediaSha256":old["mediaSha256"],
        "text":"Historical visual observations","visualEvidence":proof});
    let mut current=old.clone();current["title"]=json!("Current operator projection title");
    let alias=json!({"id":"title-alias","postKey":"title-alias","account":"LikeAvto",
        "title":current["title"],"sourceUrl":"https://vk.com/video-123_456",
        "attachments":[{"type":"video"}]});
    let mut d=json!({"account":"LikeAvto","posts":[current,alias],
        "materials":[visual,speech("current-speech",&current)]});
    sync_catalog(&mut d,AT).unwrap();
    (d,old,current)
}

#[test]
fn same_source_title_projection_preserves_proof_but_never_shares_by_title(){
    let (mut d,old,current)=title_compat_fixture();
    d["items"]=json!([{"id":"title-item","postId":current["id"],"postKey":current["postKey"],"text":"Public recipient","revision":1}]);
    let v=rows(&d,"knowledge_versions").iter().find(|v|v["kind"]=="visual_context").unwrap();
    let unchanged=v["visualEvidence"].clone();
    assert_ne!(crate::media_fullframes::source_version(&old,"LikeAvto"),crate::media_fullframes::source_version(&current,"LikeAvto"),"global reply/source fingerprint remains strict");
    assert!(visual_source_associated(v,&current,"LikeAvto"));
    let observed=observed_video_posts(&d,"LikeAvto");
    assert_eq!(observed[0]["mediaSha256"],v["mediaSha256"]);
    let lookup=TranscriptLookup::new(&d,AT).unwrap();
    for post in rows(&d,"posts"){
        let own_source=post["id"]==current["id"];
        assert_eq!(lookup.has_visual(post).unwrap(),own_source,"same-title different video has no proof");
        let selected=select(&d,&[],std::slice::from_ref(post),AT).unwrap();
        assert_eq!(rows(&selected,"materials").iter().any(|m|m["kind"]=="visual_context"),own_source);
        assert_eq!(lookup.readiness_for_policy(post,true).unwrap()["ready"],json!(lookup.ready_for_policy(post,true).unwrap()));
    }
    assert_eq!(v["visualEvidence"],unchanged);
    let before=crate::prepare_bundle::review_fingerprint(&d,"title-item").unwrap();
    let mut changed=d.clone();changed["posts"][0]["title"]=json!("Further title change");
    assert!(TranscriptLookup::new(&changed,AT).unwrap().has_visual(&changed["posts"][0]).unwrap());
    assert_ne!(before,crate::prepare_bundle::review_fingerprint(&changed,"title-item").unwrap(),"visual proof reuse must not preserve reply approval fingerprint");
}

#[test]
fn same_title_different_video_keeps_required_evidence_missing_and_receipts_intact(){
    for account in ["LikeAvto","BAW Russia"]{
        let source=json!({"id":"source","postKey":"source","account":account,
            "title":"Repeated campaign title","sourceUrl":URL,"durationMs":1000,
            "mediaSha256":sha(format!("synthetic source {account} source").as_bytes()),
            "attachments":[{"type":"video"}]});
        let target=json!({"id":"target","postKey":"target","account":account,
            "title":"Repeated campaign title","sourceUrl":"https://vk.com/video-123_456",
            "durationMs":1000,"attachments":[{"type":"video"}]});
        let visual=crate::media_fullframes::fixture_for_post(account,&source);
        let mut d=json!({"account":account,"posts":[source,target],"materials":[
            {"id":"paid-speech","kind":"transcript","account":account,"postKey":"source",
                "sourceUrl":source["sourceUrl"],"mediaSha256":source["mediaSha256"],"text":"Only the donor's speech",
                "transcription":{"partial":false,"coverage":"full_audio",
                    "sourceVersion":crate::media_fullframes::source_version(&source,account),
                    "mediaDurationSeconds":1.0,"audioDurationSeconds":1.0}},
            {"id":"paid-screen","kind":"ocr","account":account,"postKey":"source","text":"Donor overlay"},
            {"id":"paid-visual","kind":"visual_context","account":account,"postKey":"source",
                "sourceUrl":source["sourceUrl"],"mediaSha256":source["mediaSha256"],
                "text":"Only the donor's frames","visualEvidence":visual}],
            "jobs":[{"id":"paid-preparation","status":"completed","result":{"raw":"retained paid output"}}],
            "approvals":[{"id":"historical-approval","mediaBinding":{"match":"exact_normalized_title"}}],
            "operations":[{"id":"unknown-effect","status":"unknown"}]});
        sync_catalog(&mut d,AT).unwrap();let unchanged=d.clone();
        let lookup=TranscriptLookup::new(&d,AT).unwrap();
        assert!(lookup.ready_for_policy(&source,true).unwrap());
        assert!(!lookup.ready_for_policy(&target,true).unwrap());
        let missing=lookup.readiness_for_policy(&target,true).unwrap();
        let reasons:BTreeSet<_>=rows(&missing,"reasons").iter().filter_map(Value::as_str).collect();
        assert!(reasons.contains("audio_head_missing")&&reasons.contains("visual_head_missing"));
        assert_eq!(lookup.strict_media_evidence(&target).unwrap()["screenTextReady"],false);
        assert!(rows(&select(&d,&[],std::slice::from_ref(&target),AT).unwrap(),"materials").is_empty());
        assert_ne!(media_group_key(&source,account),media_group_key(&target,account));
        assert_eq!(d,unchanged,"selection cannot delete paid receipts, old approvals, UNKNOWN or media history");
    }
}

#[test]
fn exact_bytes_share_evidence_and_groups_without_sharing_company_authority(){
    let (mut d,_,source)=title_compat_fixture();
    d["posts"][1]["title"]=json!("Different publication title");
    d["posts"][1]["mediaSha256"]=source["mediaSha256"].clone();
    let target=d["posts"][1].clone();let unchanged=d.clone();
    assert!(TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&target,true).unwrap());
    assert_eq!(media_group_key(&source,"LikeAvto"),media_group_key(&target,"LikeAvto"));
    assert!(rows(&select(&d,&[],std::slice::from_ref(&target),AT).unwrap(),"materials").iter().any(|m|m["kind"]=="transcript"));
    let mut foreign=target.clone();foreign["account"]=json!("BAW Russia");
    assert!(!TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&foreign,true).unwrap());
    assert_eq!(media_group_key(&foreign,"LikeAvto"),None);
    assert_eq!(d,unchanged);
}

#[test]
fn visual_title_compatibility_rejects_every_non_title_source_change(){
    let (d,_,current)=title_compat_fixture();
    let v=rows(&d,"knowledge_versions").iter().find(|v|v["kind"]=="visual_context").unwrap();
    for (field,value) in [("text",json!("changed")),("sourceUrl",json!("https://youtu.be/ZbCdEf123_-")),
        ("url",json!(URL)),("attachments",json!([])),("id",json!("other")),("postKey",json!("other")),
        ("account",json!("BAW Russia")),("durationMs",json!(1001)),("durationSeconds",json!(1)),
        ("contentSha256",json!("b".repeat(64))),("mediaSha256",json!("b".repeat(64))),
        ("canonicalMediaId",json!("other"))]{
        let mut changed=current.clone();changed[field]=value;
        assert!(!visual_source_associated(v,&changed,"LikeAvto"),"non-title field {field}");
    }
    for field in ["title","postKey","account","mediaSha256"]{
        let mut changed=v.clone();changed[field]=json!("invalid");
        assert!(!visual_source_associated(&changed,&current,"LikeAvto"),"head field {field}");
    }
    for (field,value) in [("companyImport",json!({})),("trust",json!("verified")),("status",json!("retired"))]{
        let mut changed=v.clone();changed[field]=value;
        assert!(!visual_source_associated(&changed,&current,"LikeAvto"),"noncanonical fallback {field}");
    }
    let mut missing=d.clone();missing["posts"]=json!([d["posts"][1]]);
    assert!(!TranscriptLookup::new(&missing,AT).unwrap().has_visual(&d["posts"][1]).unwrap(),"original source must exist");
    let mut retired=d.clone();retired["knowledge_entries"].as_array_mut().unwrap().retain(|e|e["currentVersionId"]!=v["id"]);
    assert!(!TranscriptLookup::new(&retired,AT).unwrap().has_visual(&current).unwrap(),"historical proof is not a current head");
}

#[test]
fn visual_title_compatibility_enriches_only_ephemeral_missing_observations(){
    let (_,mut old,_)=title_compat_fixture();
    let fields=old.as_object_mut().unwrap();fields.remove("mediaSha256");fields.remove("durationMs");
    let proof=crate::media_fullframes::fixture_for_post("LikeAvto",&old);
    let mut current=old.clone();current["title"]=json!("Current operator projection title");
    let mut d=json!({"account":"LikeAvto","posts":[current],"materials":[{
        "id":"unobserved-visual","title":"Visual context: Original provider title","kind":"visual_context",
        "account":"LikeAvto","postKey":old["postKey"],"sourceUrl":old["sourceUrl"],
        "mediaSha256":proof["source"]["mediaSha256"],"text":"Historical visual observations","visualEvidence":proof}]});
    sync_catalog(&mut d,AT).unwrap();let unchanged=d.clone();
    let observed=observed_video_posts(&d,"LikeAvto");
    assert_eq!(observed[0]["mediaSha256"],proof["source"]["mediaSha256"]);
    assert_eq!(observed[0]["durationMs"],proof["source"]["durationMs"]);
    assert!(observed[0]["mediaSha256"].is_string());assert!(observed[0]["durationMs"].is_number());
    assert!(TranscriptLookup::new(&d,AT).unwrap().has_visual(&current).unwrap());
    assert_eq!(d,unchanged,"raw source and immutable proof remain untouched");
}

fn speech(id: &str, post: &Value) -> Value {
    json!({"id":id,"kind":"transcript","account":"LikeAvto",
        "postKey":post["postKey"],"sourceUrl":post["sourceUrl"],
        "mediaSha256":post["mediaSha256"],"text":format!("Spoken words for {id}"),
        "transcription":{"partial":false,"coverage":"full_audio",
            "sourceVersion":crate::media_fullframes::source_version(post,"LikeAvto"),
            "mediaDurationSeconds":1.0,"audioDurationSeconds":1.0}})
}

#[test]
fn explicitly_malformed_transcript_source_version_never_matches() {
    let post = json!({"id":"p","postKey":"p","account":"LikeAvto",
        "title":"Specific road test","sourceUrl":URL,"mediaSha256":"a".repeat(64)});
    let mut baseline = json!({"account":"LikeAvto","posts":[post],"materials":[speech("speech", &post)]});
    sync_catalog(&mut baseline, AT).unwrap();
    assert!(TranscriptLookup::new(&baseline, AT).unwrap().has(&post).unwrap());
    let valid_source_version = baseline["materials"][0]["transcription"]["sourceVersion"].clone();
    for malformed in [json!(42), json!({"sourceVersion":"not-a-string"})] {
        let mut d = baseline.clone();
        d["materials"][0]["transcription"]["sourceVersion"] = malformed;
        sync_catalog(&mut d, AT).unwrap();
        assert!(!TranscriptLookup::new(&d, AT).unwrap().has(&post).unwrap());
        let selected = select(&d, &[], std::slice::from_ref(&post), AT).unwrap();
        assert!(!rows(&selected, "materials").iter().any(|m| m["kind"] == "transcript"));

        d["materials"][0]["transcription"]["sourceVersion"] = valid_source_version.clone();
        sync_catalog(&mut d, AT).unwrap();
        assert!(TranscriptLookup::new(&d, AT).unwrap().has(&post).unwrap());
        let restored = select(&d, &[], std::slice::from_ref(&post), AT).unwrap();
        assert!(rows(&restored, "materials").iter().any(|m| m["kind"] == "transcript"));
    }
}

#[test]
fn stale_direct_audio_never_pairs_with_current_visual_evidence() {
    let old = json!({"id":"p","postKey":"p","account":"LikeAvto",
        "title":"Specific road test","sourceUrl":URL,"mediaSha256":"a".repeat(64),
        "durationMs":1000,"attachments":[{"type":"video"}]});
    let mut d = json!({"account":"LikeAvto","posts":[old],"materials":[speech("speech", &old)]});
    sync_catalog(&mut d, AT).unwrap();
    assert!(TranscriptLookup::new(&d, AT).unwrap().has(&d["posts"][0]).unwrap());
    let old_version = d["knowledge_versions"][0].clone();
    let old_source_version = crate::media_fullframes::source_version(&d["posts"][0], "LikeAvto");

    d["posts"][0]["sourceUrl"] = json!("https://youtu.be/ZbCdEf123_-");
    // fixture_for_post uses these deterministic synthetic source bytes.
    d["posts"][0]["mediaSha256"] = json!(sha(b"synthetic source LikeAvto p"));
    let current = d["posts"][0].clone();
    let current_source_version = crate::media_fullframes::source_version(&current, "LikeAvto");
    assert_ne!(old_source_version, current_source_version);
    let visual = crate::media_fullframes::fixture_for_post("LikeAvto", &current);
    assert_eq!(visual["source"]["mediaSha256"], current["mediaSha256"]);
    d["materials"].as_array_mut().unwrap().push(json!({"id":"visual-v2",
        "kind":"visual_context","account":"LikeAvto","postKey":"p",
        "sourceUrl":current["sourceUrl"],"mediaSha256":current["mediaSha256"],
        "text":"Current source reviewed","visualEvidence":visual}));
    sync_catalog(&mut d, AT).unwrap();

    let lookup = TranscriptLookup::new(&d, AT).unwrap();
    assert!(lookup.has_visual(&current).unwrap(), "the new visual proof must be independently valid");
    assert!(!lookup.has(&current).unwrap(), "postKey cannot keep speech from the old source alive");
    assert!(!lookup.ready_for_policy(&current, true).unwrap());
    for visual_required in [false, true] {
        let diagnostic = lookup.readiness_for_policy(&current, visual_required).unwrap();
        assert_eq!(diagnostic["ready"], json!(lookup.ready_for_policy(&current, visual_required).unwrap()),
            "stale-source boolean and diagnostic gates must agree");
    }
    let selected = select(&d, &[], std::slice::from_ref(&current), AT).unwrap();
    assert!(!rows(&selected, "materials").iter().any(|m| m["kind"] == "transcript"));
    assert!(rows(&d, "knowledge_versions").contains(&old_version), "history must remain immutable");

    d["materials"][0] = speech("speech", &current);
    sync_catalog(&mut d, AT).unwrap();
    assert!(TranscriptLookup::new(&d, AT).unwrap().ready_for_policy(&current, true).unwrap());
    assert!(TranscriptLookup::new(&d, AT).unwrap().ready_for_policy(&current, false).unwrap());
}

#[test]
fn foreign_target_with_local_post_key_has_no_audio() {
    let local = json!({"id":"p","postKey":"p","account":"LikeAvto",
        "title":"Specific road test","sourceUrl":URL,"mediaSha256":"a".repeat(64)});
    let mut d = json!({"account":"LikeAvto","posts":[local],"materials":[speech("speech", &local)]});
    sync_catalog(&mut d, AT).unwrap();
    let lookup = TranscriptLookup::new(&d, AT).unwrap();
    assert!(lookup.has(&local).unwrap());
    for field in ["account", "accountId", "scope", "connectorBinding"] {
        let mut foreign = local.clone();
        foreign[field] = match field {
            "scope" => json!({"account":"BAW Russia"}),
            "connectorBinding" => json!({"accountId":"BAW Russia"}),
            _ => json!("BAW Russia"),
        };
        assert!(!lookup.has(&foreign).unwrap(), "foreign target field {field}");
        assert!(!lookup.ready_for_policy(&foreign, false).unwrap(), "foreign audio-only target field {field}");
        for visual_required in [false, true] {
            let diagnostic = lookup.readiness_for_policy(&foreign, visual_required).unwrap();
            assert_eq!(diagnostic["ready"], json!(lookup.ready_for_policy(&foreign, visual_required).unwrap()),
                "foreign target field {field}: boolean and diagnostic gates must agree");
        }
    }
}

#[test]
fn company_import_alias_scope_matches_readiness_and_selection() {
    let record = json!({"companyKey":"likeavto","importKey":"alias-speech","kind":"transcript",
        "metadata":{"grantsExecutionAuthority":false,"source_url":URL,"post_key":"legacy:a"},
        "scope":{"companyKey":"likeavto","postAliases":[{"namespace":"commentops-fast.post-key","value":"legacy:a"}]},
        "source":{"origin":"offline-test","sha256":"a".repeat(64)},"text":"Imported spoken words","observedAt":AT});
    let records = record.to_string().into_bytes();
    let manifest = serde_json::to_vec(&json!({"schemaVersion":"communityhero.company-knowledge.v1",
        "grantsExecutionAuthority":false,"generatedAt":AT,"recordCount":1,
        "companies":{"likeavto":{"recordCount":1}},
        "files":{"records.jsonl":{"bytes":records.len(),"sha256":sha(&records)}}})).unwrap();
    let package = company_import::Package::from_bytes(&manifest, &records, &sha(&manifest), &sha(&records)).unwrap();
    let mut d = json!({"account":"LikeAvto",
        "connectorBinding":{"connector":"angryspace","accountId":"LikeAvto","providerAccountId":"likeavto"},
        "posts":[{"id":"a","postKey":"legacy:a","sourceUrl":URL},
            {"id":"b","postKey":"legacy:b","sourceUrl":URL}],
        "materials":[],"knowledge_entries":[],"knowledge_versions":[]});
    company_import::apply(&mut d, &package, "likeavto", AT).unwrap();
    let lookup = TranscriptLookup::new(&d, AT).unwrap();
    for (index, expected) in [(0, true), (1, false)] {
        let post = &d["posts"][index];
        let selected = select(&d, &[], std::slice::from_ref(post), AT).unwrap();
        let selected_audio = rows(&selected, "materials").iter().any(|m| m["kind"] == "transcript");
        assert_eq!(selected_audio, expected);
        assert_eq!(lookup.has(post).unwrap(), selected_audio, "legacy alias scope cannot expand through a shared URL");
    }
}

#[test]
fn conflicting_copies_of_one_provider_url_keep_both_transcripts() {
    for (reverse, bridge) in [(false, false), (true, false), (false, true), (true, true)] {
        let a = json!({"id":"a","postKey":"a","account":"LikeAvto",
            "title":"Specific road test","sourceUrl":URL,"mediaSha256":"a".repeat(64)});
        let b = json!({"id":"b","postKey":"b","account":"LikeAvto",
            "title":"Specific road test","sourceUrl":URL,"mediaSha256":"b".repeat(64)});
        let mut posts = vec![a.clone(), b.clone()];
        let mut materials = vec![speech("speech-a", &a), speech("speech-b", &b)];
        if bridge {
            // This unknown copy sorts before both known copies. Pairwise
            // representative checks must not merge A/B transitively through C.
            posts.push(json!({"id":"c","postKey":"0-bridge","account":"LikeAvto",
                "title":"Specific road test","sourceUrl":URL}));
        }
        if reverse { posts.reverse(); materials.reverse(); }
        let mut d = json!({"account":"LikeAvto","posts":posts,"materials":materials});
        sync_catalog(&mut d, AT).unwrap();
        let before = d.clone();
        for (post, id) in [(&a, "speech-a"), (&b, "speech-b")] {
            let selected = select(&d, &[], std::slice::from_ref(post), AT).unwrap();
            assert_eq!(rows(&selected, "materials").len(), 1);
            assert_eq!(selected["materials"][0]["id"], id);
        }
        let selected = select(&d, &[], &[a, b], AT).unwrap();
        let ids: BTreeSet<_> = rows(&selected, "materials").iter().map(|m| text(m, "id")).collect();
        assert_eq!(ids, BTreeSet::from(["speech-a", "speech-b"]),
            "URL equality cannot override conflicting observed bytes: reverse={reverse}, bridge={bridge}");
        assert_eq!(rows(&selected, "manifest").len(), 2);
        assert_eq!(d, before, "deduplication cannot rewrite history");
    }
}

#[test]
fn unrelated_current_heads_report_missing_target_heads_without_source_mismatch() {
    let unrelated = json!({"id":"unrelated","postKey":"unrelated","account":"LikeAvto",
        "title":"Unrelated vehicle interview","sourceUrl":URL,"durationMs":1000,
        "mediaSha256":sha(b"synthetic source LikeAvto unrelated"),"attachments":[{"type":"video"}]});
    let target = json!({"id":"target","postKey":"target","account":"LikeAvto",
        "title":"Different mountain road review","sourceUrl":"https://youtu.be/ZbCdEf123_-",
        "mediaSha256":"f".repeat(64),"attachments":[{"type":"video"}]});
    let visual = crate::media_fullframes::fixture_for_post("LikeAvto", &unrelated);
    let mut d = json!({"account":"LikeAvto","posts":[unrelated,target],
        "materials":[speech("unrelated-speech", &unrelated),
            {"id":"unrelated-visual","kind":"visual_context","account":"LikeAvto",
                "postKey":"unrelated","sourceUrl":unrelated["sourceUrl"],
                "mediaSha256":unrelated["mediaSha256"],"text":"Unrelated visual observations","visualEvidence":visual}]});
    sync_catalog(&mut d, AT).unwrap();
    let lookup = TranscriptLookup::new(&d, AT).unwrap();
    assert!(lookup.ready_for_policy(&unrelated, true).unwrap(), "unrelated heads must be valid current evidence");
    for visual_required in [false, true] {
        let state = lookup.readiness_for_policy(&target, visual_required).unwrap();
        assert_eq!(state["ready"], false);
        assert_eq!(state["ready"], json!(lookup.ready_for_policy(&target, visual_required).unwrap()),
            "missing target heads: boolean and diagnostic gates must agree");
        let reasons: BTreeSet<_> = rows(&state, "reasons").iter().filter_map(Value::as_str).collect();
        assert!(reasons.contains("audio_head_missing"));
        if visual_required { assert!(reasons.contains("visual_head_missing")); }
        assert!(!reasons.iter().any(|reason| reason.contains("source_mismatch")),
            "unrelated evidence cannot manufacture target source drift: {state}");
        assert!(rows(&state["currentHeadIds"], "audio").is_empty());
        assert!(rows(&state["currentHeadIds"], "visual").is_empty());
    }
}

#[test]
fn legacy_head_identifiers_are_opaque_in_readiness_diagnostics() {
    let post = json!({"id":"private-head-test","postKey":"private-head-test","account":"LikeAvto",
        "title":"Specific road test","sourceUrl":URL,"durationMs":1000,
        "mediaSha256":sha(b"synthetic source LikeAvto private-head-test"),"attachments":[{"type":"video"}]});
    let visual = crate::media_fullframes::fixture_for_post("LikeAvto", &post);
    let mut d = json!({"account":"LikeAvto","posts":[post],
        "materials":[speech("speech", &post),
            {"id":"visual","kind":"visual_context","account":"LikeAvto","postKey":post["postKey"],
                "sourceUrl":post["sourceUrl"],"mediaSha256":post["mediaSha256"],
                "text":"Current visual observations","visualEvidence":visual}]});
    sync_catalog(&mut d, AT).unwrap();
    let private_ids = [
        "https://private.example.test/workspace-secret/transcript?token=private-token",
        r"C:\private\workspace-secret\company-ledger.sqlite",
    ];
    for (index, private_id) in private_ids.iter().enumerate() {
        let old_id = d["knowledge_versions"][index]["id"].clone();
        d["knowledge_versions"][index]["id"] = json!(private_id);
        d["knowledge_versions"][index]["hash"] = json!(version_hash(&d["knowledge_versions"][index]));
        for entry in d["knowledge_entries"].as_array_mut().unwrap() {
            if entry["currentVersionId"] == old_id { entry["currentVersionId"] = json!(private_id); }
        }
    }
    let lookup = TranscriptLookup::new(&d, AT).unwrap();
    for visual_required in [false, true] {
        let state = lookup.readiness_for_policy(&post, visual_required).unwrap();
        assert_eq!(state["ready"], true, "legacy identifiers do not invalidate admitted evidence");
        assert_eq!(state["ready"], json!(lookup.ready_for_policy(&post, visual_required).unwrap()),
            "legacy head IDs: boolean and diagnostic gates must agree");
        assert_eq!(rows(&state["currentHeadIds"], "audio").len(), 1);
        if visual_required { assert_eq!(rows(&state["currentHeadIds"], "visual").len(), 1); }
        let refs = state["currentHeadIds"].to_string();
        for private_id in private_ids { assert!(!refs.contains(private_id)); }
        for secret in ["private.example.test", "workspace-secret", "private-token", "company-ledger.sqlite"] {
            assert!(!refs.contains(secret), "private legacy ID leaked into diagnostic refs: {refs}");
        }
    }
}
