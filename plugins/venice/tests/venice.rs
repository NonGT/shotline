use serde_json::json;
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::protocol::{ErrorKind, MediaKind, Op, Reference, Request, State};
use shotline_plugin::{Error, Plugin};
use venice::Venice;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
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
    assert!(
        b["audio_url"]
            .as_str()
            .unwrap()
            .starts_with("data:audio/wav;base64,")
    );
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
    assert_eq!(
        venice::map_error(402, &json!({"error": "Insufficient balance"}), &[]).kind,
        ErrorKind::Quota
    );
    assert_eq!(
        venice::map_error(422, &json!({"error": "Content policy violation"}), &[]).kind,
        ErrorKind::ContentPolicy
    );
}

/// Trimmed from Venice's real model lists.
fn image_list() -> serde_json::Value {
    let flat = |id: &str, usd: f64| json!({"id": id, "model_spec": {"pricing": {"generation": {"usd": usd}}}});
    let tiers = |id: &str| {
        json!({"id": id, "model_spec": {
            "pricing": {"resolutions": {"1K": {"usd": 0.1}, "2K": {"usd": 0.14}, "4K": {"usd": 0.19}}},
            "constraints": {"defaultResolution": "1K"}
        }})
    };
    json!({"data": [
        tiers("nano-banana-2"), tiers("nano-banana-pro"), flat("seedream-v5-lite", 0.05),
        flat("seedream-v4", 0.05), tiers("qwen-image-2-1-pro"), tiers("gpt-image-2"),
    ]})
}

fn edit_list() -> serde_json::Value {
    let data: Vec<_> = ["nano-banana-2", "nano-banana-pro", "seedream-v5-lite", "seedream-v4", "qwen-image-2-1-pro"]
        .iter()
        .map(|id| json!({"id": format!("{id}-edit"), "model_spec": {"pricing": {"inpaint": {"usd": 0.05}}}}))
        .chain([json!({"id": "gpt-image-2-edit", "model_spec": {"pricing": {
            "inpaint": {"usd": 0.31},
            "inputImages": {"included": 1, "additional": {"usd": 0.03}}
        }}})])
        .collect();
    json!({"data": data})
}

/// Per million tokens, in and out.
fn text_list() -> serde_json::Value {
    let data: Vec<_> = venice::models()
        .iter()
        .filter(|m| m.op == Op::TextGenerate)
        .map(|m| {
            json!({"id": m.id, "model_spec": {"pricing": {
                "input": {"usd": 0.1, "diem": 0.1}, "output": {"usd": 0.5, "diem": 0.5}
            }}})
        })
        .collect();
    json!({"data": data})
}

async fn mock() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(query_param("type", "text"))
        .respond_with(ResponseTemplate::new(200).set_body_json(text_list()))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(
            json!({"messages": [{"role": "user", "content": "empty"}]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "<think>hmm</think>"}}]
        })))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({
            "venice_parameters": {"include_venice_system_prompt": false}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant",
                "content": "<think>plan</think>\n@paka walks into the trees at dusk."}}],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 200, "total_tokens": 1200}
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(query_param("type", "image"))
        .respond_with(ResponseTemplate::new(200).set_body_json(image_list()))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(query_param("type", "inpaint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(edit_list()))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/image/generate"))
        .and(body_partial_json(json!({"prompt": "flagged"})))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-venice-is-content-violation", "true")
                .set_body_json(json!({"images": ["cG5nIGJ5dGVz"]})),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/image/generate"))
        .and(body_partial_json(json!({"format": "png"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "x", "images": ["cG5nIGJ5dGVz"]})),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/image/multi-edit"))
        .and(body_partial_json(
            json!({"modelId": "nano-banana-2-edit", "output_format": "png"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(b"edited png".to_vec()),
        )
        .mount(&s)
        .await;
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
    Mock::given(method("GET"))
        .and(path("/api_keys/rate_limits"))
        .and(header("Authorization", "Bearer k"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
            "balances": {"USD": 3.8197, "DIEM": 0, "BUNDLED_CREDITS": 0},
            "rateLimits": []
        }})))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/video/queue"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"queue_id": "q1", "model": H3})),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/video/retrieve"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "PROCESSING", "progress": 40})),
        )
        .up_to_n_times(1)
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/video/retrieve"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "video/mp4")
                .set_body_bytes(b"mp4".to_vec()),
        )
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
    assert_eq!(
        std::fs::read(dir.path().join("o").join(&out.outputs[0].file)).unwrap(),
        b"mp4"
    );
    assert!(matches!(v.cancel(&job).await, Err(Error::Unsupported(_))));
    let b = v.balance().await.unwrap().balances;
    let units: Vec<&str> = b.iter().map(|a| a.unit.as_str()).collect();
    assert_eq!(units, ["venice-bundled-credits", "venice-diem", "USD"]);
    assert_eq!(b[2].amount, 3.8197);
}

