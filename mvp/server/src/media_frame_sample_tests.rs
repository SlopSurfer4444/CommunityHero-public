use super::*;

pub(super) fn envelope() -> Value {
    json!({"schemaVersion":1,"needId":"need-1","needSha256":"a".repeat(64),"companyId":"fixture-company",
        "member":{"postId":"fixture-post","connectorBinding":{"connector":"fixture-native","connectionId":"fixture-connection"}},
        "asset":{"attachmentIndex":0,"attachmentIdentity":"b".repeat(64),"sourceVersion":"c".repeat(64),
            "sourceArtifactRef":{"sha256":"d".repeat(64),"bytes":12},"sourceArtifactSha256":"d".repeat(64)},
        "sourceProofRef":{"sha256":"e".repeat(64),"bytes":256},"sourceDurationMs":10000,
        "requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":3000},
        "profile":{"id":RANGE_PROFILE,"version":1,"maxFrames":8,"windowMs":1000,"rangeStepMs":1000,"overviewFrames":6,
            "maxImageBytes":4*1024*1024,"maxArtifactBytes":32*1024*1024,"maxTotalPixels":8000000,
            "maxDecodedDurationMs":30000,"maxDecodedFrames":900,"maxPrerollMs":5000,"deadlineMs":30000},
        "baseUsage":{"imageCount":2,"imageBytes":1000,"pixels":20000},
        "transportLimits":{"maxImages":16,"maxBytes":32*1024*1024,"maxPixels":32000000}})
}
#[test]
fn canonical_range_is_finite_and_preserves_request_identity() {
    let input = envelope();
    let plan = plan_sample(&input).unwrap();
    assert_eq!(
        plan["requestedTimeOrIntent"],
        input["requestedTimeOrIntent"]
    );
    assert_eq!(
        plan["targets"],
        json!([{ "targetIndex":0,"requestedTimestampMs":1000,"windowEndMs":2000 },
        {"targetIndex":1,"requestedTimestampMs":2000,"windowEndMs":3000}])
    );
    validate_plan(&plan).unwrap();
    for key in ["companyId", "member", "asset", "targets", "profile"] {
        let mut changed = plan.clone();
        changed[key] = Value::Null;
        assert!(validate_plan(&changed).is_err(), "{key}");
    }
}
#[test]
fn requested_point_uses_a_bounded_window_and_never_requests_eof() {
    let mut input = envelope();
    input["requestedTimeOrIntent"]["endMs"] = json!(1000);
    let plan = plan_sample(&input).unwrap();
    assert_eq!(
        plan["targets"],
        json!([{ "targetIndex":0,"requestedTimestampMs":1000,"windowEndMs":2000 }])
    );
    input["requestedTimeOrIntent"]["startMs"] = json!(10000);
    input["requestedTimeOrIntent"]["endMs"] = json!(10000);
    assert!(plan_sample(&input).is_err());
}
#[test]
fn overview_is_uniform_nonexhaustive_and_count_includes_mandatory_photos() {
    let mut input = envelope();
    input["sourceDurationMs"] = json!(12000);
    input["requestedTimeOrIntent"] =
        json!({"kind":"uniform_overview","timelineBasis":"relative_video_start"});
    input["profile"]["id"] = json!(OVERVIEW_PROFILE);
    let plan = plan_sample(&input).unwrap();
    let targets: Vec<_> = plan["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["requestedTimestampMs"].as_u64().unwrap())
        .collect();
    assert_eq!(targets, vec![1000, 3000, 5000, 7000, 9000, 11000]);
    assert_eq!(plan["exhaustive"], false);
    input["baseUsage"]["imageCount"] = json!(11);
    assert_eq!(
        plan_sample(&input).unwrap_err(),
        "frame_sample_transport_budget_exhausted"
    );
}
#[test]
fn unknown_timeline_zero_unbounded_profiles_and_nonfinite_numbers_are_rejected() {
    let baseline = envelope();
    for (key, value) in [
        ("maxFrames", json!(0)),
        ("deadlineMs", json!(120001)),
        ("maxDecodedFrames", json!(10001)),
        ("maxPrerollMs", json!(30001)),
        ("maxImageBytes", Value::Null),
    ] {
        let mut input = baseline.clone();
        input["profile"][key] = value;
        assert!(plan_sample(&input).is_err(), "{key}");
    }
    let mut input = baseline.clone();
    input["requestedTimeOrIntent"]["timelineBasis"] = json!("absolute_source_time");
    assert!(plan_sample(&input).is_err());
    let mut input = baseline;
    input["requestedTimeOrIntent"]["kind"] = json!("visual_intent");
    assert!(plan_sample(&input).is_err());
}
#[test]
fn source_pts_are_signed_exact_rational_and_distinct_from_elapsed_offsets() {
    let time = source_time(-45000, 1, 90000, -90000).unwrap();
    assert_eq!(time["pts"], "-45000");
    assert_eq!(time["sourceTimestampMs"], -500);
    assert_eq!(time["timelineOffsetMs"], 500);
    assert_eq!(
        time["sourceTimestamp"],
        json!({"numerator":"-45000","denominator":"90000","unit":"seconds"})
    );
    let fractional = source_time(1, 1, 3, 0).unwrap();
    assert_eq!(fractional["sourceTimestamp"]["numerator"], "1");
    assert_eq!(fractional["timestampMs"], 333);
    assert!(time.get("wordTimestamps").is_none());
    assert!(source_time(-90001, 1, 90000, -90000).is_err());
    assert!(source_time(i64::MAX, u64::MAX, 1, 0).is_err());
    assert!(source_time(0, 1, 0, 0).is_err());
}
