//! One-statement, selected-item review projection. The PostgreSQL reader pool is
//! read-only; this module never claims a writer lease or dispatches an action.
use super::*;
use serde_json::json;

const PG_REVIEW: &str = r#"
WITH selected AS (
  SELECT id,payload FROM communityhero.items WHERE workspace_id=$1 AND id=ANY($2)
)
SELECT w.execution_enabled,
       (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
        AND w.account=w.metadata->>'account' AND w.account=$3) IS TRUE AS identity_valid,
       jsonb_build_object(
         'account',w.account,
         'items',COALESCE((SELECT jsonb_agg(i.payload ORDER BY i.ordinal)
           FROM communityhero.items i WHERE i.workspace_id=w.id AND i.id=ANY($2)),'[]'::jsonb),
         'posts',COALESCE((SELECT jsonb_agg(p.payload ORDER BY p.ordinal)
           FROM communityhero.posts p WHERE p.workspace_id=w.id AND p.id IN
             (SELECT i.post_id FROM communityhero.items i WHERE i.workspace_id=w.id AND i.id=ANY($2))),'[]'::jsonb),
         'branches',COALESCE((SELECT jsonb_agg(b.payload-'observedMessages' ORDER BY b.ordinal)
           FROM communityhero.branches b WHERE b.workspace_id=w.id AND b.id IN
             (SELECT i.branch_id FROM communityhero.items i WHERE i.workspace_id=w.id AND i.id=ANY($2))),'[]'::jsonb),
         'proposals',COALESCE((SELECT jsonb_agg(p.payload ORDER BY p.ordinal)
           FROM communityhero.proposals p WHERE p.workspace_id=w.id AND p.item_id=ANY($2)),'[]'::jsonb),
         'operations',COALESCE((SELECT jsonb_agg(o.payload-'dispatchAuthority' ORDER BY o.ordinal)
           FROM communityhero.operations o WHERE o.workspace_id=w.id AND
             (o.item_id=ANY($2) OR EXISTS(SELECT 1 FROM selected s WHERE
               o.payload->'target'->>'objectId'=s.payload->>'objectId' AND
               o.payload->'target'->>'itemId'=s.payload->>'itemId' AND
               jsonb_typeof(o.payload->'target'->'connectorBinding')='object' AND
               ((o.payload->'target'->'connectorBinding')-'revision')=((s.payload->'connectorBinding')-'revision')))),'[]'::jsonb),
         'ambiguousOperationCount',(SELECT count(*) FROM communityhero.operations o WHERE o.workspace_id=w.id
           AND COALESCE(NOT (o.item_id=ANY($2)),true) AND EXISTS(SELECT 1 FROM selected s WHERE
             o.payload->'target'->>'objectId'=s.payload->>'objectId' AND
             o.payload->'target'->>'itemId'=s.payload->>'itemId' AND
             (o.payload->'target'->'connectorBinding'->>'accountId' IS NULL OR
              o.payload->'target'->'connectorBinding'->>'accountId'=s.payload->'connectorBinding'->>'accountId') AND
             (jsonb_typeof(o.payload->'target'->'connectorBinding') IS DISTINCT FROM 'object' OR
              ((o.payload->'target'->'connectorBinding')-'revision') IS DISTINCT FROM ((s.payload->'connectorBinding')-'revision'))))
       )::text AS projection
FROM communityhero.workspaces w WHERE w.id=$1
"#;

