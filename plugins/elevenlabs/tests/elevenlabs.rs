use elevenlabs::ElevenLabs;
use serde_json::json;
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::protocol::{ErrorKind, Op, Request, State};
use shotline_plugin::{Error, Plugin};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn line(text: &str) -> Request {
    Request {
        op: Op::AudioTts,
        model: "eleven_v4".into(),
        prompt: Some(text.into()),
        voice: Some(json!({"voice_id": "v1", "settings": {"stability": 0.5}})),
        ..Request::default()
    }
}

#[tokio::test]
async fn quotes_by_character() {
    let el = ElevenLabs::new(None, "http://unused");
    let q = el.quote(&line("เราต้องไปต่อก่อนมืด")).await.unwrap();
    assert_eq!((q.amount, q.unit.as_str()), (19.0, "elevenlabs-credit"));
    let mut bad = line("x");
    bad.model = "eleven_nope".into();
    match el.quote(&bad).await {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("model")),
        other => panic!("{other:?}"),
    }
    match el.quote(&line("  ")).await {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("prompt")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn errors() {
    let e = elevenlabs::map_error(
        401,
        &json!({"detail": {"status": "invalid_api_key", "message": "Invalid API key"}}),
    );
    assert_eq!(
        (e.kind, e.message.as_str()),
        (ErrorKind::Auth, "Invalid API key")
    );
    let e = elevenlabs::map_error(
        401,
        &json!({"detail": {"status": "quota_exceeded", "message": "You have 3 credits left"}}),
    );
    assert_eq!(e.kind, ErrorKind::Quota);
    let e = elevenlabs::map_error(
        404,
        &json!({"detail": {"status": "voice_not_found", "message": "Voice not found"}}),
    );
    assert_eq!(
        (e.kind, e.field.as_deref()),
        (ErrorKind::Validation, Some("voice"))
    );
    let e = elevenlabs::map_error(
        422,
        &json!({"detail": [{"loc": ["body", "voice_settings", "stability"], "msg": "bad"}]}),
    );
    assert_eq!(e.field.as_deref(), Some("voice"));
}

async fn mock() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/text-to-speech/v1"))
        .and(query_param("output_format", "mp3_44100_128"))
        .and(header("xi-api-key", "k"))
        .and(body_partial_json(
            json!({"model_id": "eleven_v4", "voice_settings": {"stability": 0.5}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(b"mp3".to_vec()),
        )
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/user/subscription"))
        .and(header("xi-api-key", "k"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tier": "starter", "currency": "usd",
            "character_count": 24761, "character_limit": 40000,
            "next_invoice": {"amount_due_cents": 600, "subtotal_cents": 600}
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/voices"))
        .and(header("xi-api-key", "k"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"voices": [
            {"voice_id": "abc", "name": "Rachel", "description": "",
             "labels": {"accent": "american", "gender": "female"},
             "preview_url": "https://example.com/rachel.mp3"},
            {"voice_id": "def", "name": "Somchai", "description": "warm Thai narrator"}
        ]})))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn lists_voices() {
    let s = mock().await;
    let el = ElevenLabs::new(Some("k".into()), &format!("{}/v1", s.uri()));
    let v = el.voices().await.unwrap().voices;
    assert_eq!(v.len(), 2);
    assert_eq!((v[0].id.as_str(), v[0].name.as_str()), ("abc", "Rachel"));
    assert_eq!(v[0].description.as_deref(), Some("american, female"));
    assert_eq!(
        v[0].preview.as_deref(),
        Some("https://example.com/rachel.mp3")
    );
    assert_eq!(v[1].description.as_deref(), Some("warm Thai narrator"));
    let d = el.describe().await.unwrap();
    let speech = d.models.iter().filter(|m| m.op == Op::AudioTts);
    assert!(speech.clone().count() > 0 && speech.clone().all(|m| m.voice_schema.is_some()));
    assert!(d.models.iter().any(|m| m.op == Op::AudioSfx));
}

#[tokio::test]
async fn rate_and_balance_from_the_plan() {
    let s = mock().await;
    let el = ElevenLabs::new(Some("k".into()), &format!("{}/v1", s.uri()));
    let r = el.rates().await.unwrap().rates;
    assert_eq!(r.len(), 1);
    assert_eq!(
        (r[0].unit.as_str(), r[0].currency.as_str()),
        ("elevenlabs-credit", "USD")
    );
    assert!((r[0].amount - 0.00015).abs() < 1e-12);
    let b = el.balance().await.unwrap().balances;
    assert_eq!(
        (b[0].amount, b[0].unit.as_str()),
        (15239.0, "elevenlabs-credit")
    );
    assert!(
        elevenlabs::credit_rate(&json!({"character_limit": 10000, "currency": "usd",
            "next_invoice": {"amount_due_cents": 0}}))
        .is_none(),
        "free plans have no rate"
    );
}

#[tokio::test]
async fn speaks() {
    let s = mock().await;
    let el = ElevenLabs::new(Some("k".into()), &format!("{}/v1", s.uri()));
    let dir = tempfile::tempdir().unwrap();
    let job = el.run(&line("hello"), dir.path()).await.unwrap();
    assert_eq!(el.poll(&job).await.unwrap().state, State::Done);
    let out = el.fetch(&job, dir.path()).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join(&out.outputs[0].file)).unwrap(),
        b"mp3"
    );

    let mut no_voice = line("hello");
    no_voice.voice = None;
    match el.run(&no_voice, dir.path()).await {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("voice")),
        other => panic!("{other:?}"),
    }
}

fn effect(text: &str, length: Option<f64>) -> Request {
    Request {
        op: Op::AudioSfx,
        model: "eleven_text_to_sound_v2".into(),
        prompt: Some(text.into()),
        duration: length,
        ..Request::default()
    }
}

#[tokio::test]
async fn quotes_sound_effects_by_length() {
    let el = ElevenLabs::new(None, "http://unused");
    let q = el.quote(&effect("a door slams", Some(2.5))).await.unwrap();
    assert_eq!((q.amount, q.units), (100.0, Some(2.5)));
    let q = el.quote(&effect("a door slams", None)).await.unwrap();
    assert_eq!(q.amount, 200.0);
    match el.quote(&effect("a door slams", Some(40.0))).await {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("duration")),
        other => panic!("{other:?}"),
    }
    let mut speech_model = effect("a door slams", None);
    speech_model.model = "eleven_v4".into();
    assert!(el.quote(&speech_model).await.is_err());
}

