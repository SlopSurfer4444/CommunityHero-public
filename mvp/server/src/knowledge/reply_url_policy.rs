//! Owner-authored, versioned company URL policy. Imported account cards remain
//! historical evidence; an explicit edit creates a typed successor head.
use super::*;
use crate::{App,ApiResult,audit,auto_prepare,conflict,now,reply_constraints};
use axum::{Json,extract::State};

fn field<'a>(body:&'a Value,key:&str,max:usize)->Result<&'a str,&'static str>{
    body[key].as_str().filter(|value|!value.trim().is_empty()&&value.len()<=max
        &&!value.bytes().any(|byte|byte<0x20||byte==0x7f))
        .ok_or("Invalid reply URL policy request")
}

fn policy(values:&Value)->Result<Value,&'static str>{
    let value=json!({"schemaVersion":1,"operator":"allowed_reply_urls","matching":"exact_url_v1","values":values});
    reply_constraints::typed_values(&value)?;
    Ok(value)
}

fn receipt(d:&Value,version:&Value,at:&str,replayed:bool)->Result<Value,&'static str>{
    let mut current=read(d,at)?;
    if current["configured"]==true && current["entryId"]!=version["entryId"] {
        return Err("Reply URL policy current head differs from saved version");
    }
    current["savedVersionId"]=version["id"].clone();
    current["replayed"]=json!(replayed);
    Ok(current)
}

pub(crate) fn read(d:&Value,at:&str)->Result<Value,&'static str>{
    let Some(head)=reply_constraints::company_head(d,at)? else {
        return Ok(json!({"configured":false,"source":"none","entryId":null,"currentVersionId":null,"values":null}));
    };
    Ok(json!({"configured":true,"source":match head.source {reply_constraints::PolicySource::Typed=>"typed",reply_constraints::PolicySource::LegacyImport=>"legacy_import"},
        "entryId":head.entry_id,"currentVersionId":head.version_id,
        "values":head.values.into_iter().collect::<Vec<_>>()}))
}