// SQLite stores one workspace document; JSON1 filters before returning data to
// Rust. As with other SQLite projections, the JSON document itself is scanned.
const SQLITE_REVIEW: &str = r#"
WITH selected AS MATERIALIZED (
  SELECT i.key AS ordinal, i.value,
    json_extract(i.value,'$.id') AS id,
    json_extract(i.value,'$.postId') AS post_id,
    json_extract(i.value,'$.branchId') AS branch_id,
    json_extract(i.value,'$.objectId') AS object_id,
    json_extract(i.value,'$.itemId') AS provider_item_id,
    json_extract(i.value,'$.connectorBinding.id') AS binding_id,
    json_extract(i.value,'$.connectorBinding.workspaceId') AS workspace_id,
    json_extract(i.value,'$.connectorBinding.accountId') AS account_id,
    json_extract(i.value,'$.connectorBinding.connector') AS connector,
    json_extract(i.value,'$.connectorBinding.providerAccountId') AS provider_account_id
  FROM workspace w, json_each(w.payload,'$.items') i
  WHERE w.id=1 AND EXISTS(SELECT 1 FROM json_each(?1) ids WHERE ids.value=json_extract(i.value,'$.id'))
)
SELECT json_object(
  'account',json_extract(w.payload,'$.account'),
  'items',json(COALESCE((SELECT json_group_array(json(i.value)) FROM (SELECT value FROM selected ORDER BY ordinal) i),'[]')),
  'posts',json(COALESCE((SELECT json_group_array(json(p.value)) FROM json_each(w.payload,'$.posts') p
    WHERE EXISTS(SELECT 1 FROM selected i WHERE i.post_id=json_extract(p.value,'$.id'))),'[]')),
  'branches',json(COALESCE((SELECT json_group_array(json_remove(b.value,'$.observedMessages')) FROM json_each(w.payload,'$.branches') b
    WHERE EXISTS(SELECT 1 FROM selected i WHERE i.branch_id=json_extract(b.value,'$.id'))),'[]')),
  'proposals',json(COALESCE((SELECT json_group_array(json(p.value)) FROM json_each(w.payload,'$.proposals') p
    WHERE EXISTS(SELECT 1 FROM json_each(?1) ids WHERE ids.value=json_extract(p.value,'$.itemId'))),'[]')),
  'operations',json(COALESCE((SELECT json_group_array(json_remove(o.value,'$.dispatchAuthority')) FROM json_each(w.payload,'$.operations') o
    WHERE EXISTS(SELECT 1 FROM selected
      WHERE json_extract(o.value,'$.itemId')=selected.id
         OR (json_extract(o.value,'$.target.objectId')=selected.object_id
             AND json_extract(o.value,'$.target.itemId')=selected.provider_item_id
             AND json_extract(o.value,'$.target.connectorBinding.id')=selected.binding_id
             AND json_extract(o.value,'$.target.connectorBinding.workspaceId')=selected.workspace_id
             AND json_extract(o.value,'$.target.connectorBinding.accountId')=selected.account_id
             AND json_extract(o.value,'$.target.connectorBinding.connector')=selected.connector
             AND json_extract(o.value,'$.target.connectorBinding.providerAccountId')=selected.provider_account_id))),'[]')),
  'ambiguousOperationCount',(SELECT count(*) FROM json_each(w.payload,'$.operations') o
    WHERE NOT EXISTS(SELECT 1 FROM json_each(?1) ids WHERE ids.value=json_extract(o.value,'$.itemId'))
      AND EXISTS(SELECT 1 FROM selected
        WHERE json_extract(o.value,'$.target.objectId')=selected.object_id
          AND json_extract(o.value,'$.target.itemId')=selected.provider_item_id
          AND (json_extract(o.value,'$.target.connectorBinding.accountId') IS NULL OR
            json_extract(o.value,'$.target.connectorBinding.accountId')=selected.account_id)
          AND (json_extract(o.value,'$.target.connectorBinding.id') IS NOT selected.binding_id
            OR json_extract(o.value,'$.target.connectorBinding.workspaceId') IS NOT selected.workspace_id
            OR json_extract(o.value,'$.target.connectorBinding.connector') IS NOT selected.connector
            OR json_extract(o.value,'$.target.connectorBinding.providerAccountId') IS NOT selected.provider_account_id)))
) AS projection FROM workspace w WHERE w.id=1
"#;

