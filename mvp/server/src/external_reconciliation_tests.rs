use super::*;

fn fixture()->(Value,Value,Value) {
    let mut d=crate::connection_gate::tests::workspace();
    let target=crate::connection_gate::tests::operation(&d,1)["target"].clone();
    let binding=active_binding(&d).unwrap().to_json();
    let source=json!({"sourceSha256":"b".repeat(64),"archiveManifestHash":"c".repeat(64),"lineSha256":"d".repeat(64)});
    let safety=json!({"mode":"archive_fence","epoch":1,
        "safetyFence":{"companyId":"likeavto","connectionScope":binding,"archiveManifestHash":"c".repeat(64),
            "sourceBindingRefs":[source],"predicate":"all_nonempty_raw_aliases_702","coverage":{"complete":true,"declaredRows":1648},"sealedAt":now()},
        "sealedSourceRefs":[{"sha256":"b".repeat(64)}],"archiveRefs":[{"sha256":"c".repeat(64)}],
        "recipientReservations":[{"connectionScope":binding,"alias":{"namespace":"provider_item","value":"external-1"},
            "origin":"external_manual","evidenceClass":"possible_effect","disposition":"excluded","sourceRefs":[source]}],
        "quarantineCoverage":{"complete":true,"namespaces":[]}});
    crate::connection_gate::request_close(&mut d,"archive","clean_start").unwrap();
    crate::connection_gate::finalize_close(&mut d,"archive").unwrap();
    (d,safety,target)
}

#[test]
fn archive_fence_is_idempotent_and_external_alias_survives_new_local_identity() {
    let (mut d,safety,mut target)=fixture();let epoch=d[crate::connection_gate::FIELD]["gateEpoch"].as_u64().unwrap();
    install(&mut d,&safety,epoch).unwrap();
    let before=d.clone();assert_eq!(install(&mut d,&safety,epoch).unwrap()["replayed"],true);assert_eq!(d,before);
    target["id"]=json!("fresh-working-db-id");assert!(fence_recipient(&d,&target,true).is_err());
    target["itemId"]=json!("new-current-external-item");assert!(fence_recipient(&d,&target,true).is_ok());
    assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
}

#[test]
fn incomplete_scope_coverage_and_reference_identity_fail_closed() {
    let (d,safety,target)=fixture();
    for field in ["archiveManifestHash","connectionScope"] {
        let mut bad=safety.clone();bad["safetyFence"][field]=Value::Null;assert!(validate_value(&d,&bad).is_err());
    }
    let mut missing=safety.clone();missing["quarantineCoverage"]["complete"]=json!(false);
    assert!(validate_value(&d,&missing).is_err());
    let mut empty=d.clone();empty.as_object_mut().unwrap().remove(FIELD);
    assert!(fence_recipient(&empty,&target,true).is_err());
}

#[test]
fn fence_activation_cannot_race_an_open_gate_or_drop_reservations_at_budget_overflow() {
    let (mut d,safety,_)=fixture();let epoch=d[crate::connection_gate::FIELD]["gateEpoch"].as_u64().unwrap();
    d[crate::connection_gate::FIELD]["state"]=json!("open");let before=d.clone();
    assert!(install(&mut d,&safety,epoch).is_err());assert_eq!(d,before);
    let mut huge=safety.clone();huge["recipientReservations"]=json!((0..MAX_RESERVATIONS+1).map(|n|{
        let mut row=safety["recipientReservations"][0].clone();row["alias"]["value"]=json!(format!("raw-{n}"));row
    }).collect::<Vec<_>>());
    assert!(validate_value(&d,&huge).is_err());
}

#[test]
fn ambiguous_namespace_is_held_even_with_empty_new_operation_history() {
    let (mut d,mut safety,mut target)=fixture();
    safety["recipientReservations"]=json!([]);
    safety["quarantineCoverage"]["namespaces"]=json!([{"connectionScope":active_binding(&d).unwrap().to_json(),
        "reason":"unmapped_targetless_attempt","sourceRefs":safety["safetyFence"]["sourceBindingRefs"]}]);
    d[FIELD]=safety;target["id"]=json!("new-id");
    assert!(fence_recipient(&d,&target,true).is_err());
    assert!(list(&d,"operations").is_empty());
}

#[test]
fn replacement_cannot_prune_holds_or_change_their_raw_archive_identity() {
    let (mut d,safety,_)=fixture();let epoch=d[crate::connection_gate::FIELD]["gateEpoch"].as_u64().unwrap();
    install(&mut d,&safety,epoch).unwrap();let before=d.clone();
    let mut removed=safety.clone();removed["epoch"]=json!(2);removed["recipientReservations"]=json!([]);
    assert!(install(&mut d,&removed,epoch).is_err());assert_eq!(d,before);
    let mut foreign=safety.clone();foreign["recipientReservations"][0]["sourceRefs"][0]["sourceSha256"]=json!("e".repeat(64));
    assert!(validate_value(&d,&foreign).is_err());
}
