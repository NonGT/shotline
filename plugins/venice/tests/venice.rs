use serde_json::json;
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::protocol::{ErrorKind, MediaKind, Op, Reference, Request, State};
use shotline_plugin::{Error, Plugin};
use venice::Venice;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const H3: &str = "minimax-h3-max-reference-to-video";

fn req(dir: &std::path::Path) -> Request {
    let img = dir.join("a.png");
    std::fs::write(&img, b"png").unwrap();
    let wav = dir.join("line.wav");
    std::fs::write(&wav, b"wav").unwrap();
    Request {
        op: Op::VideoGenerate,
        model: H3.into(),
        prompt: Some("<Picture 1> speaks".into()),
        duration: Some(8.0),
        resolution: Some("480P".into()),
        aspect: Some("9:16".into()),
        references: vec![
            Reference {
                kind: MediaKind::Audio,
                path: wav,
            },
            Reference {
                kind: MediaKind::Image,
                path: img,
            },
        ],
        ..Request::default()
    }
}

#[test]
fn body_shape() {
    let dir = tempfile::tempdir().unwrap();
    let (b, refs) = venice::body(&req(dir.path())).unwrap();
    assert_eq!(b["duration"], "8s");
    assert_eq!(b["aspect_ratio"], "9:16");
    assert!(b["audio_url"].as_str().unwrap().starts_with("data:audio/wav;base64,"));
    assert_eq!(b["reference_image_urls"].as_array().unwrap().len(), 1);
    assert_eq!(refs, [1]);

    let mut r = req(dir.path());
    r.start_frame = Some(dir.path().join("a.png"));
    match venice::body(&r) {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("start_frame")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn zod_errors_point_at_fields() {
    let e = venice::map_error(
        400,
        &json!({"error": "Invalid request", "details": {"reference_image_urls": {"_errors": [], "0": {"_errors": ["Image too large"]}}}}),
        &[1],
    );
    assert_eq!(e.kind, ErrorKind::Validation);
    assert_eq!(e.field.as_deref(), Some("references[1]"));
    assert_eq!(e.message, "Invalid request: Image too large");
    assert_eq!(venice::map_error(402, &json!({"error": "Insufficient balance"}), &[]).kind, ErrorKind::Quota);
    assert_eq!(
        venice::map_error(422, &json!({"error": "Content policy violation"}), &[]).kind,
        ErrorKind::ContentPolicy
    );
}

async fn mock() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/video/quote"))
        .and(body_partial_json(json!({"model": "no/such-model"})))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "Invalid model"})))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/video/quote"))
        .and(header("Authorization", "Bearer k"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"quote": 0.48})))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/video/queue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"queue_id": "q1", "model": H3})))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/video/retrieve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "PROCESSING", "progress": 40})))
        .up_to_n_times(1)
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/video/retrieve"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "video/mp4").set_body_bytes(b"mp4".to_vec()))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn full_job() {
    let s = mock().await;
    let v = Venice::new(Some("k".into()), &s.uri());
    let dir = tempfile::tempdir().unwrap();
    let r = req(dir.path());
    let q = v.quote(&r).await.unwrap();
    assert_eq!((q.amount, q.unit.as_str()), (0.48, "USD"));
    let job = v.run(&r, dir.path()).await.unwrap();
    let st = v.poll(&job).await.unwrap();
    assert_eq!((st.state, st.progress), (State::Running, Some(0.4)));
    assert_eq!(v.poll(&job).await.unwrap().state, State::Done);
    let out = v.fetch(&job, &dir.path().join("o")).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("o").join(&out.outputs[0].file)).unwrap(), b"mp4");
    assert!(matches!(v.cancel(&job).await, Err(Error::Unsupported(_))));
}

#[tokio::test]
async fn conforms() {
    let s = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        run_jobs: true,
        ..Options::offline(dir.path())
    };
    let checks = conformance::check(&Venice::new(Some("k".into()), &s.uri()), &opts).await;
    let failed: Vec<_> = checks.iter().filter(|c| c.problem.is_some()).collect();
    assert!(failed.is_empty(), "{failed:#?}");
}
