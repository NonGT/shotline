use serde_json::json;
use shotline_builtin::fal::{self, Fal, RefMap};
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::protocol::{ErrorKind, MediaKind, Op, Reference, Request, State};
use shotline_plugin::{Error, Plugin};
use wiremock::matchers::{body_partial_json, header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const H3: &str = "minimax/h3-max/reference-to-video";

fn h3(dir: &std::path::Path) -> Request {
    let img = |n: &str| {
        let p = dir.join(n);
        std::fs::write(&p, b"png").unwrap();
        Reference {
            kind: MediaKind::Image,
            path: p,
        }
    };
    let audio = dir.join("line.mp3");
    std::fs::write(&audio, b"mp3").unwrap();
    Request {
        op: Op::VideoGenerate,
        model: H3.into(),
        prompt: Some("Image 1 talks to Image 2".into()),
        duration: Some(8.0),
        resolution: Some("768P".into()),
        aspect: Some("9:16".into()),
        references: vec![
            img("a.png"),
            Reference {
                kind: MediaKind::Audio,
                path: audio,
            },
            img("b.jpg"),
        ],
        ..Request::default()
    }
}

#[test]
fn input_fields_are_top_level() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = h3(dir.path());
    req.params
        .insert("enable_safety_checker".into(), json!(false));
    req.params
        .insert("prompt_expansion_mode".into(), json!("balanced"));
    let (body, refs) = fal::input(&req).unwrap();
    assert_eq!(body["prompt"], "Image 1 talks to Image 2");
    assert_eq!(body["duration"], 8.0);
    assert_eq!(body["aspect_ratio"], "9:16");
    assert_eq!(body["reference_image_urls"].as_array().unwrap().len(), 2);
    assert!(
        body["reference_image_urls"][1]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
    assert!(
        body["reference_audio_urls"][0]
            .as_str()
            .unwrap()
            .starts_with("data:audio/mpeg;base64,")
    );
    assert_eq!(body["enable_safety_checker"], false);
    assert_eq!(body["prompt_expansion_mode"], "balanced", "params win");
    assert!(!body.contains_key("input"));
    assert_eq!(refs.images, [0, 2]);
    assert_eq!(refs.audio, [1]);

    req.op = Op::AudioTts;
    match fal::input(&req) {
        Err(Error::Plugin(e)) => assert_eq!(e.kind, ErrorKind::Unsupported),
        other => panic!("{other:?}"),
    }
}

#[test]
fn errors_point_back_at_the_request() {
    let refs = RefMap {
        images: vec![0, 2],
        audio: vec![1],
        video: vec![],
    };
    let e = fal::map_error(
        422,
        &json!({"detail": [{"loc": ["body", "reference_image_urls", 1], "msg": "Image too small", "type": "value_error"}]}),
        &refs,
    );
    assert_eq!(
        (e.kind, e.field.as_deref()),
        (ErrorKind::Validation, Some("references[2]"))
    );
    assert_eq!(e.message, "Image too small");

    let e = fal::map_error(
        422,
        &json!({"detail": [{"loc": ["body", "image_url"], "msg": "flagged", "type": "content_policy_violation"}]}),
        &refs,
    );
    assert_eq!(
        (e.kind, e.field.as_deref()),
        (ErrorKind::ContentPolicy, Some("start_frame"))
    );

    let e = fal::map_error(
        422,
        &json!({"detail": [{"loc": ["body", "prompt_expansion_mode"], "msg": "Field required", "type": "missing"}]}),
        &refs,
    );
    assert_eq!(e.field.as_deref(), Some("params.prompt_expansion_mode"));

    assert_eq!(
        fal::map_error(401, &json!({"detail": "No key"}), &refs).kind,
        ErrorKind::Auth
    );
    let e = fal::map_error(
        403,
        &json!({"detail": "User is locked. Reason: Exhausted balance."}),
        &refs,
    );
    assert_eq!(e.kind, ErrorKind::Quota);
    assert_eq!(e.message, "User is locked. Reason: Exhausted balance.");
}

async fn mock_fal() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/models/pricing/estimate"))
        .and(body_partial_json(
            json!({"endpoints": {"no/such-model": {}}}),
        ))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"detail": "Endpoint not found"})),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/models/pricing/estimate"))
        .and(header("Authorization", "Key test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"estimate_type": "unit_price", "total_cost": 0.4, "currency": "USD"}),
        ))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/{H3}")))
        .and(body_partial_json(
            json!({"prompt_expansion_mode": "disabled"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"request_id": "req-1"})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/minimax/h3-max/requests/req-1/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "IN_QUEUE", "queue_position": 2})),
        )
        .up_to_n_times(1)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/minimax/h3-max/requests/req-1/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "COMPLETED"})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/minimax/h3-max/requests/req-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "video": {"url": format!("{}/files/v.mp4", s.uri())},
            "seed": 7
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/v.mp4"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"mp4 bytes".to_vec()))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/fal-ai/nano-banana(-pro)?(/edit)?$"))
        .and(body_partial_json(
            json!({"num_images": 1, "output_format": "png"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"request_id": "img-1"})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/fal-ai/nano-banana(-pro)?/requests/img-1/status$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "COMPLETED"})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/fal-ai/nano-banana(-pro)?/requests/img-1$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "images": [{"url": format!("{}/files/i.png", s.uri()), "content_type": "image/png"}],
            "description": ""
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/i.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"png bytes".to_vec()))
        .mount(&s)
        .await;
    Mock::given(method("PUT"))
        .and(path("/minimax/h3-max/requests/req-1/cancel"))
        .respond_with(
            ResponseTemplate::new(202).set_body_json(json!({"status": "CANCELLATION_REQUESTED"})),
        )
        .mount(&s)
        .await;
    s
}

