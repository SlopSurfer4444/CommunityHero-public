use super::*;
use std::sync::Mutex;

/// Mimics Windows pipe shutdown semantics: only dropping the writer signals
/// EOF. The receiver represents a child that cannot exit before EOF arrives.
struct EofWriter {
    state: std::sync::Arc<Mutex<(Vec<u8>, bool)>>,
    eof: Option<tokio::sync::oneshot::Sender<()>>,
}
impl AsyncWrite for EofWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.state.lock().unwrap().0.extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.state.lock().unwrap().1 = true;
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
impl Drop for EofWriter {
    fn drop(&mut self) {
        if let Some(eof) = self.eof.take() {
            let _ = eof.send(());
        }
    }
}
#[tokio::test]
async fn pipe_input_flushes_and_closes_before_waiting_for_eof_dependent_child() {
    let state = std::sync::Arc::new(Mutex::new((Vec::new(), false)));
    let (eof, receiver) = tokio::sync::oneshot::channel();
    let writer = EofWriter {
        state: state.clone(),
        eof: Some(eof),
    };
    let input = b"png packet needing final EOF";
    tokio::time::timeout(Duration::from_secs(1), async {
        let (sent, child_exit) = tokio::join!(send_input(writer, input), receiver);
        sent.unwrap();
        child_exit.unwrap();
    })
    .await
    .expect("EOF must arrive while child wait is pending");
    assert_eq!(*state.lock().unwrap(), (input.to_vec(), true));
}

