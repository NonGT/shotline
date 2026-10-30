use serde_json::json;
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::protocol::{ErrorKind, Op, Request, State};
use shotline_plugin::{Error, Plugin};
use wavespeed::WaveSpeed;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn req(dir: &std::path::Path) -> Request {
    let v = dir.join("in.mp4");
    let a = dir.join("in.mp3");
    std::fs::write(&v, b"mp4").unwrap();
    std::fs::write(&a, b"mp3").unwrap();
    let mut r = Request {
        op: Op::VideoLipsync,
        model: "sync/lipsync-3".into(),
        duration: Some(8.0),
        ..Request::default()
    };
    r.inputs.insert("video".into(), v);
    r.inputs.insert("audio".into(), a);
    r.params.insert("sync_mode".into(), json!("silence"));
    r
}

async fn mock() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v3/sync/lipsync-3"))
        .and(header("Authorization", "Bearer k"))
        .and(body_partial_json(json!({"sync_mode": "silence"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 200, "data": {"id": "p1", "status": "created"}})),
        )
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v3/sync/lipsync-3"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 200, "data": {"id": "p1", "status": "created"}})),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/predictions/p1/result"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 200, "data": {"status": "processing"}})),
        )
        .up_to_n_times(1)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/predictions/p1/result"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code": 200, "data": {
                "status": "completed", "outputs": [format!("{}/files/o.mp4", s.uri())]
            }})),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/predictions/no-such-job/result"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"code": 404, "message": "prediction not found"})),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code": 200, "data": [
                {"model_id": "sync/lipsync-2", "base_price": 0.05},
                {"model_id": "sync/lipsync-3", "base_price": 0.135}
            ]})),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v3/balance"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 200, "data": {"balance": 2.72}})),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/o.mp4"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"synced".to_vec()))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn quotes_the_listed_price() {
    let s = mock().await;
    let w = WaveSpeed::new(Some("k".into()), &format!("{}/api/v3", s.uri()));
    let dir = tempfile::tempdir().unwrap();
    let q = w.quote(&req(dir.path())).await.unwrap();
    assert_eq!(q.unit, "USD");
    assert!((q.amount - 1.08).abs() < 1e-9, "{}", q.amount);
    assert_eq!(q.units, Some(8.0));
    let b = w.balance().await.unwrap().balances;
    assert_eq!((b[0].amount, b[0].unit.as_str()), (2.72, "USD"));
    let mut r = req(dir.path());
    r.inputs.remove("audio");
    match w.quote(&r).await {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("inputs.audio")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn syncs() {
    let s = mock().await;
    let w = WaveSpeed::new(Some("k".into()), &format!("{}/api/v3", s.uri()));
    let dir = tempfile::tempdir().unwrap();
    let job = w.run(&req(dir.path()), dir.path()).await.unwrap();
    assert_eq!(job, "p1");
    assert_eq!(w.poll(&job).await.unwrap().state, State::Running);
    assert_eq!(w.poll(&job).await.unwrap().state, State::Done);
    let out = w.fetch(&job, &dir.path().join("o")).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("o").join(&out.outputs[0].file)).unwrap(),
        b"synced"
    );
}

#[test]
fn errors() {
    assert_eq!(
        wavespeed::map_error(400, &json!({"message": "Insufficient balance"})).kind,
        ErrorKind::Quota
    );
    assert_eq!(
        wavespeed::map_error(401, &json!({"message": "bad key"})).kind,
        ErrorKind::Auth
    );
}

#[tokio::test]
async fn conforms() {
    let s = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut opts = Options::offline(dir.path());
    opts.run_jobs = true;
    let checks = conformance::check(
        &WaveSpeed::new(Some("k".into()), &format!("{}/api/v3", s.uri())),
        &opts,
    )
    .await;
    let failed: Vec<_> = checks.iter().filter(|c| c.problem.is_some()).collect();
    assert!(failed.is_empty(), "{failed:#?}");
}
