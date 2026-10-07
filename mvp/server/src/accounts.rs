//! Explicit single-account workspace identity shared by the CLI and website.
//! A second account uses a separate database; changing a populated workspace is
//! never an account-switch operation.
use crate::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Profile { LikeAvto, BawRussia }

impl Profile {
    pub(crate) fn parse(value: &str) -> ApiResult<Self> {
        match value {
            "likeavto" => Ok(Self::LikeAvto),
            "baw-russia" => Ok(Self::BawRussia),
            _ => Err(bad("Unknown engine account")),
        }
    }
    pub(crate) fn from_workspace(data: &Value) -> ApiResult<Self> {
        match data["account"].as_str() {
            Some("LikeAvto") => Ok(Self::LikeAvto),
            Some("BAW Russia") => Ok(Self::BawRussia),
            _ => Err(conflict("Account has no configured connector")),
        }
    }
    pub(crate) fn key(self) -> &'static str {
        match self { Self::LikeAvto => "likeavto", Self::BawRussia => "baw-russia" }
    }
    pub(crate) fn display(self) -> &'static str {
        match self { Self::LikeAvto => "LikeAvto", Self::BawRussia => "BAW Russia" }
    }
    pub(crate) fn binding(self) -> Value {
        json!({"id":format!("angryspace-{}-v1",self.key()),"workspaceId":"local-pilot",
            "accountId":self.display(),"connector":"angryspace","revision":1,
            "providerAccountId":self.key()})
    }
    pub(crate) fn bind_request(self, request: &mut Value) -> ApiResult<()> {
        if !request.is_object() { return Err(bad("Bridge request must be an object")); }
        if request.get("account").is_some_and(|value| {
            value.as_str().is_none_or(|value| value != self.key() && value != self.display())
        }) {
            return Err(conflict("Bridge account differs from workspace"));
        }
        request["account"] = json!(self.key());
        Ok(())
    }
}

pub(crate) fn initialize(data: &mut Value, selected: Profile) -> ApiResult<()> {
    if Profile::from_workspace(data)? != selected {
        let pristine = data.get("connectorBinding").is_none()
            && ["items","posts","branches","conversations","proposals","approvals",
                "operations","materials","jobs","audit"].iter()
                .all(|key| data[*key].as_array().is_some_and(Vec::is_empty))
            && data.as_object().is_some_and(|object| object.values()
                .all(|value| value.as_array().is_none_or(Vec::is_empty)));
        if !pristine { return Err(conflict("Database belongs to another account; use a separate data directory/database")); }
        data["account"] = json!(selected.display());
        data["settings"]["provider"] = json!(selected.display());
    }
    if let Some(value) = data.get("connectorBinding") {
        // Account identity survives connector replacement. Only the legacy
        // compatibility case below supplies an Angry.Space default; existing
        // routes and UNKNOWN evidence are never retargeted during startup.
        let binding = ConnectorBinding::from_json(value).map_err(|e| conflict(e.0))?;
        binding.validate_scope("local-pilot", selected.display()).map_err(|e| conflict(e.0))?;
    } else {
        data["connectorBinding"] = selected.binding();
    }
    data["settings"]["concurrencyBoundary"] = json!("One workspace owner, parallel independent actions, serialized item/conversation conflicts; ambiguous effects require readback.");
    Ok(())
}

