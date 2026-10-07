use super::*;

fn outcome(result: ApiResult<()>) -> Option<(u16, String)> {
    result.err().map(|error| (error.0.as_u16(), error.1))
}
fn parity(before: &Value, after: &Value) {
    assert_eq!(outcome(validate_change(before, after)), outcome(linear_validate_change(before, after)));
}
fn fixture() -> Value {
    let mut d = crate::empty();
    crate::accounts::initialize(&mut d, crate::accounts::Profile::LikeAvto).unwrap();
    let item = json!({"id":"recipient","objectId":"object","itemId":"external","branchId":"branch",
        "postKey":"post","conversationKey":"thread","connectorBinding":d["connectorBinding"]});
    d["items"] = json!([item]);
    let request = json!({"account":d["account"],"connectorBinding":d["connectorBinding"],"items":d["items"]});
    d["jobs"] = json!([{"id":"paid","kind":"assistant","purpose":"engine_prepare","status":"running",
        "selectedItemIds":["recipient"],"prepareBundle":{"id":"bundle","version":1,"itemIds":["recipient"],
            "request":request,"digest":hash(&request)},
        "preparationStages":{"first":null,"review":null,"groupAdmission":[]}}]);
    d["jobs"][0]["scopeReservation"] = capture(&d, "paid").unwrap();
    d
}

#[test]
fn recipient_keys_uses_existing_company_connector_alias_contract() {
    let d = fixture();
    let binding = crate::active_binding(&d).unwrap();
    assert_eq!(recipient_keys(&d, &d["items"][0]).unwrap(), item_keys(&binding, &d["items"][0]).unwrap());
    let mut foreign = d["items"][0].clone();
    foreign["connectorBinding"] = crate::accounts::Profile::BawRussia.binding();
    assert!(recipient_keys(&d, &foreign).is_err());
    let mut changed = d.clone();
    changed["connectorBinding"]["accountId"] = json!("foreign");
    assert!(recipient_keys(&changed, &d["items"][0]).is_err());
}

#[test]
fn reservation_index_preserves_paid_and_failed_witness_guards_in_large_history() {
    let mut before = fixture();
    before["jobs"][0]["scopeFailure"] = capture_failed_no_result(&before, "paid", "ASSISTANT_BUSY").unwrap();
    before["jobs"][0]["status"] = json!("failed");
    let saved = before["jobs"][0].clone();
    // Retained completed controls are deliberately interleaved with cold rows.
    for n in 0..1024 {
        let mut row = json!({"id":format!("history-{n}"),"kind":"assistant","status":"completed",
            "result":{"cold":"x".repeat(256)}});
        if n % 4 == 0 {
            row["scopeReservation"] = json!({"account":before["account"],"future":{"paid":"retain"}});
            row["scopeFailure"] = json!({"category":"historical","unknown":true});
        }
        before["jobs"].as_array_mut().unwrap().push(row);
    }
    // Put the real protected owner late; its immutable request remains present.
    before["jobs"].as_array_mut().unwrap().swap(0, 1024);
    assert!(validate_change(&before, &before).is_ok());
    parity(&before, &before);
    for mode in ["reservation", "failure", "owner_removed", "request", "company", "projection"] {
        let mut after = before.clone();
        let paid = after["jobs"].as_array_mut().unwrap().iter_mut().find(|job| job["id"] == "paid").unwrap();
        match mode {
            "reservation" => paid["scopeReservation"]["keysDigest"] = json!("forged"),
            "failure" => paid["scopeFailure"]["category"] = json!("unknown"),
            "request" => paid["prepareBundle"]["request"]["items"][0]["itemId"] = json!("retargeted"),
            "owner_removed" => { after["jobs"].as_array_mut().unwrap().retain(|job| job["id"] != "paid"); },
            "company" => after["account"] = json!("BAW Russia"),
            "projection" => after["scopeOwners"] = json!([{"id":"injected"}]),
            _ => unreachable!(),
        }
        assert!(validate_change(&before, &after).is_err(), "planted {mode} mutation must hit a guard");
        parity(&before, &after);
    }
    assert_eq!(before["jobs"][1024], saved, "validation never changes retained evidence");
}

#[test]
fn reservation_index_preserves_first_match_and_any_presence_for_duplicate_ids() {
    let mut before = fixture();
    // A later duplicate carries a saved reservation. The first-match guard
    // must still look at the first after row, while the new-control test sees
    // field presence on ANY prior duplicate.
    let controlled = before["jobs"][0].clone();
    let mut bare = controlled.clone();
    bare.as_object_mut().unwrap().remove("scopeReservation");
    before["jobs"] = json!([bare, controlled]);
    let index = JobHistoryIndex::new(rows(&before, "jobs"));
    assert!(std::ptr::eq(index.first(&json!("paid")).unwrap(), &before["jobs"][0]));
    assert!(index.has_reservation(&json!("paid")));
    for swap in [false, true] {
        let mut after = before.clone();
        if swap { after["jobs"].as_array_mut().unwrap().swap(0, 1); }
        parity(&before, &after);
    }
    let mut after = before.clone();
    after["jobs"][0]["scopeReservation"] = before["jobs"][1]["scopeReservation"].clone();
    assert!(validate_change(&before, &after).is_ok(), "later prior reservation satisfies ANY presence");
    parity(&before, &after);
}

