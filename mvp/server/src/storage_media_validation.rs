//! Read-only media checks on the SAME leased writer and workspace row lock.
//! Omitted collections confer no absence or mutation authority. Reuse retains
//! the complete typed ledger, and legacy reuse the complete ordered catalogue.
use super::*;
use serde_json::json;

#[derive(Clone, Copy)]
pub(crate) enum MediaValidationMode { Execution, Reuse { legacy_catalog: bool } }
impl MediaValidationMode {
    fn ledger(self)->bool { matches!(self,Self::Reuse{..}) }
    fn catalog(self)->bool { matches!(self,Self::Reuse{legacy_catalog:true}) }
}
#[derive(Clone, Copy)]
pub(crate) struct MediaValidationScope<'a> {
    pub job: &'a str,
    pub post: &'a str,
    pub mode: MediaValidationMode,
}
impl Database {
    /// This is deliberately NOT a reader-pool snapshot: drain/source changes
    /// are serialized before the immutable callback by the workspace row lock.
    pub(crate) async fn check_media_validation<T>(&self,scope:MediaValidationScope<'_>,
        f:impl FnOnce(&Value)->ApiResult<T>)->ApiResult<T> {
        if scope.job.trim().is_empty() || scope.post.trim().is_empty() {
            return Err(internal("Media validation target missing"));
        }
        match self {
            Self::Sqlite(pool)=>{
                let mut tx=pool.begin().await?;
                let payload:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1")
                    .fetch_one(&mut *tx).await?;
                let mut workspace=parse(&payload)?;normalize(&mut workspace);
                // SQLite still parses its monolithic row, but neither clones
                // it for a mutable callback nor writes it back for a pure check.
                let view=scope_projection(&workspace,scope)?;
                let result=f(&view)?;tx.commit().await?;Ok(result)
            },
            Self::Postgres{writer,..}=>{
                let mut tx=writer.begin().await?;
                let record=sqlx::query("SELECT account,execution_enabled,(jsonb_build_object('account',metadata->'account') || CASE WHEN metadata ? 'connectorBinding' THEN jsonb_build_object('connectorBinding',metadata->'connectorBinding') ELSE '{}'::jsonb END || CASE WHEN metadata ? 'runtimeLifecycle' THEN jsonb_build_object('runtimeLifecycle',metadata->'runtimeLifecycle') ELSE '{}'::jsonb END)::text AS metadata FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                if record.try_get::<bool,_>("execution_enabled")? {return Err(internal("PostgreSQL pilot execution must remain disabled"));}
                let mut view=parse(record.try_get::<&str,_>("metadata")?)?;
                if !view.is_object() || record.try_get::<Option<String>,_>("account")?.as_deref()!=view["account"].as_str() {
                    return Err(internal("Workspace identity mismatch"));
                }
                view["posts"]=json!(selected_rows(&mut tx,"posts",scope).await?);
                view["jobs"]=json!(selected_rows(&mut tx,"jobs",scope).await?);
                if scope.mode.catalog() {
                    for table in ["knowledge_entries","knowledge_versions"] {
                        view[table]=json!(selected_rows(&mut tx,table,scope).await?);
                    }
                }
                targets(&view,scope)?;
                let result=f(&view)?;tx.commit().await?;Ok(result)
            },
        }
    }
}

fn selected(job:&Value,scope:MediaValidationScope<'_>)->bool {
    job["id"]==scope.job || (scope.mode.ledger() && job["kind"]=="media_analysis")
}
fn targets(view:&Value,scope:MediaValidationScope<'_>)->ApiResult<()> {
    let tables=if scope.mode.catalog() {&["jobs","posts","knowledge_entries","knowledge_versions"][..]} else {&["jobs","posts"][..]};
    for table in tables {
        let mut seen=HashSet::new();
        for row in rows(view,table)? {
            if !row.is_object() || !seen.insert(text(row,"id")?) {return Err(internal("Media validation record identity mismatch"));}
            for (_,key) in projection(table) {
                if !row[*key].is_null() && !row[*key].is_string() {return Err(internal("Invalid media validation projected field"));}
            }
            if *table=="jobs" && row["kind"]=="media_analysis" && row["status"]!="ledger" {
                return Err(internal("Media analysis carrier must not run"));
            }
        }
    }
    for (table,id) in [("jobs",scope.job),("posts",scope.post)] {
        if rows(view,table)?.iter().filter(|row|row["id"]==id).count()!=1 {
            return Err(crate::conflict("Media validation target missing or duplicated"));
        }
    }
    Ok(())
}
fn scope_projection(workspace:&Value,scope:MediaValidationScope<'_>)->ApiResult<Value> {
    let mut view=json!({"account":workspace["account"]});
    for key in ["connectorBinding","runtimeLifecycle"] {
        if let Some(value)=workspace.get(key) {view[key]=value.clone();}
    }
    view["posts"]=json!(rows(workspace,"posts")?.iter().filter(|row|row["id"]==scope.post).collect::<Vec<_>>());
    view["jobs"]=json!(rows(workspace,"jobs")?.iter().filter(|row|selected(row,scope)).collect::<Vec<_>>());
    if scope.mode.catalog() {
        for table in ["knowledge_entries","knowledge_versions"] {view[table]=Value::Array(rows(workspace,table)?.clone());}
    }
    targets(&view,scope)?;Ok(view)
}
async fn selected_rows(tx:&mut PgConnection,table:&str,scope:MediaValidationScope<'_>)->ApiResult<Vec<Value>> {
    // Both sides of relational projections are selected; a damaged physical
    // kind/id must be rejected, never hide a malformed ledger carrier.
    let (predicate,target,include_ledger)=match table {
        "posts"=>(" AND (id=$2 OR payload->>'id'=$2)",Some(scope.post),false),
        "jobs"=>(" AND (id=$2 OR payload->>'id'=$2 OR ($3 AND (kind='media_analysis' OR payload->>'kind'='media_analysis')))",Some(scope.job),scope.mode.ledger()),
        "knowledge_entries"|"knowledge_versions"=>("",None,false),
        _=>return Err(internal("Invalid media validation collection")),
    };
    let columns=projection(table).iter().map(|(column,_)|format!(",{column}")).collect::<String>();
    let sql=format!("SELECT id,ordinal,payload::text{columns} FROM communityhero.{table} WHERE workspace_id=$1{predicate} ORDER BY ordinal");
    let mut query=sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(WORKSPACE);
    if let Some(target)=target {query=query.bind(target);}
    if table=="jobs" {query=query.bind(include_ledger);}
    let records=query.fetch_all(&mut *tx).await?;
    let mut values=Vec::with_capacity(records.len());let mut seen=HashSet::new();
    for (position,record) in records.into_iter().enumerate() {
        let value=parse(record.try_get::<&str,_>("payload")?)?;
        if !value.is_object() || record.try_get::<&str,_>("id")?!=text(&value,"id")? || !seen.insert(text(&value,"id")?.to_owned()) {
            return Err(internal("Media validation record identity mismatch"));
        }
        // Catalogue is complete and ordered; retain old whole-reader parity.
        if target.is_none() && record.try_get::<i32,_>("ordinal")?!=position as i32 {
            return Err(internal("Media validation catalogue order mismatch"));
        }
        for (column,key) in projection(table) {
            if record.try_get::<Option<String>,_>(*column)?.as_deref()!=value[*key].as_str() {
                return Err(internal("Media validation relational projection mismatch"));
            }
        }
        values.push(value);
    }
    Ok(values)
}

#[cfg(test)]
#[path="storage_media_validation_tests.rs"]
mod tests;
