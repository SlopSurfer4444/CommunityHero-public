use super::*;

// Measure the actual production futures without polling a provider or granting
// any work. A later nested transport addition must not regrow caller frames.
#[tokio::test]
async fn shared_bridge_future_stays_bounded_before_first_poll() {
    let (app, _temp) = Box::pin(crate::tests::test_app()).await;
    let before = app.lifecycle_work.snapshot().unwrap();
    for operation in ["context", "execute", "readback", "assistant", "media_vision"] {
        let future = app.bridge(operation, json!({}));
        let bytes = std::mem::size_of_val(&future);
        assert!(bytes <= 16 * 1024, "{operation} bridge caller future grew to {bytes} bytes");
        drop(future);
    }
    let direct = app.bridge_observed_inner("context", json!({}), None, None);
    assert_eq!(std::mem::size_of_val(&direct), std::mem::size_of::<usize>(), "shared bridge must be one heap pointer before polling");
    drop(direct);
    let after = app.lifecycle_work.snapshot().unwrap();
    assert_eq!(after.active, before.active);
    assert_eq!(after.unresolved, before.unresolved);
    assert_eq!(after.closed, before.closed);
}