#[test]
fn reservation_index_matches_exact_linear_semantics_for_hostile_ids_and_presence() {
    let ids = [json!("paid"), json!(""), json!(" "), json!(null), json!(17), json!(true),
        json!([]), json!([null,{"large":"x".repeat(8192)}]), json!({"nested":[1,2]}), json!(17.0)];
    for id in &ids {
        for missing in [false, true] {
            for failure in [false, true] {
                let mut before = fixture();
                before["jobs"][0]["id"] = id.clone();
                if missing { before["jobs"][0].as_object_mut().unwrap().remove("id"); }
                if failure { before["jobs"][0]["scopeFailure"] = json!(null); }
                let mut duplicate = before["jobs"][0].clone();
                duplicate.as_object_mut().unwrap().remove("scopeReservation");
                before["jobs"].as_array_mut().unwrap().insert(0, duplicate);
                for mutation in 0..5 {
                    let mut after = before.clone();
                    match mutation {
                        1 => after["jobs"].as_array_mut().unwrap().swap(0, 1),
                        2 => after["jobs"][0]["scopeReservation"] = before["jobs"][1]["scopeReservation"].clone(),
                        3 => after["jobs"][1]["scopeReservation"] = json!(null),
                        4 => after["jobs"].as_array_mut().unwrap().clear(),
                        _ => {},
                    }
                    parity(&before, &after);
                }
            }
        }
    }
    for jobs in [Value::Null, json!({}), json!([null,1,false,[],{}]), json!([])] {
        let before = json!({"jobs":jobs});
        parity(&before, &before);
    }
}

#[test]
fn reservation_index_keeps_new_failure_binding_validation_and_known_terminal_transition() {
    let before = fixture();
    let mut after = before.clone();
    after["jobs"][0]["scopeFailure"] = capture_failed_no_result(&after, "paid", "ASSISTANT_BUSY").unwrap();
    assert!(validate_change(&before, &after).is_ok());
    parity(&before, &after);
    for mode in ["category", "status", "witness"] {
        let mut forged = after.clone();
        match mode {
            "category" => forged["jobs"][0]["scopeFailure"]["category"] = json!("unknown"),
            "status" => forged["jobs"][0]["status"] = json!("interrupted"),
            _ => forged["jobs"][0]["scopeFailure"] = json!(null),
        }
        assert!(validate_change(&before, &forged).is_err());
        parity(&before, &forged);
    }
    let mut terminal = after.clone();
    terminal["jobs"][0]["status"] = json!("failed");
    assert!(validate_change(&after, &terminal).is_ok());
    parity(&after, &terminal);
}

// Linear lookup oracle based on preimage SHA256
// 1105667fc64b3b622b5495c7e299158e88af6ccb6eae382a2c87d3ea5211d7cf.
// Domain guards include unpaid material refinement; only lookup remains linear.
fn linear_validate_change(before: &Value, after: &Value) -> ApiResult<()> {
    if before.get("scopeOwners") != after.get("scopeOwners")
        || before.get("scopeProposals") != after.get("scopeProposals")
    {
        return Err(conflict("Preparation ownership projection is read-only"));
    }
    for old in rows(before, "jobs") {
        if let Some(witness) = old.get("scopeFailure") {
            let next = rows(after, "jobs")
                .iter()
                .find(|j| j["id"] == old["id"])
                .ok_or_else(|| conflict("Preparation failure witness owner disappeared"))?;
            if next.get("scopeFailure") != Some(witness) {
                return Err(conflict("Preparation failure release witness is immutable"));
            }
        }
        if let Some(saved) = old.get("scopeReservation") {
            let next = rows(after, "jobs")
                .iter()
                .find(|j| j["id"] == old["id"])
                .ok_or_else(|| conflict("Preparation reservation owner disappeared"))?;
            if next.get("scopeReservation") != Some(saved)
                || saved["account"] != after["account"]
                || (next["prepareBundle"] != old["prepareBundle"]
                    && captured(after, next)? != *saved)
            {
                unpaid_material_refinement(after,old,next)?;
            }
        }
    }
    for job in rows(after, "jobs") {
        if let Some(witness) = job.get("scopeFailure") {
            if !rows(before, "jobs")
                .iter()
                .any(|old| old["id"] == job["id"] && old.get("scopeFailure").is_some())
            {
                let expected = capture_failed_no_result(
                    after,
                    string(job, "id")?,
                    string(witness, "category")?,
                )?;
                if !matches!(job["status"].as_str(), Some("running" | "failed"))
                    || expected != *witness
                {
                    return Err(conflict(
                        "Preparation failure release witness binding changed",
                    ));
                }
            }
        }
        if let Some(saved) = job.get("scopeReservation") {
            if !rows(before, "jobs")
                .iter()
                .any(|old| old["id"] == job["id"] && old.get("scopeReservation").is_some())
            {
                if captured(after, job)? != *saved {
                    return Err(conflict("Preparation scope reservation binding changed"));
                }
                check(after, saved, Some(string(job, "id")?))?;
            }
        }
    }
    Ok(())
}
