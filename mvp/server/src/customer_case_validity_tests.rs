// Candidate test text only. Included inside customer_case_context::tests.
fn validity_time(raw:&str)->chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(raw).unwrap().with_timezone(&chrono::Utc)
}
fn validity_context(d:&Value,at:&str)->Result<Value,&'static str> {
    select_with_catalog_at(&crate::knowledge::Catalog::new(d)?,&[json!({"id":"selected"})],validity_time(at))
}
fn validity_has_import(context:&Value,key:&str,field:&str)->bool {
    rows(&context[0],field).iter().any(|row|row["sourceRecordKey"]==key)
}
fn revise_validity(d:&mut Value,body:Value,at:&str) {
    let entry=d["knowledge_entries"][0].clone();
    let mut body=body;body["expectedVersionId"]=entry["currentVersionId"].clone();
    crate::knowledge::revise(d,entry["id"].as_str().unwrap(),&body,at).unwrap();
}

#[test]
fn imported_customer_case_revisions_observe_inclusive_from_and_exclusive_until() {
    let from="2026-09-25T10:00:00.123Z";let until="2026-09-25T10:00:00.125Z";
    for (role,field) in [("customer","messages"),("brand","brandReplies")] {
        let mut d=data();imported(&mut d,"window",role,"verified",role=="brand");
        // Exercise the supported reducer, not a stale/fabricated catalog hash.
        revise_validity(&mut d,json!({"validFrom":from,"validUntil":until}),"2026-09-24T00:00:00Z");
        let saved=d.clone();
        for (at,expected) in [("2026-09-25T10:00:00.122Z",false),(from,true),
            ("2026-09-25T13:00:00.124+03:00",true),(until,false),("2026-09-25T10:00:00.126Z",false)] {
            let selected=validity_context(&d,at).unwrap();
            assert_eq!(validity_has_import(&selected,"window",field),expected,"{role} at {at}");
            if role=="brand" {
                assert_eq!(validity_has_import(&selected,"window","priorContractRequests"),expected,
                    "an expired/future imported brand statement cannot survive as a contract pin");
            }
            // Knowledge windows do not rewrite or suppress valid local history.
            assert!(rows(&selected[0],"messages").iter().any(|row|row["itemId"]=="older"));
        }
        assert_eq!(d,saved);
    }
}

#[test]
fn reused_catalog_reevaluates_customer_case_time_on_every_selection() {
    let mut d=data();imported(&mut d,"window","customer","",false);
    revise_validity(&mut d,json!({"validFrom":"2026-09-24T00:00:00Z","validUntil":"2026-09-25T00:00:00Z"}),
        "2026-09-24T00:00:00Z");
    let catalog=crate::knowledge::Catalog::new(&d).unwrap();
    let ids=[json!({"id":"selected"})];
    let before=select_with_catalog_at(&catalog,&ids,validity_time("2026-09-24T23:59:59.999Z")).unwrap();
    let after=select_with_catalog_at(&catalog,&ids,validity_time("2026-09-25T00:00:00Z")).unwrap();
    assert!(validity_has_import(&before,"window","messages"));
    assert!(!validity_has_import(&after,"window","messages"));
}

#[test]
fn imported_case_window_matches_generic_optional_and_malformed_timestamp_behavior() {
    for field in ["validFrom","validUntil"] {
        for optional in [Value::Null,json!(123),json!({"legacy":true})] {
            let mut d=data();imported(&mut d,"optional","customer","",false);
            d["knowledge_versions"][0][field]=optional;
            rehash_version(&mut d["knowledge_versions"][0]);
            assert!(validity_has_import(&validity_context(&d,"2026-09-25T00:00:00Z").unwrap(),"optional","messages"));
        }
        let mut d=data();imported(&mut d,"malformed","customer","",false);
        d["knowledge_versions"][0][field]=json!("not a timestamp");
        rehash_version(&mut d["knowledge_versions"][0]);
        assert_eq!(validity_context(&d,"2026-09-25T00:00:00Z"),Err("Invalid knowledge timestamp"));
    }
    let mut absent=data();imported(&mut absent,"absent","customer","",false);
    assert!(validity_has_import(&validity_context(&absent,"2026-09-25T00:00:00Z").unwrap(),"absent","messages"));
}

#[test]
fn unrelated_foreign_author_or_company_case_does_not_inject_timestamp_failure() {
    for mismatch in ["author","company"] {
        let mut d=data();imported(&mut d,"foreign","customer","",false);
        if mismatch=="author" {
            d["knowledge_versions"][0]["companyImport"]["metadata"]["author_id"]=json!("author:other");
        } else {d["knowledge_versions"][0]["companyImport"]["companyKey"]=json!("baw-russia");}
        d["knowledge_versions"][0]["validUntil"]=json!("not a timestamp");
        rehash_version(&mut d["knowledge_versions"][0]);
        assert!(!validity_has_import(&validity_context(&d,"2026-09-25T00:00:00Z").unwrap(),"foreign","messages"),"{mismatch}");
    }
}

#[test]
fn case_validity_does_not_introduce_a_new_historical_speech_date_filter() {
    let mut d=data();imported(&mut d,"speech","customer","",false);
    d["knowledge_versions"][0]["companyImport"]["metadata"]["created_at"]=json!("2030-01-01T00:00:00Z");
    rehash_version(&mut d["knowledge_versions"][0]);
    let selected=validity_context(&d,"2026-09-25T00:00:00Z").unwrap();
    let speech=rows(&selected[0],"messages").iter().find(|row|row["sourceRecordKey"]=="speech").unwrap();
    assert_eq!(speech["sourceCreatedAt"],"2030-01-01T00:00:00Z");
}