impl Database {
    /// Full current records for selected items, including every operation and
    /// UNKNOWN receipt. No history limit or status filter is applied.
    pub(crate) async fn read_bounded_review(
        &self,
        ids: &[String],
        account: &str,
    ) -> ApiResult<Value> {
        if ids.is_empty() || ids.len() > 100 {
            return Err(crate::bad("Select between 1 and 100 items"));
        }
        let unique: HashSet<&str> = ids.iter().map(String::as_str).collect();
        if unique.len() != ids.len() || ids.iter().any(|id| id.is_empty() || id.len() > 128) {
            return Err(crate::bad("Invalid or duplicate selected item ID"));
        }
        let payload = match self {
            Self::Sqlite(pool) => {
                sqlx::query_scalar::<_, String>(SQLITE_REVIEW)
                    .bind(serde_json::to_string(ids).map_err(|_| internal("Invalid item IDs"))?)
                    .fetch_one(pool)
                    .await?
            }
            Self::Postgres { reader: pool, .. } => {
                let record = sqlx::query(PG_REVIEW)
                    .bind(WORKSPACE)
                    .bind(ids)
                    .bind(account)
                    .fetch_one(pool)
                    .await?;
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                if !record.try_get::<bool, _>("identity_valid")? {
                    return Err(internal("Workspace identity mismatch"));
                }
                record.try_get::<String, _>("projection")?
            }
        };
        let mut view = parse(&payload)?;
        if view["account"] != account {
            return Err(internal("Workspace account mismatch"));
        }
        let items = view["items"]
            .as_array()
            .ok_or_else(|| internal("Invalid item projection"))?;
        let returned: HashSet<&str> = items
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect();
        if returned.len() != ids.len()
            || items.len() != ids.len()
            || ids.iter().any(|id| !returned.contains(id.as_str()))
        {
            return Err(crate::ApiError(
                crate::StatusCode::NOT_FOUND,
                "Selected item missing or duplicated".into(),
            ));
        }
        let item_count = items.len();
        let operation_count = view["operations"]
            .as_array()
            .ok_or_else(|| internal("Invalid operation projection"))?
            .len();
        let ambiguous=view["ambiguousOperationCount"].as_u64()
            .ok_or_else(||internal("Invalid operation coverage"))?;
        view.as_object_mut().unwrap().remove("ambiguousOperationCount");
        view["selectedItemIds"] = json!(ids);
        view["coverage"] = json!({"itemsReturned":item_count,"operationsReturned":operation_count,
            "operationsComplete":ambiguous==0,"historyTruncated":false});
        if ambiguous>0{view["coverage"]["ambiguousOperationCount"]=json!(ambiguous);}
        Ok(view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (Database, tempfile::TempDir, Value) {
        let folder = tempfile::tempdir().unwrap();
        let pool = crate::open_db(&folder.path().join("workspace.sqlite"))
            .await
            .unwrap();
        let mut data = crate::empty();
        data["items"] = json!([
            {"id":"selected","postId":"post","branchId":"branch","revision":8,
             "contextEvidenceDigest":"evidence-eight","branchContextDigest":"branch-eight","draft":"current"},
            {"id":"other","postId":"other-post","branchId":"other-branch","revision":1,
             "draft":"private unrelated draft"}
        ]);
        data["posts"] = json!([{"id":"post","text":"current post"},
            {"id":"other-post","text":"unrelated".repeat(100_000)}]);
        data["branches"] = json!([{"id":"branch","postId":"post","messages":[{"id":"m","text":"current branch"}],
            "observedMessages":[{"private":"internal evidence"}]},
            {"id":"other-branch","postId":"other-post","messages":[{"text":"unrelated".repeat(100_000)}]}]);
        data["proposals"] = json!([{"id":"p","itemId":"selected","kind":"reply","text":"current proposal","revision":3},
            {"id":"other-p","itemId":"other","text":"unrelated proposal"}]);
        data["operations"] = json!([
            {"id":"op-unknown","itemId":"selected","status":"unknown","evidence":{"receipt":"uncertain"},
             "dispatchAuthority":{"private":"server authority"}},
            {"id":"op-success","itemId":"selected","status":"succeeded","evidence":{"receipt":"verified"}},
            {"id":"other-op","itemId":"other","status":"failed","evidence":{"receipt":"unrelated"}}
        ]);
        data["jobs"] = json!([{"id":"huge-job","result":{"private":"unrelated".repeat(100_000)}}]);
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1")
            .bind(data.to_string())
            .execute(&pool)
            .await
            .unwrap();
        (Database::Sqlite(pool), folder, data)
    }

    #[tokio::test]
    async fn selected_review_preserves_stale_evidence_and_all_operation_outcomes() {
        let (db, _folder, data) = fixture().await;
        let view = db
            .read_bounded_review(&["selected".into()], "LikeAvto")
            .await
            .unwrap();
        assert_eq!(view["items"], json!([data["items"][0]]));
        assert_eq!(view["posts"], json!([data["posts"][0]]));
        let mut branch = data["branches"][0].clone();
        branch.as_object_mut().unwrap().remove("observedMessages");
        assert_eq!(view["branches"], json!([branch]));
        assert_eq!(view["proposals"], json!([data["proposals"][0]]));
        assert_eq!(view["operations"].as_array().unwrap().len(), 2);
        assert_eq!(view["operations"][0]["status"], "unknown");
        assert_eq!(
            view["operations"][0]["evidence"],
            data["operations"][0]["evidence"]
        );
        assert!(view["operations"][0].get("dispatchAuthority").is_none());
        assert_eq!(view["operations"][1], data["operations"][1]);
        assert_eq!(
            view["coverage"],
            json!({"itemsReturned":1,"operationsReturned":2,
            "operationsComplete":true,"historyTruncated":false})
        );
        assert!(view.to_string().len() * 100 < data.to_string().len());
        db.close().await;
    }

    #[tokio::test]
    async fn missing_duplicate_excess_and_cross_account_selection_fail_closed() {
        let (db, _folder, _data) = fixture().await;
        for ids in [
            vec!["missing".into()],
            vec!["selected".into(), "selected".into()],
            vec!["selected".into(), "missing".into()],
            vec!["x".into(); 101],
            vec!["selected' OR 1=1 --".into()],
        ] {
            assert!(db.read_bounded_review(&ids, "LikeAvto").await.is_err());
        }
        assert!(
            db.read_bounded_review(&["selected".into()], "BAW Russia")
                .await
                .is_err()
        );
        db.close().await;
    }
    #[tokio::test]
    async fn selected_review_includes_unknown_operation_on_same_scoped_provider_recipient_alias() {
        let (db,_folder,mut data)=fixture().await;
        let binding=crate::accounts::Profile::LikeAvto.binding();
        for item in data["items"].as_array_mut().unwrap() {
            item["objectId"]=json!("11391");item["itemId"]=json!("provider-comment");
            item["connectorBinding"]=binding.clone();
        }
        data["operations"][2]["status"]=json!("unknown");
        data["operations"][2]["itemId"]=json!("removed-local-alias");
        data["operations"][2]["target"]=json!({"objectId":"11391","itemId":"provider-comment","connectorBinding":binding});
        let mut foreign=data["items"][1].clone();foreign["id"]=json!("foreign");
        foreign["connectorBinding"]["accountId"]=json!("Other");
        data["items"].as_array_mut().unwrap().push(foreign);
        data["operations"].as_array_mut().unwrap().push(json!({"id":"foreign-op","itemId":"foreign","status":"unknown",
            "target":{"objectId":"11391","itemId":"provider-comment","connectorBinding":{"accountId":"Other"}}}));
        if let Database::Sqlite(pool)=&db {
            sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
        }
        let view=db.read_bounded_review(&["selected".into()],"LikeAvto").await.unwrap();
        assert_eq!(view["items"].as_array().unwrap().len(),1);
        assert_eq!(view["operations"].as_array().unwrap().len(),3);
        assert!(view["operations"].as_array().unwrap().iter().any(|op|op["id"]=="other-op"&&op["status"]=="unknown"));
        assert!(!view.to_string().contains("foreign-op"));
        assert_eq!(view["coverage"]["operationsComplete"],true);
        data["operations"].as_array_mut().unwrap().push(json!({"id":"ambiguous-op","itemId":"missing-local-row","status":"unknown",
            "target":{"objectId":"11391","itemId":"provider-comment","connectorBinding":{}}}));
        if let Database::Sqlite(pool)=&db {
            sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
        }
        let ambiguous=db.read_bounded_review(&["selected".into()],"LikeAvto").await.unwrap();
        assert_eq!(ambiguous["coverage"]["operationsComplete"],false);
        assert_eq!(ambiguous["coverage"]["ambiguousOperationCount"],1);
        db.close().await;
    }
    #[tokio::test]
    async fn large_branch_backlog_preserves_exact_order_unknown_aliases_and_ambiguity() {
        let(db,_folder,mut data)=fixture().await;
        let binding=crate::accounts::Profile::LikeAvto.binding();
        data["items"][0]["objectId"]=json!("selected-object");
        data["items"][0]["itemId"]=json!("selected-provider-comment");
        data["items"][0]["connectorBinding"]=binding.clone();
        for n in 0..1200 {
            data["items"].as_array_mut().unwrap().push(json!({"id":format!("backlog-{n}"),
                "postId":"other-post","branchId":format!("backlog-branch-{n}"),
                "objectId":"other-object","itemId":format!("other-comment-{n}"),"connectorBinding":binding}));
            data["branches"].as_array_mut().unwrap().push(json!({"id":format!("backlog-branch-{n}"),
                "postId":"other-post","messages":[{"text":"Private unselected branch"}],"observedMessages":[{"private":true}]}));
        }
        let grant=json!({"id":"conductor-grant","kind":"conductor","conductor":{"grant":{"scope":{"itemIds":["selected"]}}}});
        data["jobs"].as_array_mut().unwrap().push(grant.clone());
        data["operations"].as_array_mut().unwrap().extend([
            json!({"id":"alias-unknown","itemId":"removed-local-alias","status":"unknown",
                "target":{"objectId":"selected-object","itemId":"selected-provider-comment","connectorBinding":binding},
                "evidence":{"receipt":"original-uncertain-receipt"},"dispatchAuthority":{"private":true}}),
            json!({"id":"foreign-unknown","itemId":"foreign-local-alias","status":"unknown",
                "target":{"objectId":"selected-object","itemId":"selected-provider-comment","connectorBinding":{"accountId":"Other"}}}),
            json!({"id":"ambiguous-unknown","itemId":"missing-local-row","status":"unknown",
                "target":{"objectId":"selected-object","itemId":"selected-provider-comment","connectorBinding":{}}})
        ]);
        if let Database::Sqlite(pool)=&db {
            sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
        }
        let ids=vec!["backlog-1".to_owned(),"selected".to_owned(),"backlog-0".to_owned()];
        let view=db.read_bounded_review(&ids,"LikeAvto").await.unwrap();
        assert_eq!(view["items"].as_array().unwrap().iter().map(|i|i["id"].as_str().unwrap()).collect::<Vec<_>>(),
            vec!["selected","backlog-0","backlog-1"],"workspace order survives a reordered selector");
        assert_eq!(view["branches"].as_array().unwrap().len(),3);
        assert!(view["branches"].as_array().unwrap().iter().all(|b|b.get("observedMessages").is_none()));
        assert!(!view.to_string().contains("backlog-branch-1199"));
        let operations=view["operations"].as_array().unwrap();assert_eq!(operations.len(),3);
        let alias=operations.iter().find(|op|op["id"]=="alias-unknown").unwrap();
        assert_eq!(alias["status"],"unknown");assert_eq!(alias["evidence"]["receipt"],"original-uncertain-receipt");
        assert!(alias.get("dispatchAuthority").is_none());
        assert!(!operations.iter().any(|op|op["id"]=="foreign-unknown"||op["id"]=="ambiguous-unknown"));
        assert_eq!(view["coverage"]["operationsComplete"],false);assert_eq!(view["coverage"]["ambiguousOperationCount"],1);
        assert!(db.read_bounded_review(&ids,"BAW Russia").await.is_err());
        assert!(db.read_bounded_review(&["selected' OR 1=1 --".into()],"LikeAvto").await.is_err());
        let before_duplicate=if let Database::Sqlite(pool)=&db {
            sqlx::query_scalar::<_,String>("SELECT payload FROM workspace WHERE id=1").fetch_one(pool).await.unwrap()
        }else{unreachable!()};
        assert_eq!(serde_json::from_str::<Value>(&before_duplicate).unwrap(),data,"read-only projection leaves the current grant and full workspace intact");
        assert_eq!(data["jobs"].as_array().unwrap().last().unwrap(),&grant);
        let duplicate=data["items"][0].clone();data["items"].as_array_mut().unwrap().push(duplicate);
        if let Database::Sqlite(pool)=&db {
            sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
        }
        assert!(db.read_bounded_review(&["selected".into()],"LikeAvto").await.is_err(),"duplicate selected rows remain fail-closed");
        db.close().await;
    }

}