pub(crate) async fn status(State(app): State<App>) -> ApiResult<Json<Value>> {
    let data = app.db.read_engine_status().await?;
    let profile = Profile::from_workspace(&data)?;
    active_binding(&data)?;
    let count = |collection: &str, field: &str, state: &str| list(&data, collection)
        .iter().filter(|row| row[field] == state).count();
    Ok(Json(json!({"version":1,"account":profile.key(),"displayAccount":profile.display(),
        "storageGeneration":data["storageGeneration"],
        "externalWrites":app.external_writes,"authority":"shared-rust-engine",
        "prepareScopeReservations":{"version":1},
        "strictGrouping":{"version":1,"contract":crate::preparation_unit::CONTRACT},
        "prepareWorkers":{"version":1,"maxWorkers":app.preparation_workers.width()},
        "capabilities":{"read":true,"prepare":true,"exactApproval":true,"reconcile":true,
            "autonomousApproval":"explicit-client-run-only","restoreDeleted":false},
        "counts":data.get("counts").cloned().unwrap_or_else(||json!({"items":list(&data,"items").len(),"prepared":count("items","workflow","prepared"),
            "attention":count("items","workflow","attention"),"unknown":count("operations","status","unknown"),
            "dispatching":count("operations","status","dispatching")}))})))
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn bridge_identity_is_pinned_without_reading_full_workspace() {
        for profile in [Profile::LikeAvto, Profile::BawRussia] {
            for account in [profile.key(), profile.display()] {
                let mut request=json!({"account":account,"operation":"context"});
                profile.bind_request(&mut request).unwrap();
                assert_eq!(request["account"],profile.key());
            }
            for invalid in [json!({"account":"foreign"}),json!({"account":null}),json!([])] {
                assert!(profile.bind_request(&mut invalid.clone()).is_err());
            }
        }
        let mut foreign=json!({"account":"baw-russia"});
        assert!(Profile::LikeAvto.bind_request(&mut foreign).is_err());
    }
    #[test] fn cached_knowledge_alone_also_prevents_account_relabel() {
        let mut data=empty();data["knowledge_entries"]=json!([{"id":"old-account-rule"}]);
        let before=data.clone();
        assert!(initialize(&mut data,Profile::BawRussia).is_err());
        assert_eq!(data,before);
    }
    #[test] fn only_supported_accounts_have_distinct_bindings() {
        assert!(Profile::parse("other").is_err());
        assert_ne!(Profile::LikeAvto.binding(),Profile::BawRussia.binding());
        for profile in [Profile::LikeAvto,Profile::BawRussia] {
            let mut data=empty();initialize(&mut data,profile).unwrap();
            assert_eq!(bridge_account(&active_binding(&data).unwrap()).unwrap(),profile.key());
            assert_eq!(data["settings"]["provider"],profile.display());
        }
    }
    #[test] fn account_switch_never_relabels_existing_data_or_a_bound_empty_workspace() {
        let mut data=empty();data["items"]=json!([{"id":"customer"}]);let before=data.clone();
        assert!(initialize(&mut data,Profile::BawRussia).is_err());assert_eq!(data,before);
        let mut data=empty();initialize(&mut data,Profile::BawRussia).unwrap();let before=data.clone();
        assert!(initialize(&mut data,Profile::LikeAvto).is_err());assert_eq!(data,before);
    }
    #[test] fn binding_cannot_cross_account_or_guess_unknown_connection() {
        let mut data=empty();data["connectorBinding"]=Profile::BawRussia.binding();
        assert!(active_binding(&data).is_err());
        let mut value=Profile::BawRussia.binding();value["revision"]=json!(2);
        let binding=ConnectorBinding::from_json(&value).unwrap();
        assert!(bridge_account(&binding).is_err());
    }
    #[test] fn initialization_preserves_a_scoped_native_connector_without_enabling_transport() {
        let mut data=empty();
        let binding=json!({"id":"native-vk-account","workspaceId":"local-pilot","accountId":"LikeAvto",
            "connector":"vk","revision":17,"providerAccountId":"native-account-id"});
        data["connectorBinding"]=binding.clone();
        data["operations"]=json!([{"id":"old-unknown","status":"unknown","target":{"connectorBinding":Profile::LikeAvto.binding()}}]);
        let operations=data["operations"].clone();
        initialize(&mut data,Profile::LikeAvto).unwrap();
        assert_eq!(data["connectorBinding"],binding);
        assert_eq!(data["operations"],operations);
        assert!(bridge_account(&active_binding(&data).unwrap()).is_err());
    }
    #[test] fn initialization_rejects_foreign_or_corrupt_bindings_without_repairing_them() {
        for value in [Profile::BawRussia.binding(),json!({"connector":"vk"}),Value::Null] {
            let mut data=empty();data["connectorBinding"]=value;let before=data.clone();
            assert!(initialize(&mut data,Profile::LikeAvto).is_err());
            assert_eq!(data,before);
        }
    }
}
