//! Bounded PostgreSQL source deltas. Only the locked source-snapshot caller may
//! invoke this after validate_change; the caller owns commit/rollback and metadata.
//! No readonly/job/approval/operation payload enters this writer.
use super::*;

const MAX_ROWS: usize = 128;
const MAX_BYTES: usize = 1024 * 1024;

// A closed enum keeps both identifiers and projection keys out of stored data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Table { Posts, Branches, Items, Proposals, Audit }
const WRITE_TABLES: [Table; 5] = [Table::Posts, Table::Branches, Table::Items, Table::Proposals, Table::Audit];
impl Table {
    fn name(self) -> &'static str {
        match self { Self::Posts=>"posts", Self::Branches=>"branches", Self::Items=>"items", Self::Proposals=>"proposals", Self::Audit=>"audit" }
    }
    fn columns(self) -> &'static [(&'static str, &'static str)] { projection(self.name()) }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode { Update, Insert }

// Borrow only the changed rows; do not clone a workspace, a table or payloads.
struct DeltaRows<'a> {
    old: &'a [Value], new: &'a [Value], index: usize, start: i64, mode: Mode,
}
impl<'a> DeltaRows<'a> {
    fn new(old: &'a [Value], new: &'a [Value], start: i64, mode: Mode) -> Self {
        Self { old, new, index: if mode==Mode::Insert { old.len() } else { 0 }, start, mode }
    }
}
impl<'a> Iterator for DeltaRows<'a> {
    type Item = (i64, &'a Value);
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(value)=self.new.get(self.index) {
            let index=self.index; self.index+=1;
            if self.mode==Mode::Update {
                let old=self.old.get(index)?;
                if old==value { continue; }
            }
            // Validated collection lengths are bounded by i32::MAX. Addition
            // still needs checking when audit starts after retained history.
            return Some((self.start+index as i64,value));
        }
        None
    }
}
struct WireRow<'a> { id: &'a str, ordinal: i32, payload: String, bytes: usize }
impl<'a> WireRow<'a> {
    fn new((ordinal,value):(i64,&'a Value)) -> ApiResult<Self> {
        let ordinal=i32::try_from(ordinal).ok().filter(|v|*v>=0)
            .ok_or_else(||internal("Source admission ordinal overflow"))?;
        let id=text(value,"id")?;
        let payload=serde_json::to_string(value).map_err(|_|internal("Source admission serialization failed"))?;
        // Include the text-array identity and ordinal overhead. This is a bound
        // on serialized input bytes, not PostgreSQL memory or network framing.
        let bytes=payload.len().saturating_add(id.len()).saturating_add(24);
        Ok(Self { id, ordinal, payload, bytes })
    }
}
#[derive(Default)]
struct Batch<'a> { ids: Vec<&'a str>, ordinals: Vec<i32>, payloads: Vec<String>, bytes: usize }
impl<'a> Batch<'a> {
    fn push(&mut self,row:WireRow<'a>) {
        self.bytes=self.bytes.saturating_add(row.bytes);
        self.ids.push(row.id); self.ordinals.push(row.ordinal); self.payloads.push(row.payload);
    }
    fn len(&self) -> usize { self.ids.len() }
}
struct Batches<'a,I:Iterator<Item=(i64,&'a Value)>> { rows:I, pending:Option<WireRow<'a>>, failed:bool }
impl<'a,I:Iterator<Item=(i64,&'a Value)>> Batches<'a,I> {
    fn new(rows:I)->Self { Self { rows, pending:None, failed:false } }
}
impl<'a,I:Iterator<Item=(i64,&'a Value)>> Iterator for Batches<'a,I> {
    type Item=ApiResult<Batch<'a>>;
    fn next(&mut self)->Option<Self::Item> {
        if self.failed { return None; }
        let mut batch=Batch::default();
        while batch.len()<MAX_ROWS {
            let row=if let Some(row)=self.pending.take() { row } else {
                let Some(value)=self.rows.next() else { break; };
                match WireRow::new(value) { Ok(row)=>row, Err(error)=>{self.failed=true;return Some(Err(error));} }
            };
            if batch.len()>0 && batch.bytes.saturating_add(row.bytes)>MAX_BYTES {
                self.pending=Some(row); break;
            }
            batch.push(row);
            // An individual oversized source is legal, but travels alone.
            if batch.bytes>=MAX_BYTES { break; }
        }
        (batch.len()>0).then_some(Ok(batch))
    }
}