pub(crate) fn save(d:&mut Value,body:&Value,at:&str)->Result<Value,&'static str>{
    validate(d)?;
    timestamp(at)?;
    let object=body.as_object().ok_or("Reply URL policy must be an object")?;
    if object.keys().any(|key|!["requestId","expectedVersionId","values"].contains(&key.as_str())) {
        return Err("Unsupported reply URL policy field");
    }
    let request_id=field(body,"requestId",160)?;
    let expected=body["expectedVersionId"].as_str();
    if !body["expectedVersionId"].is_null() && expected.is_none_or(|value|value.is_empty()||value.len()>512) {
        return Err("Invalid expected reply URL policy version");
    }
    let typed=policy(&body["values"])?;
    let account=supported_account(d)?.to_owned();
    let binding=d.get("connectorBinding")
        .and_then(|value|crate::connectors::ConnectorBinding::from_json(value).ok())
        .filter(|binding|binding.validate_scope("local-pilot",&account).is_ok())
        .ok_or("Reply URL policy needs an explicit company binding")?;
    let scope=json!({"account":account,"postKeys":[]});
    let request_hash=hash(&json!({"requestId":request_id,"scope":scope,"values":body["values"],"matching":"exact_url_v1"}));
    // Resolve an idempotent replay before current-head CAS. It may be an older
    // version after a later authorized edit, and never moves the head back.
    if let Some(existing)=rows(d,"knowledge_versions").iter().find(|v|
        v["category"]=="reply_url_policy"&&v["operatorRequestId"]==request_id
            &&v["scope"]==scope) {
        if existing["operatorRequestHash"]!=request_hash {return Err("Reply URL policy requestId was reused");}
        return receipt(d,existing,at,true);
    }
    let current=reply_constraints::company_head(d,at)?;
    let candidates=rows(d,"knowledge_entries").iter().filter_map(|entry|
        rows(d,"knowledge_versions").iter().find(|v|v["id"]==entry["currentVersionId"]))
        .filter(|v|v["scope"]["account"]==account &&
            (v["category"]=="reply_url_policy"
                || v["companyImport"]["source"]["originalIds"]["field"]=="allowed_reply_urls"))
        .count();
    if candidates>1 || (current.is_none()&&candidates>0) {
        return Err("Reply URL policy has an unresolved current head");
    }
    if current.as_ref().map(|head|head.version_id.as_str())!=expected {
        return Err("Reply URL policy version conflict");
    }
    let previous=current.as_ref().map(|head|rows(d,"knowledge_versions").iter()
        .find(|v|v["id"]==head.version_id).ok_or("Missing current reply URL policy version"))
        .transpose()?;
    let source=previous.map(|v|text(v,"sourceMaterialId").to_owned())
        .unwrap_or_else(||format!("operator-reply-url-policy-{}",uuid::Uuid::new_v4()));
    let entry=previous.map(|v|text(v,"entryId").to_owned())
        .unwrap_or_else(||format!("knowledge-{}",sha(source.as_bytes())));
    let revision=if previous.is_some(){
        rows(d,"materials").iter().find(|m|m["id"]==source)
            .and_then(|m|m["revision"].as_u64()).ok_or("Reply URL policy backing material missing")?
            .checked_add(1).ok_or("Reply URL policy revision overflow")?
    }else{1};
    if previous.is_none() && rows(d,"knowledge_entries").iter().any(|e|e["id"]==entry)
        || rows(d,"materials").iter().filter(|m|m["id"]==source).count()>1 {
        return Err("Reply URL policy identity conflict");
    }
    let values=body["values"].as_array().unwrap();
    let description=if values.is_empty(){"В исходящих ответах ссылки не разрешены.".to_owned()}
        else{format!("В исходящих ответах допустимы только эти точные URL, если ссылка действительно нужна:\n{}",
            values.iter().map(|v|v.as_str().unwrap()).collect::<Vec<_>>().join("\n"))};
    if description.encode_utf16().count()>24_000 {
        return Err("Reply URL policy exceeds preparation text budget");
    }
    let material=json!({"id":source,"account":account,"title":"Разрешённые ссылки","text":description,
        "kind":"policy","postKey":"","sourceUrl":"","sourceDate":null,"revision":revision,
        "updatedAt":at,"locallyEdited":true,"policyType":"reply_url_policy","replyUrlPolicy":typed});
    let mut version=json!({"entryId":entry,"sourceMaterialId":source,"sourceRevision":revision,
        "sourceHash":hash(&content(&material)),"title":material["title"],"text":material["text"],
        "sourceUrl":"","postKey":"","kind":"policy","category":"reply_url_policy",
        "scope":scope,"trust":"verified","status":"active","validFrom":at,"validUntil":null,
        "sourceDate":null,"supersedes":previous.map(|v|v["id"].clone()),"createdAt":at,
        "changedBy":"operator","grantsExecutionAuthority":false,
        "replyUrlPolicy":typed,"operatorRequestId":request_id,"operatorRequestHash":request_hash,
        "bindingAtEdit":binding.to_json()});
    let digest=version_hash(&version);
    version["hash"]=json!(digest);
    version["id"]=json!(format!("knowledge-version-{digest}"));
    let mut head=json!({"id":entry,"sourceMaterialId":source,"currentVersionId":version["id"],
        "kind":"policy","scope":scope,"status":"active"});
    if let Some(previous_entry)=rows(d,"knowledge_entries").iter().find(|row|row["id"]==entry) {
        if let Some(receipts)=previous_entry.get("companyImportReceipts") {
            head["companyImportReceipts"]=receipts.clone();
        }
    }
    for (collection,record) in [("materials",material),("knowledge_entries",head)] {
        if !d[collection].is_array(){d[collection]=json!([]);}
        let rows=d[collection].as_array_mut().unwrap();
        if let Some(old)=rows.iter_mut().find(|row|row["id"]==record["id"]){*old=record;}
        else{rows.push(record);}
    }
    if !d["knowledge_versions"].is_array(){d["knowledge_versions"]=json!([]);}
    d["knowledge_versions"].as_array_mut().unwrap().push(version.clone());
    receipt(d,&version,at,false)
}

