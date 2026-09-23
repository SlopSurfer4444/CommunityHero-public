//! Bounded cross-post history, scoped to an exact provider author identity.
//! Source statements are evidence of speech, never an internal order status.
use serde_json::{Value,json};
use std::collections::{BTreeMap,BTreeSet};

const LIMIT:usize=12;
fn rows<'a>(v:&'a Value,key:&str)->&'a [Value]{v[key].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn text<'a>(v:&'a Value,key:&str)->&'a str{v[key].as_str().unwrap_or("")}
fn time(v:&Value)->Option<i64>{chrono::DateTime::parse_from_rfc3339(text(v,"createdAt")).ok().map(|t|t.timestamp_millis())}
fn bounded(v:&Value,key:&str,max:usize)->Result<Value,&'static str>{
    let s=text(v,key);if s.encode_utf16().count()>max{return Err("Customer case field exceeds context limit");}Ok(json!(s))
}
fn account_matches(d:&Value,v:&Value)->bool {
    ["account","accountId"].iter().all(|k|v.get(*k).is_none_or(|a|a==&d["account"]))
}
fn contract_request(value:&str)->bool {
    let value=value.to_lowercase();
    value.split(['.','!','?','\n']).any(|sentence| {
        let Some(number)=sentence.find("номер") else{return false;};
        if !sentence[number..].contains("договор"){return false;}
        ["напиш","пришл","укаж","сообщ","отправ","подскаж","скин"].iter().any(|verb|
            sentence[..number].rfind(verb).is_some_and(|start|sentence[start..number].chars().count()<=100))
    })
}
// Company archive identities are aliases for the old Angry.Space connection.
// They are never provider IDs for a native social-network connection.
fn imported_cases(catalog:&crate::knowledge::Catalog<'_>,binding:&crate::ConnectorBinding,author:&str,platform:&str)
    ->Result<(Vec<Value>,Vec<Value>),&'static str> {
    let d=catalog.workspace();
    if binding.connector!=crate::connectors::ConnectorKind::AngrySpace {return Ok((vec![],vec![]));}
    let company=crate::accounts::Profile::from_workspace(d)
        .map_err(|_|"Customer case account is not configured")?.key();
    let platform=platform.to_ascii_lowercase();
    let mut customers=Vec::new();let mut published=Vec::new();
    let mut seen=BTreeSet::new();
    for entry in rows(d,"knowledge_entries") {
        if entry["kind"]!="customer_case" || entry["status"]!="active" {continue;}
        let Some(version)=catalog.version(text(entry,"currentVersionId")).filter(|v|
            v["id"]==entry["currentVersionId"] && v["entryId"]==entry["id"]
            && v["sourceMaterialId"]==entry["sourceMaterialId"]
            && v["kind"]==entry["kind"] && v["status"]==entry["status"]
            && v["scope"]==entry["scope"]) else {continue;};
        let provenance=&version["companyImport"];
        let metadata=&provenance["metadata"];
        let scope=&provenance["scope"];
        if version["trust"]!="source_only" || text(&version["scope"],"account")!=text(d,"account")
            || !rows(&version["scope"],"postKeys").is_empty()
            || text(provenance,"companyKey")!=company || text(scope,"companyKey")!=company
            || text(metadata,"account")!=company || text(metadata,"platform")!=platform
            || text(metadata,"author_id")!=author || text(version,"text")!=text(metadata,"text")
            || metadata["grantsExecutionAuthority"]!=false
            || text(version,"text").trim().is_empty() {continue;}
        let aliases=rows(scope,"authorAliases");
        if aliases.len()!=1 || text(&aliases[0],"namespace")!="commentops-fast.author-id"
            || text(&aliases[0],"platform")!=platform || text(&aliases[0],"value")!=author {continue;}
        let key=text(provenance,"importKey");
        let digest=text(provenance,"recordSha256");
        if key.is_empty() || digest.len()!=64 || !digest.bytes().all(|b|b.is_ascii_hexdigit())
            || !seen.insert(key.to_owned()) {continue;}
        let created=metadata["created_at"].clone();
        if !created.is_string() && !created.is_i64() && !created.is_u64() {continue;}
        let base=json!({"id":key,"sourceRecordKey":key,"sourceRecordSha256":provenance["recordSha256"],
            "knowledgeEntryId":entry["id"],"knowledgeVersionId":version["id"],
            "sourceType":"legacy_company_import","legacyAuthorAlias":aliases[0],
            "text":bounded(version,"text",12000)?,"sourceCreatedAt":created,
            "historyComplete":false});
        if metadata["statementRole"]=="customer" && metadata["verificationStatus"]=="customer_report" {
            let mut value=base;
            value["claimType"]=json!("customer_statement");
            customers.push(value);
        } else if metadata["statementRole"]=="brand" && metadata["verificationStatus"]=="legacy_reply_evidence"
            && metadata["publicationStatus"]=="verified"
            && metadata["publishedEvidence"]==true {
            let mut value=base;
            value["claimType"]=json!("published_brand_statement");
            value["roleEvidence"]=json!("legacy_verified_publication");
            value["publicationStatus"]=json!("verified");
            published.push(value);
        }
    }
    customers.sort_by_key(|v|(legacy_time_key(&v["sourceCreatedAt"]),text(v,"sourceRecordKey").to_owned()));
    published.sort_by_key(|v|(legacy_time_key(&v["sourceCreatedAt"]),text(v,"sourceRecordKey").to_owned()));
    Ok((customers,published))
}
fn legacy_time_key(value:&Value)->String {
    if let Some(seconds)=value.as_i64() {
        return chrono::DateTime::<chrono::Utc>::from_timestamp(seconds,0)
            .map(|t|t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default();
    }
    value.as_str().unwrap_or("").replace('T'," ").chars().take(19).collect()
}
fn case_time_key(value:&Value)->String {
    if value.get("sourceCreatedAt").is_some(){legacy_time_key(&value["sourceCreatedAt"])}
    else {legacy_time_key(&value["createdAt"])}
}
pub(crate) fn select(d:&Value,items:&[Value])->Result<Value,&'static str> {
    if items.len()>100{return Err("Customer case selection exceeds 100 comments");}
    select_with_catalog(&crate::knowledge::Catalog::new(d)?,items)
}
pub(crate) fn select_with_catalog(catalog:&crate::knowledge::Catalog<'_>,items:&[Value])->Result<Value,&'static str> {
    if items.len()>100{return Err("Customer case selection exceeds 100 comments");}
    let d=catalog.workspace();
    let binding=crate::active_binding(d).map_err(|_|"Customer case account is not configured")?;
    let until=chrono::Utc::now().timestamp_millis();
    let mut result=Vec::new();let mut selected=BTreeSet::new();
    for attached in items {
        let id=text(attached,"id");
        if id.is_empty()||!selected.insert(id){return Err("Invalid customer case target");}
        let target=rows(d,"items").iter().find(|i|i["id"]==id).ok_or("Customer case target is missing")?;
        if !account_matches(d,target)
            || target.get("connectorBinding").is_some_and(|b|b!=&binding.to_json()) {
            return Err("Customer case target binding mismatch");
        }
        let author=text(target,"authorId");let platform=text(target,"platform");
        let mut case=json!({"itemId":id,"accountId":d["account"],"platform":platform,"authorId":author,
            "scope":"account_platform_author","historyComplete":false,"messages":[],"brandReplies":[],
            "priorContractRequests":[],"omittedMessages":0,"omittedBrandReplies":0});
        // Older local fixtures/history may not retain enough provider routing
        // proof. Absence contributes no cross-post evidence; it need not block
        // ordinary preparation from the attached comment's existing context.
        if crate::bound_item(&binding,target).is_err(){
            case["status"]=json!("unsupported_binding");result.push(case);continue;
        }
        if author.is_empty()||platform.is_empty(){
            case["status"]=json!(if author.is_empty(){"missing_author_id"}else{"missing_platform"});
            result.push(case);continue;
        }
        if author.len()>512||platform.len()>100{return Err("Customer case identity exceeds context limit");}
        let (imported_customers,imported_published)=imported_cases(catalog,&binding,author,platform)?;
        let mut history:Vec<&Value>=rows(d,"items").iter().filter(|i| i["id"]!=id
            && i["authorId"]==author && i["platform"]==platform && account_matches(d,i)
            && crate::bound_item(&binding,i).is_ok() && i["providerStatus"]!="deleted"
            && i["textUnavailable"]!=true && i["unavailable"]!=true
            && !text(i,"text").is_empty() && time(i).is_some_and(|at|at<=until)).collect();
        history.sort_by_key(|i|(time(i).unwrap(),text(i,"id")));
        let mut replies=BTreeMap::<(String,String),Value>::new();
        // Resolve only explicit provider reply-to edges. A sibling brand message
        // in the same partial thread is not evidence of a reply to this customer.
        for source in &history {
            let object=text(source,"objectId");let source_id=text(source,"itemId");
            let Some(branch)=rows(d,"branches").iter().find(|b|b["id"]==source["branchId"]&&b["postId"]==source["postId"]) else{continue;};
            for m in rows(branch,"messages") {
                let proof=text(m,"roleEvidence");
                let official=(m["providerOfficial"]==true&&matches!(proof,"provider-official"|"verified-provider-author"))
                    || proof=="provider-official-replies";
                if m["role"]!="brand"||!official||text(m,"authorId").is_empty()
                    || m["providerObjectId"]!=object || text(m,"providerItemId").is_empty()
                    || m["replyToProviderItemId"]!=source_id || m["unavailable"]==true
                    || m["deleted"]==true || m["status"]=="deleted" || text(m,"text").is_empty()
                    || !time(m).is_some_and(|at|at>=time(source).unwrap()&&at<=until) {continue;}
                let key=(object.to_owned(),text(m,"providerItemId").to_owned());
                let projected=json!({"id":text(m,"id"),"sourceItemId":source["id"],
                    "providerItemId":text(m,"providerItemId"),"providerObjectId":object,
                    "inReplyToProviderItemId":source_id,"text":bounded(m,"text",12000)?,
                    "createdAt":bounded(m,"createdAt",100)?,"claimType":"published_brand_statement","roleEvidence":proof});
                if let Some(existing)=replies.get(&key) {
                    if existing!=&projected{return Err("Conflicting customer case reply evidence");}
                } else {replies.insert(key,projected);}
            }
        }
        let mut brand:Vec<Value>=replies.into_values().collect();
        brand.extend(imported_published);
        brand.sort_by_key(|m|(case_time_key(m),text(m,"id").to_owned()));
        // Carry the latest proven contract request even when the general reply
        // window omits it. It is historical speech, not proof of resolution.
        let contract=brand.iter().rev().find(|m|contract_request(text(m,"text"))).map(|m| {
            if m["sourceType"]=="legacy_company_import" {
                json!({"sourceRecordKey":m["sourceRecordKey"],"sourceRecordSha256":m["sourceRecordSha256"],
                    "text":m["text"],"sourceCreatedAt":m["sourceCreatedAt"],
                    "claimType":"published_brand_statement","sourceType":"legacy_company_import"})
            } else {
                json!({"replyId":m["id"],"sourceItemId":m["sourceItemId"],"text":m["text"],"createdAt":m["createdAt"]})
            }
        });
        case["omittedBrandReplies"]=json!(brand.len().saturating_sub(LIMIT));
        let messages:Result<Vec<Value>,&str>=history.iter().map(|i|Ok(json!({
            "itemId":i["id"],"providerItemId":i["itemId"],"providerObjectId":i["objectId"],
            "text":bounded(i,"text",12000)?,"createdAt":bounded(i,"createdAt",100)?,
            "sourceUrl":bounded(i,"sourceUrl",2048)?,"claimType":"customer_statement"}))).collect();
        let mut messages=messages?;
        messages.extend(imported_customers);
        messages.sort_by_key(|m|(case_time_key(m),text(m,"itemId").to_owned(),text(m,"sourceRecordKey").to_owned()));
        case["omittedMessages"]=json!(messages.len().saturating_sub(LIMIT));
        case["messages"]=json!(messages.into_iter().rev().take(LIMIT).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>());
        case["brandReplies"]=json!(brand.into_iter().rev().take(LIMIT).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>());
        case["priorContractRequests"]=json!(contract.into_iter().collect::<Vec<_>>());
        case["status"]=json!("partial_observed_history");result.push(case);
    }
    result.sort_by_key(|c|text(c,"itemId").to_owned());
    Ok(json!(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest,Sha256};
    fn rehash_version(version:&mut Value) {
        let mut payload=version.clone();
        for key in ["id","hash","createdAt"] {payload.as_object_mut().unwrap().remove(key);}
        version["hash"]=json!(format!("{:x}",Sha256::digest(payload.to_string().as_bytes())));
    }
    fn item(id:&str,author:&str,day:u32)->Value {json!({"id":id,"itemId":id,"objectId":"11341",
        "postKey":format!("11341:post-{id}"),"conversationKey":format!("11341:{id}"),"postId":format!("post-{id}"),
        "branchId":format!("branch-{id}"),"authorId":author,"author":"Same display name","platform":"VK",
        "createdAt":format!("2026-09-{day:02}T10:00:00Z"),"text":format!("Customer claim {id}"),"providerStatus":"closed","sourceUrl":"https://vk.test/post"})}
    fn data()->Value {
        let mut d=crate::empty();d["items"]=json!([item("older","author:1",1),item("selected","author:1",22)]);
        d["branches"]=json!([{"id":"branch-older","postId":"post-older","messages":[{
            "id":"brand-reply","providerItemId":"provider-reply","providerObjectId":"11341","authorId":"brand:1",
            "role":"brand","providerOfficial":true,"roleEvidence":"provider-official","replyToProviderItemId":"older",
            "text":"Пришлите, пожалуйста, номер договора.","createdAt":"2026-09-02T10:00:00Z"}]}]);d
    }
    fn context(d:&Value)->Value{select(d,&[json!({"id":"selected"})]).unwrap()}
    fn imported(d:&mut Value,key:&str,role:&str,publication:&str,published:bool) {
        let account=text(d,"account");
        let body=if role=="brand" {"Пожалуйста, пришлите номер договора."} else {"Где узнать статус заказа?"};
        let version_id=format!("version-{key}");
        let entry_id=format!("entry-{key}");
        let value=json!({"companyKey":"likeavto","importKey":key,"recordSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "scope":{"companyKey":"likeavto","authorAliases":[{"namespace":"commentops-fast.author-id","platform":"vk","value":"author:1"}]},
            "metadata":{"account":"likeavto","platform":"vk","author_id":"author:1","text":body,
                "created_at":1790000000,"statementRole":role,"verificationStatus":if role=="customer"{"customer_report"}else{"legacy_reply_evidence"},
                "publicationStatus":publication,"publishedEvidence":published,"grantsExecutionAuthority":false}});
        let entry=json!({"id":entry_id,"currentVersionId":version_id,"sourceMaterialId":format!("material-{key}"),
            "kind":"customer_case","scope":{"account":account,"postKeys":[]},"status":"active"});
        let mut version=json!({"id":version_id,"entryId":entry_id,"sourceMaterialId":entry["sourceMaterialId"],
            "kind":"customer_case","scope":entry["scope"],"status":"active","trust":"source_only",
            "text":body,"companyImport":value});
        rehash_version(&mut version);
        if !d["knowledge_entries"].is_array(){d["knowledge_entries"]=json!([]);}
        if !d["knowledge_versions"].is_array(){d["knowledge_versions"]=json!([]);}
        d["knowledge_entries"].as_array_mut().unwrap().push(entry);
        d["knowledge_versions"].as_array_mut().unwrap().push(version);
    }
    #[test]
    fn imports_only_account_platform_author_scoped_speech_with_publication_proof() {
        let mut d=data();
        imported(&mut d,"customer","customer","",false);
        imported(&mut d,"verified","brand","verified",true);
        imported(&mut d,"uncertain","brand","uncertain",false);
        let c=context(&d);
        assert!(rows(&c[0],"messages").iter().any(|m|m["sourceRecordKey"]=="customer"));
        let brand=rows(&c[0],"brandReplies");
        assert!(brand.iter().any(|m|m["sourceRecordKey"]=="verified"));
        assert!(!brand.iter().any(|m|m["sourceRecordKey"]=="uncertain"));
        let imported=brand.iter().find(|m|m["sourceRecordKey"]=="verified").unwrap();
        assert_eq!(imported["claimType"],"published_brand_statement");
        assert!(imported.get("providerItemId").is_none());
        assert!(imported.get("inReplyToProviderItemId").is_none());
        assert!(rows(&c[0],"priorContractRequests").iter().any(|r|r["sourceRecordKey"]=="verified"));
        assert!(c[0].get("resolutionStatus").is_none());
    }
    #[test]
    fn imported_legacy_alias_never_crosses_connection_or_scopes() {
        let mut d=data();imported(&mut d,"source","customer","",false);
        for mismatch in ["native","account","platform","author","namespace","detached","untrusted"] {
            let mut copy=d.clone();
            match mismatch {
                "native"=>{copy["connectorBinding"]=crate::legacy_binding();copy["connectorBinding"]["connector"]=json!("vk");},
                "account"=>copy["knowledge_versions"][0]["companyImport"]["companyKey"]=json!("baw-russia"),
                "platform"=>copy["knowledge_versions"][0]["companyImport"]["scope"]["authorAliases"][0]["platform"]=json!("instagram"),
                "author"=>copy["knowledge_versions"][0]["companyImport"]["scope"]["authorAliases"][0]["value"]=json!("other"),
                "namespace"=>copy["knowledge_versions"][0]["companyImport"]["scope"]["authorAliases"][0]["namespace"]=json!("native.author"),
                "detached"=>copy["knowledge_entries"][0]["currentVersionId"]=json!("missing"),
                _=>copy["knowledge_versions"][0]["trust"]=json!("verified"),
            }
            if mismatch=="detached" {assert!(select(&copy,&[json!({"id":"selected"})]).is_err());continue;}
            rehash_version(&mut copy["knowledge_versions"][0]);
            if mismatch=="native" {
                let c=select(&copy,&[json!({"id":"selected"})]).unwrap();
                assert!(rows(&c[0],"messages").is_empty());continue;
            }
            assert!(!rows(&context(&copy)[0],"messages").iter().any(|m|m["sourceRecordKey"]=="source"),"{mismatch}");
        }
    }
    #[test]
    fn imported_window_is_bounded_and_record_key_not_text_deduplicates() {
        let mut d=data();
        for n in 0..18 {imported(&mut d,&format!("record-{n:02}"),"customer","",false);}
        let c=context(&d);
        assert_eq!(rows(&c[0],"messages").len(),12);
        assert_eq!(c[0]["omittedMessages"],7);
        assert_eq!(rows(&d,"knowledge_entries").len(),18);
    }
    #[test]
    fn preserves_scoped_customer_and_published_brand_speech_without_claiming_resolution() {
        let d=data();let before=d.clone();let c=context(&d);
        assert_eq!(c[0]["messages"][0]["text"],"Customer claim older");
        assert_eq!(c[0]["messages"][0]["claimType"],"customer_statement");
        assert_eq!(c[0]["brandReplies"][0]["inReplyToProviderItemId"],"older");
        assert_eq!(c[0]["brandReplies"][0]["claimType"],"published_brand_statement");
        assert_eq!(c[0]["priorContractRequests"][0]["replyId"],"brand-reply");
        assert_eq!(c[0]["historyComplete"],false);assert_eq!(d,before);
        assert!(c[0].get("orderStatus").is_none());assert!(c[0]["messages"][0].get("draft").is_none());
        let mut older_target=d.clone();
        older_target["items"][1]["createdAt"]=json!("2026-08-30T10:00:00Z");
        // Reviewing an older comment still needs a later, already observed
        // contract request made to the same customer on another post.
        assert_eq!(context(&older_target),c);
    }
    #[test]
    fn scopes_to_author_platform_binding_account_and_known_past_dates() {
        for change in ["author","missing_author","platform","binding","account","future","undated"] {
            let mut d=data();
            match change {
                "author"=>d["items"][0]["authorId"]=json!("author:other"),
                "missing_author"=>d["items"][1]["authorId"]=Value::Null,
                "platform"=>d["items"][0]["platform"]=json!("Instagram"),
                "binding"=>{d["items"][0]["connectorBinding"]=crate::legacy_binding();d["items"][0]["connectorBinding"]["accountId"]=json!("BAW");},
                "account"=>d["items"][0]["account"]=json!("BAW"),
                "future"=>d["items"][0]["createdAt"]=json!((chrono::Utc::now()+chrono::Duration::days(30)).to_rfc3339()),
                _=>d["items"][0]["createdAt"]=Value::Null,
            }
            let c=context(&d);assert!(rows(&c[0],"messages").is_empty(),"{change}");
            assert!(rows(&c[0],"brandReplies").is_empty(),"{change}");
        }
    }
    #[test]
    fn never_promotes_drafts_or_unrelated_sibling_and_partial_reply_relations() {
        for change in ["sibling","missing_edge","unknown_role","unpublished","missing_author","foreign_object"] {
            let mut d=data();
            d["proposals"]=json!([{"id":"draft","itemId":"older","status":"draft","text":"We already solved everything"}]);
            d["items"][0]["draft"]=json!("Send contract number again");
            let m=&mut d["branches"][0]["messages"][0];
            match change {
                "sibling"=>m["replyToProviderItemId"]=json!("different-customer"),
                "missing_edge"=>{m["replyToProviderItemId"]=Value::Null;m["parentId"]=json!("older");},
                "unknown_role"=>m["roleEvidence"]=Value::Null,
                "unpublished"=>m["providerOfficial"]=json!(false),
                "missing_author"=>m["authorId"]=Value::Null,
                _=>m["providerObjectId"]=json!("other-object"),
            }
            let c=context(&d);assert_eq!(rows(&c[0],"messages").len(),1);
            assert!(rows(&c[0],"brandReplies").is_empty(),"{change}");
            assert!(rows(&c[0],"priorContractRequests").is_empty(),"{change}");
            assert!(!c.to_string().contains("solved everything"));
        }
    }
    #[test]
    fn stable_bounded_windows_retain_an_older_proven_contract_request() {
        let mut d=data();
        for day in 3..=20 {d["items"].as_array_mut().unwrap().push(item(&format!("past-{day}"),"author:1",day));}
        let c=context(&d);assert_eq!(rows(&c[0],"messages").len(),12);
        assert_eq!(c[0]["omittedMessages"],7);
        assert_eq!(c[0]["priorContractRequests"][0]["replyId"],"brand-reply");
        d["items"].as_array_mut().unwrap().reverse();
        assert_eq!(context(&d),c);
    }
    #[test]
    fn missing_routing_proof_is_empty_but_explicit_foreign_binding_is_rejected() {
        let mut d=data();d["items"][1].as_object_mut().unwrap().remove("itemId");
        let c=context(&d);assert_eq!(c[0]["status"],"unsupported_binding");
        assert!(rows(&c[0],"messages").is_empty());assert!(rows(&c[0],"brandReplies").is_empty());
        d["items"][1]["connectorBinding"]=crate::legacy_binding();
        d["items"][1]["connectorBinding"]["accountId"]=json!("BAW");
        assert!(select(&d,&[json!({"id":"selected"})]).is_err());
    }
}
