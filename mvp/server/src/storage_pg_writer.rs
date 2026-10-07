//! Explicit completion of the existing sole leased PG writer connection.
//! No pool topology, authority, transaction reducer or retry policy changes.
use super::*;
use sqlx::{Postgres, Transaction, pool::PoolConnection};

pub(super) struct Completion<T> {
    committed: bool,
    rollback_acknowledged: bool,
    // A failed COMMIT must not drop a possibly huge successful reducer reply
    // before the caller finishes returning this same borrowed connection.
    discarded_reply: Option<T>,
}

/// Consume the borrowed transaction while its large projections stay owned by
/// the caller. A rejected reducer still reports its original API error, as the
/// old drop-triggered rollback did. Failed COMMIT remains an ambiguous error;
/// SQLx's transaction Drop queues rollback and release below flushes it by ping.
pub(super) async fn settle<T>(tx:Transaction<'_,Postgres>,outcome:ApiResult<T>)
    ->(ApiResult<T>,Completion<T>) {
    match outcome {
        Ok(value)=>match tx.commit().await {
            Ok(())=>(Ok(value),Completion{committed:true,rollback_acknowledged:false,discarded_reply:None}),
            Err(error)=>(Err(error.into()),Completion{committed:false,rollback_acknowledged:false,discarded_reply:Some(value)}),
        },
        Err(error)=>{
            let rollback_acknowledged=tx.rollback().await.is_ok();
            (Err(error),Completion{committed:false,rollback_acknowledged,discarded_reply:None})
        }
    }
}

/// SQLx0.9 takes the live connection eagerly into this future, then pings and
/// releases the pool permit before completing. Await it in the caller rather
/// than depending on PoolConnection::Drop's spawned return task. Cancellation
/// can discard that live connection; existing pool maintenance and after_connect
/// lease fencing remain in force. Completion does not promise the same PID or
/// an idle slot under concurrency. No detach/leak/close shortcut is used.
pub(super) async fn release<T>(connection:&mut PoolConnection<Postgres>,writer:&PgPool,
    completion:Completion<T>) {
    let mut returning=crate::performance::Span::new("pg.writer.return");
    connection.return_to_pool().await;
    returning.writer_pool_state(completion.committed,completion.rollback_acknowledged,
        writer.num_idle(),writer.size());
    // All diagnostics happen AFTER the awaited return, before caller projections
    // and persistence timing guards drop. Never infer retry authority from this.
    drop(returning);
    drop(completion.discarded_reply);
}

#[cfg(test)]
#[path="storage_pg_writer_tests.rs"]
mod tests;