pub(crate) async fn get(State(app):State<App>)->ApiResult<Json<Value>>{
    let d=app.read().await?;
    Ok(Json(read(&d,&now()).map_err(conflict)?))
}

pub(crate) async fn put(State(app):State<App>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    app.change(|d|{
        let result=save(d,&body,&now()).map_err(conflict)?;
        if result["replayed"]!=true {
            auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
            audit(d,"reply_url_policy.version",result["entryId"].as_str().unwrap());
        }
        Ok(Json(result))
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT:&str="2026-09-24T12:00:00Z";
    fn workspace_for(account:&str)->Value{
        let mut d=crate::empty();
        d["account"]=json!(account);
        d["connectorBinding"]=json!({"id":"third-connection","workspaceId":"local-pilot",
            "accountId":account,"connector":"angryspace","revision":1,
            "providerAccountId":"third-company"});
        d["items"]=json!([{"id":"one","postKey":"post-one","revision":1}]);
        d
    }
    fn workspace()->Value{workspace_for("Third Company")}
    fn check(d:&Value,reply:&str)->Result<(),&'static str>{
        let context=crate::prepare_bundle::EvidenceContext::new(d);
        crate::reply_constraints::validate_reply(&context,&d["items"][0],reply)
    }
    fn check_company_policy(d:&Value,reply:&str)->Result<(),&'static str>{
        let Some(head)=reply_constraints::company_head(d,AT)? else{return Ok(())};
        let found=reply_constraints::typed_urls(reply)?;
        if found.into_iter().all(|url|head.values.contains(url)){Ok(())}
        else{Err("Reply URL is not allowed by current policy")}
    }
    #[test]
    fn new_company_typed_policy_is_scoped_and_absent_differs_from_empty(){
        let mut d=workspace();
        assert_eq!(read(&d,AT).unwrap()["configured"],false);
        assert_eq!(check_company_policy(&d,"https://outside.example/path"),Ok(()));
        let created=save(&mut d,&json!({"requestId":"empty","expectedVersionId":null,"values":[]}),AT).unwrap();
        assert_eq!(created["source"],"typed");
        assert_eq!(read(&d,AT).unwrap()["values"],json!([]));
        assert_eq!(check_company_policy(&d,"Plain text"),Ok(()));
        assert_eq!(check_company_policy(&d,"https://outside.example/path"),Err("Reply URL is not allowed by current policy"));
        for unsupported in ["data:text/plain,a","file:///tmp/x","tel:+123","ws://host/x","wss://host/x",
            "blob:https://brand.example/id","https:brand.example/path","https:/brand.example/path"] {
            assert_eq!(check_company_policy(&d,unsupported),Err("Reply URL is not allowed by current policy"),"{unsupported}");
        }
        assert_eq!(check_company_policy(&d,"Цена:100, цвет: синий"),Ok(()));
        assert_eq!(d["knowledge_entries"].as_array().unwrap().len(),1);
        assert_eq!(d["knowledge_versions"].as_array().unwrap().len(),1);
        let before=d.clone();
        let replay=save(&mut d,&json!({"requestId":"empty","expectedVersionId":null,"values":[]}),AT).unwrap();
        assert_eq!(replay["replayed"],true);
        assert_eq!(d,before);
        assert!(save(&mut d,&json!({"requestId":"empty","expectedVersionId":null,"values":["https://new.example/"]}),AT).is_err());
        assert_eq!(d,before);
    }
    #[test]
    fn versioned_edit_invalidates_review_fingerprint_and_preserves_unrelated_state(){
        let mut d=workspace_for("LikeAvto");
        d["items"][0]["draft"]=json!("human draft");
        d["operations"]=json!([{"id":"unknown-one","status":"unknown","providerRetryAllowed":false}]);
        let first=save(&mut d,&json!({"requestId":"first","expectedVersionId":null,
            "values":["https://brand.example/path"]}),AT).unwrap();
        let selected=super::super::select(&d,&[json!({"postKey":"post-one"})],&[],AT).unwrap();
        assert_eq!(selected["materials"][0]["replyUrlPolicy"]["operator"],"allowed_reply_urls");
        assert_eq!(selected["manifest"][0]["versionId"],first["currentVersionId"]);
        let source=d["knowledge_entries"][0]["sourceMaterialId"].as_str().unwrap().to_owned();
        assert!(super::super::protects_reply_url_material(&d,&source));
        let mut tampered=d.clone();
        tampered["materials"][0]["text"]=json!("Unreviewed generic material text");
        assert_eq!(super::super::sync_catalog(&mut tampered,AT),Err("Reply URL policy requires a versioned edit"));
        assert_eq!(tampered["knowledge_entries"][0]["currentVersionId"],first["currentVersionId"]);
        assert_eq!(check(&d,"Read https://brand.example/path."),Ok(()));
        assert_eq!(check(&d,"Read https://brand.example/path?other=1"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"Read https://brand.example.evil/path"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"Read HTTP://brand.example/path"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"Read www.brand.example/path"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"Read //brand.example/path"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"//brand.example/path"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"custom://brand.example/path"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"Email mailto:brand@example.com"),Err("Reply URL is not allowed by current policy"));
        let before=crate::prepare_bundle::EvidenceContext::new(&d).review_fingerprint("one").unwrap();
        let old=d.clone();
        assert!(save(&mut d,&json!({"requestId":"wrong-cas","expectedVersionId":"wrong",
            "values":[]}),AT).is_err());
        assert_eq!(d,old);
        let second=save(&mut d,&json!({"requestId":"second","expectedVersionId":first["currentVersionId"],
            "values":["https://brand.example/new"]}),"2026-09-24T12:01:00Z").unwrap();
        assert_ne!(first["currentVersionId"],second["currentVersionId"]);
        let before_replay=d.clone();
        let replay=save(&mut d,&json!({"requestId":"first","expectedVersionId":null,
            "values":["https://brand.example/path"]}),"2026-09-24T12:02:00Z").unwrap();
        assert_eq!(replay["replayed"],true);
        assert_eq!(replay["savedVersionId"],first["currentVersionId"]);
        assert_eq!(replay["currentVersionId"],second["currentVersionId"]);
        assert_eq!(replay["values"],json!(["https://brand.example/new"]));
        assert_eq!(d,before_replay);
        let after=crate::prepare_bundle::EvidenceContext::new(&d).review_fingerprint("one").unwrap();
        assert_ne!(before,after);
        assert_eq!(d["items"][0]["draft"],"human draft");
        assert_eq!(d["operations"][0]["status"],"unknown");
        assert_eq!(d["operations"][0]["providerRetryAllowed"],false);
        assert_eq!(check(&d,"https://brand.example/path"),Err("Reply URL is not allowed by current policy"));
        assert_eq!(check(&d,"https://brand.example/new"),Ok(()));
    }
    #[test]
    fn foreign_company_binding_and_invalid_urls_fail_without_mutation(){
        let mut d=workspace();
        d["connectorBinding"]["accountId"]=json!("Foreign Company");
        let before=d.clone();
        assert!(save(&mut d,&json!({"requestId":"foreign","expectedVersionId":null,"values":[]}),AT).is_err());
        assert_eq!(d,before);
        let mut d=workspace();
        for url in ["https://brand.example.evil@brand.example/","https://BRAND.example/","https://brand.example/#fragment",
            "https://brand.example:443/","https://brand.example/%GG","javascript:alert(1)"] {
            let before=d.clone();
            assert!(save(&mut d,&json!({"requestId":"bad","expectedVersionId":null,"values":[url]}),AT).is_err(),"{url}");
            assert_eq!(d,before);
        }
        let long:Vec<_>=(0..20).map(|n|format!("https://brand.example/{n}/{}","x".repeat(1500))).collect();
        let before=d.clone();
        assert_eq!(save(&mut d,&json!({"requestId":"oversized","expectedVersionId":null,"values":long}),AT),
            Err("Reply URL policy exceeds preparation text budget"));
        assert_eq!(d,before);
    }
}