#[tokio::test]
async fn images() {
    let s = mock().await;
    let v = Venice::new(Some("k".into()), &s.uri());
    let dir = tempfile::tempdir().unwrap();
    let mut r = Request {
        op: Op::ImageGenerate,
        model: "nano-banana-2".into(),
        prompt: Some("a girl in a forest".into()),
        aspect: Some("9:16".into()),
        resolution: Some("2K".into()),
        ..Request::default()
    };
    assert_eq!(v.quote(&r).await.unwrap().amount, 0.14);
    let job = v.run(&r, dir.path()).await.unwrap();
    assert_eq!(v.poll(&job).await.unwrap().state, State::Done);
    let out = v.fetch(&job, &dir.path().join("o")).await.unwrap();
    assert_eq!(out.outputs[0].file, "out.png");
    assert_eq!(
        std::fs::read(dir.path().join("o/out.png")).unwrap(),
        b"png bytes"
    );

    // References go to the edit twin, which answers with the image itself.
    let img = dir.path().join("a.png");
    std::fs::write(&img, b"png").unwrap();
    r.references = vec![
        Reference {
            kind: MediaKind::Image,
            path: img.clone(),
        };
        2
    ];
    let (path, b, _) = venice::image_body(&r).unwrap();
    assert_eq!(path, "/image/multi-edit");
    assert_eq!(b["images"].as_array().unwrap().len(), 2);
    assert_eq!(v.quote(&r).await.unwrap().amount, 0.05);
    let job = v.run(&r, dir.path()).await.unwrap();
    v.fetch(&job, &dir.path().join("e")).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("e/out.png")).unwrap(),
        b"edited png"
    );

    r.model = "gpt-image-2".into();
    let q = v.quote(&r).await.unwrap().amount;
    assert!(
        (q - 0.34).abs() < 1e-9,
        "one extra input image at 0.03: {q}"
    );

    r.references.clear();
    r.model = "nano-banana-2".into();
    r.prompt = Some("flagged".into());
    match v.run(&r, dir.path()).await {
        Err(Error::Plugin(e)) => assert_eq!(e.kind, ErrorKind::ContentPolicy),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn text() {
    let s = mock().await;
    let v = Venice::new(Some("k".into()), &s.uri());
    let dir = tempfile::tempdir().unwrap();
    let mut r = Request {
        op: Op::TextGenerate,
        model: "claude-haiku-5-5".into(),
        prompt: Some("ผกาเดินเข้าป่า".into()),
        ..Request::default()
    };
    // 14 characters in, and the whole 2048 tokens out.
    let q = v.quote(&r).await.unwrap().amount;
    assert!((q - (14.0 * 0.1 + 2048.0 * 0.5) / 1e6).abs() < 1e-12, "{q}");
    let job = v.run(&r, dir.path()).await.unwrap();
    let st = v.poll(&job).await.unwrap();
    assert_eq!(st.state, State::Done);
    let cost = st.cost.unwrap();
    assert!((cost.amount - 0.0002).abs() < 1e-12, "{cost:?}");
    assert_eq!(cost.units, Some(1200.0));
    let out = v.fetch(&job, &dir.path().join("o")).await.unwrap();
    assert_eq!(out.outputs[0].file, "out.txt");
    assert_eq!(out.outputs[0].kind, MediaKind::Data);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("o/out.txt")).unwrap(),
        "@paka walks into the trees at dusk."
    );

    r.prompt = Some("empty".into());
    assert!(
        v.run(&r, dir.path()).await.is_err(),
        "only thinking, no answer"
    );
    r.model = "no-such-text".into();
    assert!(v.quote(&r).await.is_err());
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