fn string_projection(key:&str)->String {
    // ->> alone would stringify booleans/numbers/objects. Match Value::as_str.
    format!("CASE WHEN jsonb_typeof(d.payload->'{key}')='string' THEN d.payload->>'{key}' ELSE NULL END")
}
fn statement(table:Table,mode:Mode)->ApiResult<String> {
    if table==Table::Audit && mode==Mode::Update {
        return Err(internal("Source admission cannot update audit history"));
    }
    let input="WITH d AS (SELECT id,ordinal,payload::jsonb AS payload FROM unnest($2::text[],$3::integer[],$4::text[]) AS input(id,ordinal,payload))";
    let columns=table.columns();
    Ok(match mode {
        Mode::Update=>format!("{input} UPDATE communityhero.{} AS target SET payload=d.payload{} FROM d WHERE target.workspace_id=$1 AND target.id=d.id AND target.ordinal=d.ordinal",
            table.name(),columns.iter().map(|(col,key)|format!(",{col}={}",string_projection(key))).collect::<String>()),
        Mode::Insert=>format!("{input} INSERT INTO communityhero.{}(workspace_id,id,ordinal,payload{}) SELECT $1,d.id,d.ordinal,d.payload{} FROM d ORDER BY d.ordinal",
            table.name(),columns.iter().map(|(col,_)|format!(",{col}")).collect::<String>(),
            columns.iter().map(|(_,key)|format!(",{}",string_projection(key))).collect::<String>()),
    })
}
async fn execute(connection:&mut PgConnection,sql:&str,batch:Batch<'_>)->ApiResult<()> {
    let expected=batch.len() as u64;
    let _write=crate::performance::Span::new("source.snapshot.persist.batch.execute");
    let affected=sqlx::query(sqlx::AssertSqlSafe(sql)).bind(WORKSPACE)
        .bind(batch.ids).bind(batch.ordinals).bind(batch.payloads)
        .execute(&mut *connection).await?.rows_affected();
    if affected!=expected { return Err(internal("Source admission batch row count mismatch")); }
    Ok(())
}

/// Tables only. Workspace FOR UPDATE and validate_change are caller preconditions.
/// Emits one DML statement per nonempty bounded batch and at most one audit-start
/// SELECT. For C changed rows the worst-case bound is C+1 statements (oversized
/// rows); with byte limit inactive, DML count is sum_t ceil(U_t/128)+ceil(I_t/128).
/// Metadata update/transaction/load statements belong to the caller, not here.
pub(super) async fn persist(connection:&mut PgConnection,before:&Value,after:&Value)->ApiResult<()> {
    for table in WRITE_TABLES {
        let old=rows(before,table.name())?; let new=rows(after,table.name())?;
        // Audit is an append-only projection: before is intentionally empty.
        // Refuse accidental reuse with a full-history view rather than rewriting.
        if table==Table::Audit && !old.is_empty() { return Err(internal("Source admission audit projection is not empty")); }
        if new.len()<old.len() { return Err(internal("Source admission cannot delete rows")); }
        let start=if table==Table::Audit && !new.is_empty() {
            sqlx::query_scalar::<_,i64>("SELECT COALESCE(MAX(ordinal)::bigint,-1)+1 FROM communityhero.audit WHERE workspace_id=$1")
                .bind(WORKSPACE).fetch_one(&mut *connection).await?
        } else { 0 };
        for mode in [Mode::Update,Mode::Insert] {
            if table==Table::Audit && mode==Mode::Update { continue; }
            let sql=statement(table,mode)?;
            let mut batches=Batches::new(DeltaRows::new(old,new,start,mode));
            loop {
                let serialize=crate::performance::Span::new("source.snapshot.persist.batch.serialize");
                let next=batches.next(); drop(serialize);
                let Some(batch)=next else { break; };
                execute(connection,&sql,batch?).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path="storage_source_delta_tests.rs"]
mod tests;
