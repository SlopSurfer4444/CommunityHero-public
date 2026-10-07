//! Exact physical-row source deltas. The caller owns workspace lock/lease,
//! business validation, commit and rollback. No job/paid/operation DML exists.
use super::*;

const TABLES_TO_WRITE:&[&str]=&["posts","branches","items","proposals","audit"];
const MAX_ROWS:usize=128;
const MAX_BYTES:usize=1024*1024;

#[derive(Default)]
pub(super) struct PhysicalOrdinals {
    rows:HashMap<(&'static str,String),i32>,
}
impl PhysicalOrdinals {
    pub fn insert(&mut self,table:&'static str,id:String,ordinal:i32)->ApiResult<()> {
        if !TABLES_TO_WRITE.contains(&table)||ordinal<0||self.rows.contains_key(&(table,id.clone())){
            return Err(internal("Invalid or duplicate source physical ordinal"));
        }self.rows.insert((table,id),ordinal);Ok(())
    }
    pub fn get(&self,table:&'static str,id:&str)->ApiResult<i32> {
        self.rows.get(&(table,id.to_owned())).copied().ok_or_else(||internal("Source row was not loaded"))
    }
}

#[derive(Default)]
struct Batch {ids:Vec<String>,ordinals:Vec<i32>,payloads:Vec<String>,expected:Vec<String>,bytes:usize}
impl Batch {
    fn push(&mut self,id:&str,ordinal:i32,value:&Value,old:Option<&Value>)->ApiResult<()> {
        let payload=serde_json::to_string(value).map_err(|_|internal("Source delta serialization failed"))?;
        let expected=old.map(Value::to_string).unwrap_or_else(||"null".to_owned());
        self.bytes=self.bytes.saturating_add(id.len()+payload.len()+expected.len()+24);
        self.ids.push(id.to_owned());self.ordinals.push(ordinal);self.payloads.push(payload);self.expected.push(expected);
        Ok(())
    }
    fn full(&self)->bool {self.ids.len()>=MAX_ROWS||self.bytes>=MAX_BYTES}
}

fn string_projection(key:&str)->String {
    format!("CASE WHEN jsonb_typeof(d.payload->'{key}')='string' THEN d.payload->>'{key}' ELSE NULL END")
}
async fn flush(connection:&mut PgConnection,table:&'static str,insert:bool,batch:&mut Batch)->ApiResult<()> {
    if batch.ids.is_empty(){return Ok(());}
    if !TABLES_TO_WRITE.contains(&table)||(table=="audit"&&!insert){return Err(internal("Readonly source delta table"));}
    let expected=batch.ids.len() as u64;
    let bytes=batch.bytes;
    let columns=projection(table);
    let input="WITH d AS (SELECT id,ordinal,payload::jsonb AS payload,expected::jsonb AS expected FROM unnest($2::text[],$3::integer[],$4::text[],$5::text[]) AS input(id,ordinal,payload,expected))";
    let statement=if insert {
        format!("{input} INSERT INTO communityhero.{table}(workspace_id,id,ordinal,payload{}) SELECT $1,d.id,d.ordinal,d.payload{} FROM d ORDER BY d.ordinal",
            columns.iter().map(|(col,_)|format!(",{col}")).collect::<String>(),
            columns.iter().map(|(_,key)|format!(",{}",string_projection(key))).collect::<String>())
    }else{
        format!("{input} UPDATE communityhero.{table} AS target SET payload=d.payload{} FROM d WHERE target.workspace_id=$1 AND target.id=d.id AND target.ordinal=d.ordinal AND target.payload=d.expected",
            columns.iter().map(|(col,key)|format!(",{col}={}",string_projection(key))).collect::<String>())
    };
    let mut write=crate::performance::Span::new("source.snapshot.persist.batch.execute");
    let owned=std::mem::take(batch);
    let payload_bytes=owned.payloads.iter().map(|payload|payload.len() as u64).sum();
    let affected=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE)
        .bind(owned.ids).bind(owned.ordinals).bind(owned.payloads).bind(owned.expected)
        .execute(&mut *connection).await?.rows_affected();
    write.counts(expected as usize,bytes,1);
    write.measurements(crate::performance::StorageMeasurements{payload_write:crate::performance::ReadMeasurements{
        rows:Some(affected),bytes:Some(payload_bytes),statements:Some(1)},changed_rows:Some(affected),..Default::default()});
    if affected!=expected{return Err(internal("Source guarded delta lost expected row identity or payload"));}
    Ok(())
}

pub(super) async fn persist(connection:&mut PgConnection,before:&Value,after:&Value,ordinals:&PhysicalOrdinals)->ApiResult<()> {
    for &table in TABLES_TO_WRITE {
        let old=rows(before,table)?;let new=rows(after,table)?;
        if new.len()<old.len()||(table=="audit"&&!old.is_empty()){return Err(internal("Invalid source append delta"));}
        let mut updates=Batch::default();
        for (old,new) in old.iter().zip(new) {
            if old==new{continue;}
            if old["id"]!=new["id"]{return Err(internal("Source delta identity changed"));}
            let id=text(new,"id")?;
            // Bound batches before push; one oversized row is legal alone.
            let bytes=new.to_string().len()+old.to_string().len()+id.len()+24;
            if !updates.ids.is_empty()&&updates.bytes.saturating_add(bytes)>MAX_BYTES {flush(connection,table,false,&mut updates).await?;}
            updates.push(id,ordinals.get(table,id)?,new,Some(old))?;
            if updates.full(){flush(connection,table,false,&mut updates).await?;}
        }
        flush(connection,table,false,&mut updates).await?;
        if new.len()==old.len(){continue;}
        let next_statement=format!("SELECT COALESCE(MAX(ordinal)::bigint,-1)+1 FROM communityhero.{table} WHERE workspace_id=$1");
        let mut ordinal=sqlx::query_scalar::<_,i64>(sqlx::AssertSqlSafe(next_statement.as_str()))
            .bind(WORKSPACE).fetch_one(&mut *connection).await?;
        let mut inserts=Batch::default();
        for new in &new[old.len()..] {
            let id=text(new,"id")?;
            let position=i32::try_from(ordinal).ok().filter(|n|*n>=0).ok_or_else(||internal("Source ordinal overflow"))?;
            ordinal+=1;
            let bytes=new.to_string().len()+id.len()+28;
            if !inserts.ids.is_empty()&&inserts.bytes.saturating_add(bytes)>MAX_BYTES {flush(connection,table,true,&mut inserts).await?;}
            inserts.push(id,position,new,None)?;
            if inserts.full(){flush(connection,table,true,&mut inserts).await?;}
        }
        flush(connection,table,true,&mut inserts).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_ordinal_is_explicit_and_unloaded_identity_cannot_update() {
        let mut map=PhysicalOrdinals::default();map.insert("items","selected".into(),912).unwrap();
        assert_eq!(map.get("items","selected").unwrap(),912);
        assert!(map.get("items","unloaded").is_err());
        assert!(map.insert("items","selected".into(),0).is_err());
        assert_eq!(map.get("items","selected").unwrap(),912,"a rejected duplicate must preserve the captured physical ordinal");
        assert!(map.insert("jobs","paid".into(),1).is_err());
    }
}