#[tokio::test]
async fn makes_a_sound_effect() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sound-generation"))
        .and(query_param("output_format", "mp3_44100_128"))
        .and(header("xi-api-key", "k"))
        .and(body_partial_json(json!({
            "text": "a door slams", "model_id": "eleven_text_to_sound_v2",
            "duration_seconds": 2.0, "prompt_influence": 0.6
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "audio/mpeg")
                .set_body_bytes(b"bang".to_vec()),
        )
        .mount(&s)
        .await;
    let el = ElevenLabs::new(Some("k".into()), &format!("{}/v1", s.uri()));
    let dir = tempfile::tempdir().unwrap();
    let mut req = effect("a door slams", Some(2.0));
    req.params.insert("prompt_influence".into(), json!(0.6));
    let job = el.run(&req, dir.path()).await.unwrap();
    let out = el.fetch(&job, dir.path()).await.unwrap();
    assert_eq!(
        std::fs::read(dir.path().join(&out.outputs[0].file)).unwrap(),
        b"bang"
    );
}

#[tokio::test]
async fn conforms() {
    let s = mock().await;
    let dir = tempfile::tempdir().unwrap();
    // The kit's sample requests have no voice, so only the offline checks apply here.
    let checks = conformance::check(
        &ElevenLabs::new(Some("k".into()), &format!("{}/v1", s.uri())),
        &Options::offline(dir.path()),
    )
    .await;
    let failed: Vec<_> = checks.iter().filter(|c| c.problem.is_some()).collect();
    assert!(failed.is_empty(), "{failed:#?}");
}
