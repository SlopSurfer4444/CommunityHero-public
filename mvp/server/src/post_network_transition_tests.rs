//! Offline candidate tests: not compiled or executed in the frozen v59 tree.
use super::*;
use std::future::Future;
use std::task::{Context, Poll};

pub(crate) async fn fixture() -> (App, tempfile::TempDir, Value, Value) {
    let (app,temp)=crate::tests::test_app().await;
    let mut data=app.db.read().await.unwrap();
    let p=create_proposal(&mut data,&json!({"itemId":"item-1","kind":"close","expectedRevision":1})).unwrap();
    let op=json!({"id":"post-network-operation","itemId":"item-1","proposalId":p["id"],"attemptId":"attempt",
        "status":"dispatching","target":p["routeTarget"],"dispatchAuthority":{"actor":"synthetic"},
        "action":{"actionId":"post-network-action","action":"close","itemId":"comment-1","contextEvidenceDigest":"digest"}});
    data["operations"]=json!([op]);
    let Database::Sqlite(pool)=&app.db else {unreachable!()};
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
    (app,temp,data,op)
}

#[tokio::test]
async fn post_network_pairs_use_bounded_settlement_burst_and_eventual_standard_progress() {
    let (app,_temp,_before,op)=fixture().await;
    // Each settlement belongs to its own immutable attempt. A different
    // receipt for the same attempt is a conflict, not another queued pair.
    let operations:Vec<Value>=(0..=writer_gate::SETTLEMENT_BURST).map(|index|{
        let mut next=op.clone();next["id"]=json!(format!("post-network-operation-{index}"));
        next["attemptId"]=json!(format!("attempt-{index}"));
        next["action"]["actionId"]=json!(format!("post-network-action-{index}"));next
    }).collect();
    app.change(|d|{list_mut(d,"operations").extend(operations.clone());Ok(())}).await.unwrap();
    let initial=app.gate.acquire(writer_gate::Class::Standard).await;
    let (sent,mut received)=tokio::sync::mpsc::unbounded_channel();
    let gate=app.gate.clone();let standard_sent=sent.clone();
    let mut standard=Box::pin(async move {
        let _permit=gate.acquire(writer_gate::Class::Standard).await;
        standard_sent.send("standard".to_owned()).unwrap();
    });
    let mut context=Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(standard.as_mut().poll(&mut context).is_pending());
    assert_eq!(app.gate.queued_count(writer_gate::Class::Standard), 1);
    let mut pairs=Vec::new();
    for index in 0..=writer_gate::SETTLEMENT_BURST {
        let worker=app.clone();let op=operations[index].clone();let sent=sent.clone();
        let mut pair=Box::pin(async move {
            worker.change_execute_transition(&op,json!({"syntheticReceipt":index}),"unknown",|d|{
                assert_eq!(row(d,"operations",op["id"].as_str().unwrap())?["executeReceipt"]["syntheticReceipt"],index);
                sent.send(format!("pair{index}")).unwrap();
                apply_operation_outcome(d,&op,"unknown",json!({"providerRetryAllowed":false}))
            }).await.unwrap();
        });
        // The native writer first awaits lifecycle metadata. Pending alone does
        // not prove Settlement registration. Poll with the real task waker until
        // this exact queue owns the next FIFO registration; never sleep or spin.
        tokio::time::timeout(std::time::Duration::from_secs(5),
            std::future::poll_fn(|cx| {
                assert!(pair.as_mut().poll(cx).is_pending(),
                    "Initial holder must block every native pair before mutation");
                if app.gate.queued_count(writer_gate::Class::Settlement) == index + 1 {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
        ).await.expect("Native pair must register in Settlement before initial release");
        pairs.push(pair);
    }
    assert_eq!(app.gate.queued_count(writer_gate::Class::Settlement), writer_gate::SETTLEMENT_BURST + 1);
    drop(initial);
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        tokio::join!(futures_util::future::join_all(pairs),standard);
    }).await.expect("Standard and all paired writers must finish");
    let order:Vec<_>=std::iter::from_fn(||received.try_recv().ok()).collect();
    let mut expected:Vec<_>=(0..writer_gate::SETTLEMENT_BURST).map(|index|format!("pair{index}")).collect();
    expected.push("standard".to_owned());
    expected.push(format!("pair{}",writer_gate::SETTLEMENT_BURST));
    assert_eq!(order,expected);
    app.db.close().await;
}

async fn file_boundary(path:&std::path::Path) {
    tokio::time::timeout(std::time::Duration::from_secs(10),async {
        while !path.exists() {tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
    }).await.expect("fake-provider boundary was not reached");
}

#[tokio::test]
async fn post_network_real_dispatcher_preserves_network_boundaries_and_counts() {
    use dispatch_diagnostics::Outcome;
    for kind in ["close","reply_and_close"] {
      for mode in ["success","known_failure","ambiguous"] {
        let (mut app,temp)=crate::tests::test_app().await;
        app.node=PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe");
        app.bridge=temp.path().join("post-network-boundary-fixture.mjs");
        let op=app.change(|data|{
            connection_gate::fixture_open(data)?;
            crate::tests::create_post_fixture(data,"item-1")?;
            let mut body=json!({"itemId":"item-1","kind":kind,"expectedRevision":1});
            if kind=="reply_and_close" {body["text"]=json!("Synthetic reviewed reply");}
            let p=create_proposal(data,&body)?;
            if kind=="reply_and_close" {
                editorial_review::fixture_accept(data,p["id"].as_str().unwrap()).map_err(bad)?;
            }
            let p=row(data,"proposals",p["id"].as_str().unwrap())?.clone();
            let target=proposal_current(data,&p)?;
            let actor=operator_auth::Actor::local_owner("test");let authority=dispatch_authority::approval_binding(&actor);
            let op=json!({"id":"boundary-operation","itemId":"item-1","proposalId":p["id"],"approvalId":"synthetic-approval","attemptId":"synthetic-attempt",
                "action":action_for(&p,&target,"boundary-operation")?,"target":target,"status":"dispatching","createdAt":now(),
                "approvedBy":actor.public_json(),"executedBy":actor.public_json(),"dispatchAuthority":{"approved":authority,"executed":authority}});
            list_mut(data,"operations").push(op.clone());Ok(op)
        }).await.unwrap();
        let log=temp.path().join("provider-calls.jsonl");
        let script=r#"import {appendFile,writeFile,access} from 'node:fs/promises';
import {join} from 'node:path';
let raw='';for await(const chunk of process.stdin)raw+=chunk;
const r=JSON.parse(raw),target=__TARGET__,mode=__MODE__,root=__ROOT__;
await appendFile(__LOG__,JSON.stringify({operation:r.operation})+'\n');
if(r.operation==='context') {target.officialReplyIds=['old-official'];process.stdout.write(JSON.stringify({ok:true,result:target}));}
else {
 await writeFile(join(root,r.operation+'.entered'),'ready');
 const deadline=Date.now()+10000;
 while(true){try{await access(join(root,r.operation+'.release'));break;}catch{if(Date.now()>deadline)throw Error('fixture boundary timeout');await new Promise(r=>setTimeout(r,5));}}
 const a=r.actions[0];
 if(mode==='ambiguous'){process.stdout.write(JSON.stringify({ok:false,error:{code:'TRANSPORT_ERROR',adapterOperation:r.operation}}));}
 else {const row={actionId:a.actionId,itemId:a.itemId,status:'verified'};
  if(r.operation==='execute'&&mode==='known_failure'){row.status='failed';row.mutationOutcome='not-attempted';}
  if(r.operation==='execute'&&a.action==='reply_and_close')row.readbackEvidence={baselineReplyIds:['old-official']};
  process.stdout.write(JSON.stringify({ok:true,result:{account:r.account,results:[row]}}));}
}"#
            .replace("__TARGET__",&op["target"].to_string()).replace("__MODE__",&json!(mode).to_string())
            .replace("__ROOT__",&json!(temp.path().to_string_lossy()).to_string()).replace("__LOG__",&json!(log.to_string_lossy()).to_string());
        std::fs::write(&app.bridge,script).unwrap();
        let observer=async {
            file_boundary(&temp.path().join("execute.entered")).await;
            let before=app.db.read().await.unwrap();let saved=&before["operations"][0];
            assert_eq!(saved["status"],"dispatching");assert!(saved.get("executeReceipt").is_none());
            assert_eq!(saved["attemptId"],op["attemptId"]);assert_eq!(saved["dispatchAuthority"],op["dispatchAuthority"]);
            if kind=="reply_and_close" {assert_eq!(saved["action"]["readbackEvidence"]["baselineReplyIds"],json!(["old-official"]));}
            let permit=tokio::time::timeout(std::time::Duration::from_secs(1),app.gate.acquire(writer_gate::Class::Standard)).await.unwrap();drop(permit);
            std::fs::write(temp.path().join("execute.release"),"continue").unwrap();
            if mode!="known_failure" {
                file_boundary(&temp.path().join("readback.entered")).await;
                let interim=app.db.read().await.unwrap();let saved=&interim["operations"][0];
                assert_eq!(saved["status"],"unknown");assert!(saved.get("executeReceipt").is_some());
                assert_eq!(saved["evidence"]["verificationPhase"],"verifying");assert_eq!(saved["evidence"]["providerRetryAllowed"],false);
                let permit=tokio::time::timeout(std::time::Duration::from_secs(1),app.gate.acquire(writer_gate::Class::Standard)).await.unwrap();drop(permit);
                std::fs::write(temp.path().join("readback.release"),"continue").unwrap();
            }
        };
        let (result,())=tokio::time::timeout(std::time::Duration::from_secs(25),async {
            tokio::join!(dispatch(app.clone(),op.clone()),observer)
        }).await.unwrap();
        let expected=match mode {"success"=>Outcome::Succeeded,"known_failure"=>Outcome::Failed,_=>Outcome::Unknown};
        assert_eq!(result.unwrap(),expected,"{kind}/{mode}");
        let calls:Vec<String>=std::fs::read_to_string(&log).unwrap().lines().map(|line|serde_json::from_str::<Value>(line).unwrap()["operation"].as_str().unwrap().to_owned()).collect();
        if mode=="known_failure" {assert_eq!(calls,["context","execute"]);}
        else {assert_eq!(calls,["context","execute","readback"]);}
        let after=app.db.read().await.unwrap();assert_eq!(after["operations"][0]["status"],expected.status());
        if mode=="ambiguous" {assert_eq!(after["operations"][0]["evidence"]["providerRetryAllowed"],false);assert_eq!(after["items"][0]["providerStatus"],"new");}
        app.db.close().await;
      }
    }
}

async fn reopen(temp:&tempfile::TempDir)->Database {
    Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap())
}

#[tokio::test]
async fn post_network_receipt_survives_outcome_callback_guard_and_sql_failure() {
    for mode in ["callback","guard","sql"] {
        let (app,temp,before,op)=fixture().await;
        let receipt=json!({"synthetic":"returned-provider-receipt","mode":mode});
        let mut events=app.events.subscribe();
        let version=app.bootstrap_cache.current_version();
        if mode=="sql" {
            let Database::Sqlite(pool)=&app.db else {unreachable!()};
            sqlx::query("CREATE TRIGGER synthetic_reject_outcome BEFORE UPDATE OF payload ON workspace WHEN json_extract(NEW.payload,'$.operations[0].status')='unknown' BEGIN SELECT RAISE(ABORT,'synthetic outcome SQL failure'); END")
                .execute(pool).await.unwrap();
        }
        let result:ApiResult<()>=app.change_execute_transition(&op,receipt.clone(),"unknown",|d|{
            assert_eq!(d["operations"][0]["executeReceipt"],receipt);
            assert_ne!(app.bootstrap_cache.current_version(),version);
            assert!(events.try_recv().is_ok(),"receipt event precedes outcome callback");
            apply_operation_outcome(d,&op,"unknown",json!({"providerRetryAllowed":false}))?;
            match mode {
                "callback"=>return Err(internal("synthetic callback rejection")),
                "guard"=>d["operations"][0]["action"]["itemId"]=json!("retargeted"),
                _=>{}
            }
            Ok(())
        }).await;
        assert!(result.is_err(),"{mode}");
        assert!(events.try_recv().is_err(),"failed outcome has no success event");
        app.db.close().await;
        let db=reopen(&temp).await;
        let mut expected=before;
        expected["operations"][0]["executeReceipt"]=receipt;
        assert_eq!(db.read().await.unwrap(),expected,"{mode}: only receipt may survive");
        db.close().await;
        let _released=app.gate.acquire(writer_gate::Class::Standard).await;
    }
}

#[tokio::test]
async fn post_network_initial_receipt_rejection_never_calls_outcome_or_publishes() {
    for mode in ["identity","sql"] {
    let (app,_temp,before,op)=fixture().await;
    let mut wrong=op;
    if mode=="identity" {wrong["attemptId"]=json!("foreign-attempt");}
    else {
        let Database::Sqlite(pool)=&app.db else {unreachable!()};
        sqlx::query("CREATE TRIGGER synthetic_reject_receipt BEFORE UPDATE OF payload ON workspace WHEN json_type(NEW.payload,'$.operations[0].executeReceipt') IS NOT NULL BEGIN SELECT RAISE(ABORT,'synthetic receipt SQL failure'); END")
            .execute(pool).await.unwrap();
    }
    let mut events=app.events.subscribe();let version=app.bootstrap_cache.current_version();
    let result:ApiResult<()>=app.change_execute_transition(&wrong,json!({"receipt":"foreign"}),"unknown",|_|{
        panic!("outcome must not run when receipt identity rejects")
    }).await;
    assert!(result.is_err());assert_eq!(app.db.read().await.unwrap(),before);
    assert_eq!(app.bootstrap_cache.current_version(),version);assert!(events.try_recv().is_err());
    let _released=app.gate.acquire(writer_gate::Class::Standard).await;
    app.db.close().await;
    }
}

#[tokio::test]
async fn post_network_pair_has_one_ticket_and_no_interleaving_standard_writer() {
    let (app,_temp,_before,op)=fixture().await;
    let gate=app.gate.clone();
    let mut standard=Box::pin(gate.acquire(writer_gate::Class::Standard));
    let (result,timings)=performance::capture(app.change_execute_transition(&op,json!({"receipt":"returned"}),"unknown",|d|{
        let mut context=Context::from_waker(futures_util::task::noop_waker_ref());
        assert!(matches!(standard.as_mut().poll(&mut context),Poll::Pending),"receipt commit must not release permit before outcome");
        apply_operation_outcome(d,&op,"unknown",json!({"providerRetryAllowed":false}))
    })).await;
    result.unwrap();
    let _next=standard.await;
    assert_eq!(timings.iter().filter(|e|e["stage"]=="operation.execute_transition.writer.wait").count(),1);
    assert_eq!(timings.iter().filter(|e|e["stage"]=="operation.evidence.total").count(),1);
    assert_eq!(timings.iter().filter(|e|e["stage"]=="operation.outcome.total").count(),1);
    assert_eq!(app.db.read().await.unwrap()["operations"][0]["status"],"unknown");
    app.db.close().await;
}

#[tokio::test]
async fn post_network_queued_cancellation_has_no_effect_and_releases_registration() {
    let (app,_temp,before,op)=fixture().await;
    let held=app.gate.acquire(writer_gate::Class::Standard).await;
    let mut pending=Box::pin(app.change_execute_transition(&op,json!({"receipt":"not-yet-saved"}),"unknown",|_|Ok(())));
    let mut context=Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(matches!(pending.as_mut().poll(&mut context),Poll::Pending));
    drop(pending);drop(held);
    let _next=app.gate.acquire(writer_gate::Class::Standard).await;
    assert_eq!(app.db.read().await.unwrap(),before);
    app.db.close().await;
}

#[tokio::test]
async fn post_network_worker_panic_after_receipt_commit_preserves_recovery_evidence() {
    let (app,temp,before,op)=fixture().await;
    let worker=app.clone();let receipt=json!({"receipt":"survives-worker-interruption"});let saved=receipt.clone();
    let result=tokio::spawn(async move {
        worker.change_execute_transition::<()>(&op,receipt,"unknown",|d|{
            assert_eq!(d["operations"][0]["executeReceipt"],saved);
            panic!("synthetic worker interruption after receipt commit")
        }).await
    }).await;
    assert!(result.unwrap_err().is_panic());
    let permit=app.gate.acquire(writer_gate::Class::Standard).await;drop(permit);
    app.db.close().await;
    let db=reopen(&temp).await;let mut expected=before;
    expected["operations"][0]["executeReceipt"]=json!({"receipt":"survives-worker-interruption"});
    assert_eq!(db.read().await.unwrap(),expected);db.close().await;
}

fn normalize(mut data:Value,baseline:&Value)->Value {
    data["operations"][0]["updatedAt"]=Value::Null;
    for record in data["audit"].as_array_mut().unwrap().iter_mut().skip(baseline["audit"].as_array().unwrap().len()) {
        record["id"]=Value::Null;record["createdAt"]=Value::Null;
    }
    for record in data["feedback"].as_array_mut().unwrap().iter_mut().skip(baseline["feedback"].as_array().unwrap().len()) {
        record["createdAt"]=Value::Null;
    }
    data
}

#[tokio::test]
async fn post_network_pair_matches_prior_failed_and_unknown_states_without_claiming_readback() {
    for (outcome,foreign) in [("not-attempted",false),("rejected",false),("uncertain",false),("verified",false),("rejected",true)] {
        let (app,_temp,before,op)=fixture().await;
        let account=operation_account(&op).unwrap();
        let receipt=json!({"account":if foreign{"foreign-account"}else{account},"results":[{
            "actionId":op["action"]["actionId"],"itemId":op["action"]["itemId"],
            "status":if outcome=="verified"{"verified"}else{"failed"},"mutationOutcome":outcome
        }]});
        let failed=dispatch_evidence::confirmed_failure(&receipt,&op["action"],account);
        assert_eq!(failed,!foreign&&matches!(outcome,"not-attempted"|"rejected"));
        let status=if failed{"failed"}else{"unknown"};
        let evidence=if failed {dispatch_diagnostics::known_failure_evidence(receipt.clone(),&op,account)}
            else {json!({"receipt":receipt,"verificationPhase":"verifying","providerRetryAllowed":false})};
        // Reference uses the previous two independently gated App calls.
        dispatch_evidence::record_execute(&app,&op,receipt.clone()).await.unwrap();
        set_outcome(&app,&op,status,evidence.clone()).await.unwrap();
        let expected=normalize(app.db.read().await.unwrap(),&before);
        let Database::Sqlite(pool)=&app.db else {unreachable!()};
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(before.to_string()).execute(pool).await.unwrap();
        app.change_execute_transition(&op,receipt,status,|d|apply_operation_outcome(d,&op,status,evidence)).await.unwrap();
        let actual=app.db.read().await.unwrap();
        assert_eq!(normalize(actual.clone(),&before),expected,"{outcome}/{foreign}");
        assert_eq!(actual["operations"][0]["status"],status);
        assert_ne!(actual["operations"][0]["status"],"succeeded","even a verified execute receipt still requires independent readback");
        assert_eq!(actual["operations"][0]["evidence"]["providerRetryAllowed"],false);
        app.db.close().await;
    }
}