fn fal_at(s: &MockServer) -> Fal {
    Fal::new(
        Some("test-key".into()),
        &s.uri(),
        &format!("{}/v1", s.uri()),
    )
}

#[tokio::test]
async fn quote_run_poll_fetch() {
    let s = mock_fal().await;
    let fal = fal_at(&s);
    let dir = tempfile::tempdir().unwrap();
    let req = h3(dir.path());

    let q = fal.quote(&req).await.unwrap();
    assert!(
        (q.amount - 0.64).abs() < 1e-9,
        "768P is 1.6x the 480P estimate"
    );
    assert_eq!(q.unit, "USD");

    let job = fal.run(&req, dir.path()).await.unwrap();
    assert_eq!(job, format!("{H3}#req-1"));
    assert_eq!(fal.poll(&job).await.unwrap().state, State::Queued);
    assert_eq!(fal.poll(&job).await.unwrap().state, State::Done);
    let out = fal.fetch(&job, &dir.path().join("out")).await.unwrap();
    assert_eq!(out.outputs[0].file, "out.mp4");
    assert_eq!(
        std::fs::read(dir.path().join("out/out.mp4")).unwrap(),
        b"mp4 bytes"
    );
    assert_eq!(out.data.unwrap()["seed"], 7);
    fal.cancel(&job).await.unwrap();
}

#[tokio::test]
async fn images_with_references_go_to_edit() {
    let s = mock_fal().await;
    let fal = fal_at(&s);
    let dir = tempfile::tempdir().unwrap();
    let mut req = Request {
        op: Op::ImageGenerate,
        model: "fal-ai/nano-banana".into(),
        prompt: Some("image 1 in a forest".into()),
        aspect: Some("9:16".into()),
        ..Request::default()
    };
    assert_eq!(fal::endpoint(&req), "fal-ai/nano-banana");
    let anchor = dir.path().join("a.png");
    std::fs::write(&anchor, b"png").unwrap();
    req.references.push(Reference {
        kind: MediaKind::Image,
        path: anchor,
    });
    assert_eq!(fal::endpoint(&req), "fal-ai/nano-banana/edit");
    let (body, _) = fal::input(&req).unwrap();
    assert_eq!(body["aspect_ratio"], "9:16");
    assert_eq!(body["image_urls"].as_array().unwrap().len(), 1);
    assert!(!body.contains_key("prompt_expansion_mode"));

    let q = fal.quote(&req).await.unwrap();
    assert_eq!(
        (q.units, q.unit_name.as_deref()),
        (Some(1.0), Some("image"))
    );
    let job = fal.run(&req, dir.path()).await.unwrap();
    assert_eq!(job, "fal-ai/nano-banana/edit#img-1");
    assert_eq!(fal.poll(&job).await.unwrap().state, State::Done);
    let out = fal.fetch(&job, &dir.path().join("out")).await.unwrap();
    assert_eq!(out.outputs[0].file, "out.png");
    assert_eq!(out.outputs[0].kind, MediaKind::Image);
    assert_eq!(
        std::fs::read(dir.path().join("out/out.png")).unwrap(),
        b"png bytes"
    );

    req.references[0].kind = MediaKind::Audio;
    match fal::input(&req) {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("references[0]")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn failures() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/{H3}")))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({"detail": [
            {"loc": ["body", "reference_image_urls", 0], "msg": "flagged", "type": "content_policy_violation"}
        ]})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/minimax/h3-max/requests/req-2/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "COMPLETED", "error": "Output flagged", "error_type": "content_policy_violation"
        })))
        .mount(&s)
        .await;
    let fal = fal_at(&s);
    let dir = tempfile::tempdir().unwrap();
    match fal.run(&h3(dir.path()), dir.path()).await {
        Err(Error::Plugin(e)) => {
            assert_eq!(e.kind, ErrorKind::ContentPolicy);
            assert_eq!(e.field.as_deref(), Some("references[0]"));
        }
        other => panic!("{other:?}"),
    }
    let st = fal.poll(&format!("{H3}#req-2")).await.unwrap();
    assert_eq!(st.state, State::Failed);
    assert_eq!(st.error.unwrap().kind, ErrorKind::ContentPolicy);

    let no_key = Fal::new(None, &s.uri(), &s.uri());
    match no_key.quote(&h3(dir.path())).await {
        Err(Error::Plugin(e)) => assert_eq!(e.kind, ErrorKind::Auth),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn conforms() {
    let s = mock_fal().await;
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        run_jobs: true,
        ..Options::offline(dir.path())
    };
    let checks = conformance::check(&fal_at(&s), &opts).await;
    let failed: Vec<_> = checks.iter().filter(|c| c.problem.is_some()).collect();
    assert!(failed.is_empty(), "{failed:#?}");
}

/// Hits fal's free pricing endpoint. Needs FAL_API_KEY and SHOTLINE_LIVE=1.
#[tokio::test]
#[ignore]
async fn live_quote() {
    if std::env::var("SHOTLINE_LIVE").as_deref() != Ok("1") {
        return;
    }
    let fal = Fal::from_env(std::env::var(fal::KEY_VAR).ok());
    let dir = tempfile::tempdir().unwrap();
    let mut req = h3(dir.path());
    req.resolution = Some("480P".into());
    let q = fal.quote(&req).await.unwrap();
    println!("{q:?}");
    assert!(q.amount > 0.0 && q.amount < 5.0);
}