#[tokio::test]
#[ignore = "root queue: explicit offline FFmpeg PNG stdin EOF regression"]
async fn offline_real_png_stdin_eof_closes_before_waiting_child() {
    let registry = Registry::default();
    let ffmpeg = PathBuf::from(
        std::env::var_os("COMMUNITYHERO_TEST_FRAME_FFMPEG").expect("explicit fixture FFmpeg path"),
    );
    hash_file(&ffmpeg).unwrap();
    let process = LocalProcess {
        registry: &registry,
    };
    let rgb = vec![1u8, 2, 3, 4, 5, 6];
    let encode = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-video_size",
        "2x1",
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
        "-c:v",
        "png",
        "-f",
        "image2pipe",
        "pipe:1",
    ]
    .iter()
    .map(|s| (*s).into())
    .collect();
    let png = execute(
        &process,
        &ProcessSpec {
            kind: "fixture_eof_png_encode",
            executable: ffmpeg.clone(),
            args: encode,
            stdin: rgb.clone(),
            stdout_limit: 4096,
            stderr_limit: META_LIMIT,
            deadline: Duration::from_secs(5),
            guard: None,
        },
    )
    .await
    .unwrap()
    .stdout;
    assert_eq!(png_dimensions(&png).unwrap(), (2, 1));
    let decode = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "image2pipe",
        "-c:v",
        "png",
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "pipe:1",
    ]
    .iter()
    .map(|s| (*s).into())
    .collect();
    let raw = execute(
        &process,
        &ProcessSpec {
            kind: "fixture_eof_png_decode",
            executable: ffmpeg,
            args: decode,
            stdin: png,
            stdout_limit: 6,
            stderr_limit: META_LIMIT,
            deadline: Duration::from_secs(5),
            guard: None,
        },
    )
    .await
    .unwrap()
    .stdout;
    assert_eq!(raw, rgb);
    assert_eq!(registry.snapshot().unwrap().active, 0);
    assert_eq!(registry.snapshot().unwrap().unresolved, 0);
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: ArtifactStore,
    input: Value,
    tools: SampleTools,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open(&dir.path().join("cas")).unwrap();
    let source = store.put_bytes(b"\0\0\0\x0cftypisom").unwrap();
    let ffmpeg = dir.path().join("fake-ffmpeg");
    let ffprobe = dir.path().join("fake-ffprobe");
    std::fs::write(&ffmpeg, b"fixture-ffmpeg").unwrap();
    std::fs::write(&ffprobe, b"fixture-ffprobe").unwrap();
    let tools = SampleTools {
        ffmpeg_sha256: hash_file(&ffmpeg).unwrap(),
        ffprobe_sha256: hash_file(&ffprobe).unwrap(),
        ffmpeg,
        ffprobe,
        ffmpeg_version: "fixture-ffmpeg-version".into(),
        ffprobe_version: "fixture-ffprobe-version".into(),
        deadline: Duration::from_secs(30),
    };
    let mut input = json!({"schemaVersion":1,"needId":"fixture-need","needSha256":"a".repeat(64),"companyId":"fixture-company",
        "member":{"postId":"fixture-post","connectorBinding":{"connector":"fixture-native","connectionId":"fixture-connection"}},
        "asset":{"attachmentIndex":0,"attachmentIdentity":"b".repeat(64),"sourceVersion":"c".repeat(64),"sourceArtifactRef":source.to_json(),"sourceArtifactSha256":source.sha256},
        "sourceDurationMs":10000,"requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000},
        "profile":{"id":"bounded_time_range_v1","version":1,"maxFrames":8,"windowMs":1000,"rangeStepMs":1000,"overviewFrames":6,
            "maxImageBytes":1024,"maxArtifactBytes":1024*1024,"maxTotalPixels":1000,"maxDecodedDurationMs":30000,"maxDecodedFrames":900,"maxPrerollMs":5000,"deadlineMs":30000},
        "baseUsage":{"imageCount":1,"imageBytes":20,"pixels":10},"transportLimits":{"maxImages":16,"maxBytes":32*1024*1024,"maxPixels":32000000}});
    input["sourceProofRef"] = proof_for(&store, &input);
    Fixture {
        _dir: dir,
        store,
        input,
        tools,
    }
}
fn proof_for(store: &ArtifactStore, input: &Value) -> Value {
    let receipt =
        json!({"method":"local_sha256_and_size","source":input["asset"]["sourceArtifactRef"]});
    retain_source_proof(store,&json!({"schemaVersion":1,"kind":"retained_video_source","companyId":input["companyId"],"member":input["member"],"asset":input["asset"],
        "sourceDurationMs":input["sourceDurationMs"],"verifiedReceipt":receipt,"verifiedFile":{"sha256":input["asset"]["sourceArtifactSha256"],"bytes":input["asset"]["sourceArtifactRef"]["bytes"],"receiptSha256":digest(&receipt)},
        "speechOutcome":"no_audio"})).unwrap()
}
fn png(width: u32, height: u32) -> Vec<u8> {
    // Pure fake process evidence. Actual PNG codec verification is exercised
    // only by the separately admitted ignored offline fixture below.
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend(width.to_be_bytes());
    b.extend(height.to_be_bytes());
    b.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
    b
}
struct Fake {
    calls: Mutex<Vec<ProcessSpec>>,
    failure: Option<&'static str>,
    empty: bool,
}
impl Fake {
    fn good() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            failure: None,
            empty: false,
        }
    }
    fn count(&self, kind: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.kind == kind)
            .count()
    }
}
impl SampleProcess for Fake {
    fn execute<'a>(
        &'a self,
        spec: &'a ProcessSpec,
    ) -> Pin<Box<dyn Future<Output = Result<ProcessOutput, String>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(spec.clone());
            let rgb = vec![1, 2, 3, 4, 5, 6];
            let out=match spec.kind {
                "probe"=>ProcessOutput{stdout:json!({"streams":[{"index":2,"width":2,"height":1,"time_base":"1/1000","start_pts":-1000,"duration_ts":10000}],"format":{"duration":"10.0"}}).to_string().into_bytes(),stderr:vec![]},
                "sample"=>{
                    let guard=spec.guard.as_ref().unwrap();let actual=guard.metadata.start_pts+guard.target_ms as i64+40;
                    let previous=actual-80;let count=if self.failure==Some("too_many"){3}else{2};
                    let mut text=String::new();
                    for n in 0..count {text.push_str(&format!("[Parsed_showinfo_1 @ fixture] n: {n} pts: {} pts_time: 0\n",if n==0{previous}else{actual}));}
                    if !self.empty {
                        let tb=if self.failure==Some("timebase"){ "1/25" }else{"1/1000"};
                        let hash=if self.failure==Some("pixelhash"){"f".repeat(64)}else{format!("{:x}",Sha256::digest(&rgb))};
                        let pts=if self.failure==Some("pts"){actual+1}else{actual};
                        text.push_str(&format!("#tb 0: {tb}\n#dimensions 0: 2x1\n0, {pts}, {pts}, 1, 6, {hash}\n"));
                    }
                    ProcessOutput{stdout:if self.empty{vec![]}else{rgb},stderr:text.into_bytes()}
                },
                "encode_png"=>ProcessOutput{stdout:png(2,1),stderr:vec![]},
                "verify_png"=>ProcessOutput{stdout:if self.failure==Some("png_pixels"){vec![9;6]}else{rgb},stderr:vec![]},
                _=>return Err("unexpected_fixture_process".into()),
            };
            Ok(out)
        })
    }
}
#[tokio::test]
async fn bound_source_pts_pixels_and_full_png_are_retained_and_cold_reused() {
    let f = fixture();
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake::good();
    let result = decode_sample_with(&f.store, &plan, &f.tools, &fake)
        .await
        .unwrap();
    let frame = &result["frames"][0];
    assert_eq!(frame["requestedTimestampMs"], 1000);
    assert_eq!(frame["actualPts"], 40);
    assert_eq!(frame["sourceTimestampMs"], 40);
    assert_eq!(frame["timelineOffsetMs"], 1040);
    assert!(frame["frameIndex"].is_null());
    assert_eq!(frame["fullFrame"], true);
    assert_eq!(result["status"], "complete");
    assert_eq!(result["used"]["decodedFrames"], 2);
    let reopened = ArtifactStore::open(f.store.root()).unwrap();
    verify_sample_result(&reopened, &plan, &result, &f.tools).unwrap();
    assert_eq!(fake.count("probe"), 1);
    assert_eq!(fake.count("sample"), 1);
    let calls = fake.calls.lock().unwrap();
    let sample = calls.iter().find(|s| s.kind == "sample").unwrap();
    assert!(sample.args.contains(&"-ss".into()));
    assert!(sample.args.contains(&"-copyts".into()));
    assert!(sample.args.contains(&"-noaccurate_seek".into()));
    assert!(sample.args.contains(&"0:2".into()));
    assert!(!sample.args.iter().any(|a| a.contains("fps=")
        || a.contains("scale=")
        || a.contains("crop=")
        || a.contains("ocr")));
    assert_eq!(frame["timeBase"], json!({"num":1,"den":1000}));
    assert!(frame.get("wordTimestamps").is_none());
}
#[tokio::test]
async fn changed_company_member_asset_proof_and_tool_identity_fail_before_dispatch() {
    let f = fixture();
    for key in ["companyId", "member", "asset"] {
        let mut input = f.input.clone();
        match key {
            "companyId" => input[key] = json!("another-company"),
            "member" => input[key]["postId"] = json!("another-post"),
            _ => input[key]["sourceVersion"] = json!("f".repeat(64)),
        }
        let plan = crate::media_frame_sample::plan_sample(&input).unwrap();
        let fake = Fake::good();
        assert_eq!(
            decode_sample_with(&f.store, &plan, &f.tools, &fake)
                .await
                .unwrap_err(),
            "frame_sample_source_binding_changed"
        );
        assert!(fake.calls.lock().unwrap().is_empty());
    }
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake::good();
    std::fs::write(&f.tools.ffmpeg, b"changed executable").unwrap();
    assert!(
        decode_sample_with(&f.store, &plan, &f.tools, &fake)
            .await
            .is_err()
    );
    assert!(fake.calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn mismatched_demux_pts_timebase_or_pixels_never_create_a_result() {
    for failure in ["timebase", "pts", "pixelhash", "png_pixels"] {
        let f = fixture();
        let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
        let fake = Fake {
            failure: Some(failure),
            ..Fake::good()
        };
        assert!(
            decode_sample_with(&f.store, &plan, &f.tools, &fake)
                .await
                .is_err(),
            "{failure}"
        );
        if failure != "png_pixels" {
            assert_eq!(fake.count("encode_png"), 0);
        }
    }
}
#[tokio::test]
async fn observed_preroll_positions_consume_one_cumulative_count_budget() {
    let mut f = fixture();
    f.input["requestedTimeOrIntent"]["endMs"] = json!(3000);
    f.input["profile"]["maxDecodedFrames"] = json!(3);
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake::good();
    assert_eq!(
        decode_sample_with(&f.store, &plan, &f.tools, &fake)
            .await
            .unwrap_err(),
        "frame_sample_decode_count_exceeded"
    );
    assert_eq!(fake.count("sample"), 2);
    assert_eq!(fake.count("encode_png"), 1);
}
#[tokio::test]
async fn missing_bounded_frame_retains_partial_coverage_without_expanding_search() {
    let f = fixture();
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake {
        empty: true,
        ..Fake::good()
    };
    let result = decode_sample_with(&f.store, &plan, &f.tools, &fake)
        .await
        .unwrap();
    assert_eq!(result["status"], "partial");
    assert!(result["frames"].as_array().unwrap().is_empty());
    assert_eq!(fake.count("sample"), 1);
    assert_eq!(fake.count("encode_png"), 0);
    verify_sample_result(&f.store, &plan, &result, &f.tools).unwrap();
}
#[tokio::test]
async fn pixel_and_transport_bytes_bound_work_and_cas_corruption_blocks_reuse() {
    let mut f = fixture();
    f.input["profile"]["maxTotalPixels"] = json!(1);
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake::good();
    assert_eq!(
        decode_sample_with(&f.store, &plan, &f.tools, &fake)
            .await
            .unwrap_err(),
        "frame_sample_pixel_budget_exhausted"
    );
    assert_eq!(fake.count("sample"), 0);
    let mut f = fixture();
    f.input["transportLimits"]["maxBytes"] = json!(20 + png(2, 1).len() - 1);
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake::good();
    assert_eq!(
        decode_sample_with(&f.store, &plan, &f.tools, &fake)
            .await
            .unwrap_err(),
        "frame_sample_byte_budget_exhausted"
    );
    assert_eq!(fake.count("sample"), 1);
    let f = fixture();
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake::good();
    let result = decode_sample_with(&f.store, &plan, &f.tools, &fake)
        .await
        .unwrap();
    let reference = artifact(&result["frames"][0]["artifact"]).unwrap();
    std::fs::write(f.store.path(&reference).unwrap(), b"corrupt").unwrap();
    assert!(verify_sample_result(&f.store, &plan, &result, &f.tools).is_err());
}

struct Slow;
impl SampleProcess for Slow {
    fn execute<'a>(
        &'a self,
        _spec: &'a ProcessSpec,
    ) -> Pin<Box<dyn Future<Output = Result<ProcessOutput, String>> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}
#[tokio::test]
async fn common_deadline_bounds_an_unresponsive_injected_process() {
    let mut f = fixture();
    f.input["profile"]["deadlineMs"] = json!(10);
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    assert_eq!(
        decode_sample_with(&f.store, &plan, &f.tools, &Slow)
            .await
            .unwrap_err(),
        "frame_sample_deadline_exceeded"
    );
}

#[tokio::test]
async fn corrupted_retained_source_fails_before_probe_and_rehashed_time_lie_fails_cold_reuse() {
    let f = fixture();
    let plan = crate::media_frame_sample::plan_sample(&f.input).unwrap();
    let fake = Fake::good();
    let result = decode_sample_with(&f.store, &plan, &f.tools, &fake)
        .await
        .unwrap();
    let mut forged = result.clone();
    forged.as_object_mut().unwrap().remove("resultRef");
    forged.as_object_mut().unwrap().remove("resultSha256");
    forged["frames"][0]["actualPts"] = json!(41);
    let reference = save_json(&f.store, &forged).unwrap();
    forged["resultSha256"] = reference["sha256"].clone();
    forged["resultRef"] = reference;
    assert_eq!(
        verify_sample_result(&f.store, &plan, &forged, &f.tools).unwrap_err(),
        "frame_sample_result_time_changed"
    );
    let source = artifact(&f.input["asset"]["sourceArtifactRef"]).unwrap();
    std::fs::write(f.store.path(&source).unwrap(), b"changed source").unwrap();
    let fresh_fake = Fake::good();
    assert!(
        decode_sample_with(&f.store, &plan, &f.tools, &fresh_fake)
            .await
            .is_err()
    );
    assert!(fresh_fake.calls.lock().unwrap().is_empty());
}
#[test]
fn negative_nonzero_start_and_vfr_positions_have_exact_source_evidence() {
    let meta = Metadata {
        index: 0,
        width: 2,
        height: 1,
        num: 1,
        den: 90000,
        start_pts: -90000,
        duration_ms: 10000,
    };
    assert_eq!(absolute_seconds(&meta, 500).unwrap(), "-0.500000000");
    let guard = DecodeGuard {
        metadata: meta.clone(),
        target_ms: 500,
        end_ms: 1000,
        max_preroll_ms: 100,
        max_frames: 4,
        max_duration_ms: 1000,
    };
    let rgb = vec![1, 2, 3, 4, 5, 6];
    let hash = format!("{:x}", Sha256::digest(&rgb));
    let out=ProcessOutput{stdout:rgb,stderr:format!("[showinfo] n: 0 pts: -50400 pts_time: -0.56\n[showinfo] n: 1 pts: -44000 pts_time: -0.488\n#tb 0: 1/90000\n#dimensions 0: 2x1\n0, -44000, -44000, 1, 6, {hash}\n").into_bytes()};
    let (pts, observed) = frame_evidence(&out, &guard).unwrap();
    assert_eq!(pts, Some(-44000));
    assert_eq!(observed.count, 2);
    let time = source_time(-44000, 1, 90000, -90000).unwrap();
    assert_eq!(time["sourceTimestampMs"], -488);
    assert_eq!(time["timelineOffsetMs"], 511);
    let mut bad = guard.clone();
    bad.max_preroll_ms = 20;
    assert!(frame_evidence(&out, &bad).is_err());
}

// Root alone admits and runs this opt-in real FFmpeg fixture. The ordinary
// native test suite above never invokes any executable or needs media config.
#[tokio::test]
#[ignore = "root queue: explicit offline FFmpeg fixture paths only"]
async fn offline_real_no_audio_nonzero_start_vfr_and_uniform_overview() {
    let dir = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open(&dir.path().join("cas")).unwrap();
    let registry = Registry::default();
    let ffmpeg = PathBuf::from(
        std::env::var_os("COMMUNITYHERO_TEST_FRAME_FFMPEG").expect("explicit fixture FFmpeg path"),
    );
    let ffprobe = PathBuf::from(
        std::env::var_os("COMMUNITYHERO_TEST_FRAME_FFPROBE")
            .expect("explicit fixture FFprobe path"),
    );
    let mut tools = SampleTools {
        ffmpeg_sha256: hash_file(&ffmpeg).unwrap(),
        ffprobe_sha256: hash_file(&ffprobe).unwrap(),
        ffmpeg,
        ffprobe,
        ffmpeg_version: "offline-fixture-pinned-ffmpeg".into(),
        ffprobe_version: "offline-fixture-pinned-ffprobe".into(),
        deadline: Duration::from_secs(60),
    };
    let process = LocalProcess {
        registry: &registry,
    };
    for (executable, is_decoder) in [(tools.ffmpeg.clone(), true), (tools.ffprobe.clone(), false)] {
        let output = execute(
            &process,
            &ProcessSpec {
                kind: "fixture_version",
                executable,
                args: vec!["-version".into()],
                stdin: vec![],
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                deadline: Duration::from_secs(10),
                guard: None,
            },
        )
        .await
        .unwrap();
        let version = std::str::from_utf8(&output.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_owned();
        if is_decoder {
            tools.ffmpeg_version = version;
        } else {
            tools.ffprobe_version = version;
        }
    }
    let source_path = dir.path().join("no-audio-vfr.mp4");
    let args = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=64x48:rate=10:duration=6",
        "-vf",
        "select=not(eq(mod(n\\,7)\\,0)),setpts=PTS+2/TB",
        "-fps_mode",
        "vfr",
        "-c:v",
        "mpeg4",
        "-threads",
        "1",
        "-g",
        "5",
        "-an",
        "-avoid_negative_ts",
        "disabled",
        "-movflags",
        "+faststart",
        "-y",
    ]
    .iter()
    .map(|s| (*s).into())
    .chain([source_path.display().to_string()])
    .collect();
    execute(
        &process,
        &ProcessSpec {
            kind: "fixture_source",
            executable: tools.ffmpeg.clone(),
            args,
            stdin: vec![],
            stdout_limit: META_LIMIT,
            stderr_limit: META_LIMIT,
            deadline: Duration::from_secs(30),
            guard: None,
        },
    )
    .await
    .unwrap();
    let source = store.put_file(&source_path).unwrap();
    let probe_args = [
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=index,width,height,time_base,start_pts,duration_ts:format=duration,start_time",
        "-of",
        "json",
    ]
    .iter()
    .map(|s| (*s).into())
    .chain([source_path.display().to_string()])
    .collect();
    let probe_out = execute(
        &process,
        &ProcessSpec {
            kind: "fixture_probe",
            executable: tools.ffprobe.clone(),
            args: probe_args,
            stdin: vec![],
            stdout_limit: META_LIMIT,
            stderr_limit: META_LIMIT,
            deadline: Duration::from_secs(10),
            guard: None,
        },
    )
    .await
    .unwrap();
    let probe: Value = serde_json::from_slice(&probe_out.stdout).unwrap();
    let stream = &probe["streams"][0];
    assert!(stream["start_pts"].as_i64().unwrap() > 0);
    let (num, den) = stream["time_base"]
        .as_str()
        .unwrap()
        .split_once('/')
        .unwrap();
    let duration =
        (stream["duration_ts"].as_i64().unwrap() as u64) * num.parse::<u64>().unwrap() * 1000
            / den.parse::<u64>().unwrap();
    let mut input = fixture().input;
    input["sourceDurationMs"] = json!(duration);
    input["asset"]["sourceArtifactRef"] = source.to_json();
    input["asset"]["sourceArtifactSha256"] = json!(source.sha256);
    input["profile"]["maxImageBytes"] = json!(1024 * 1024);
    input["profile"]["maxTotalPixels"] = json!(100000);
    input["profile"]["deadlineMs"] = json!(60000);
    input["sourceProofRef"] = proof_for(&store, &input);
    let range = crate::media_frame_sample::plan_sample(&input).unwrap();
    let range_result = decode_sample(&store, &range, &tools, &registry)
        .await
        .unwrap();
    assert_eq!(range_result["status"], "complete");
    verify_sample_result(&store, &range, &range_result, &tools).unwrap();
    input["requestedTimeOrIntent"] =
        json!({"kind":"uniform_overview","timelineBasis":"relative_video_start"});
    input["profile"]["id"] = json!("bounded_uniform_overview_v1");
    let overview = crate::media_frame_sample::plan_sample(&input).unwrap();
    let overview_result = decode_sample(&store, &overview, &tools, &registry)
        .await
        .unwrap();
    assert_eq!(overview_result["status"], "complete");
    assert_eq!(overview_result["frames"].as_array().unwrap().len(), 6);
    verify_sample_result(&store, &overview, &overview_result, &tools).unwrap();
    // Independent demux/decode observation on this six-second synthetic input
    // proves captured signed PTS belong to actual source frames. Production
    // sampling never invokes show_frames or inventories the full source.
    let baseline_args = [
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_frames",
        "-show_entries",
        "frame=pts",
        "-of",
        "json",
    ]
    .iter()
    .map(|s| (*s).into())
    .chain([source_path.display().to_string()])
    .collect();
    let baseline_output = execute(
        &process,
        &ProcessSpec {
            kind: "fixture_pts_oracle",
            executable: tools.ffprobe.clone(),
            args: baseline_args,
            stdin: vec![],
            stdout_limit: META_LIMIT,
            stderr_limit: META_LIMIT,
            deadline: Duration::from_secs(10),
            guard: None,
        },
    )
    .await
    .unwrap();
    let baseline: Value = serde_json::from_slice(&baseline_output.stdout).unwrap();
    let actual_source_pts: std::collections::BTreeSet<i64> = baseline["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|frame| frame["pts"].as_i64().unwrap())
        .collect();
    for result in [&range_result, &overview_result] {
        for frame in result["frames"].as_array().unwrap() {
            assert!(actual_source_pts.contains(&frame["actualPts"].as_i64().unwrap()));
        }
    }
    assert_eq!(registry.snapshot().unwrap().active, 0);
    assert_eq!(registry.snapshot().unwrap().unresolved, 0);
}
