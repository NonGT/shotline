use std::path::PathBuf;

use serde_json::json;
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::discovery::{Found, Source, describe_all};
use shotline_plugin::fake::{FakePlugin, KEY_VAR};
use shotline_plugin::protocol::{ErrorKind, Op, Request, State};
use shotline_plugin::{Error, Plugin, ProcessPlugin};

fn fake_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_shotline-plugin-fake"))
}

fn fake_process(env: Vec<(String, String)>) -> ProcessPlugin {
    ProcessPlugin::new("fake", fake_exe(), vec![], env)
}

fn video(params: serde_json::Value) -> Request {
    Request {
        op: Op::VideoGenerate,
        model: "fake/video".into(),
        prompt: Some("a fox".into()),
        duration: Some(4.0),
        params: params.as_object().unwrap().clone(),
        ..Request::default()
    }
}

async fn assert_conforms(p: &dyn Plugin) {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        run_jobs: true,
        ..Options::offline(dir.path())
    };
    let checks = conformance::check(p, &opts).await;
    let failed: Vec<_> = checks.iter().filter(|c| c.problem.is_some()).collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(checks.len() > 8);
}

#[tokio::test]
async fn fake_conforms_in_process() {
    assert_conforms(&FakePlugin::default()).await;
}

#[tokio::test]
async fn fake_conforms_over_stdio() {
    assert_conforms(&fake_process(vec![])).await;
}

#[tokio::test]
async fn job_lifecycle_and_costs() {
    let p = fake_process(vec![]);
    let q = p.quote(&video(json!({}))).await.unwrap();
    assert_eq!((q.amount, q.unit.as_str()), (0.4, "USD"));

    let dir = tempfile::tempdir().unwrap();
    let job = p
        .run(&video(json!({"fake_polls": 2})), dir.path())
        .await
        .unwrap();
    assert_eq!(p.poll(&job).await.unwrap().state, State::Running);
    assert_eq!(p.poll(&job).await.unwrap().state, State::Running);
    let done = p.poll(&job).await.unwrap();
    assert_eq!(done.state, State::Done);
    assert_eq!(done.cost.unwrap().amount, 0.4);
    let out = p.fetch(&job, dir.path()).await.unwrap();
    assert!(dir.path().join(&out.outputs[0].file).is_file());

    let job = p
        .run(
            &video(json!({"fake_fail": "content_policy", "fake_charge_on_fail": true})),
            dir.path(),
        )
        .await
        .unwrap();
    p.poll(&job).await.unwrap();
    let failed = p.poll(&job).await.unwrap();
    assert_eq!(failed.state, State::Failed);
    assert_eq!(failed.error.unwrap().kind, ErrorKind::ContentPolicy);
    assert!(failed.cost.is_some(), "charged even though it failed");

    let job = p.run(&video(json!({})), dir.path()).await.unwrap();
    p.cancel(&job).await.unwrap();
    assert_eq!(p.poll(&job).await.unwrap().state, State::Cancelled);
}

#[tokio::test]
async fn errors_keep_their_shape_over_stdio() {
    let p = fake_process(vec![]);
    match p.quote(&video(json!({"fake_reject": "quota"}))).await {
        Err(Error::Plugin(e)) => {
            assert_eq!(e.kind, ErrorKind::Quota);
            assert_eq!(e.field.as_deref(), Some("params.fake_reject"));
        }
        other => panic!("{other:?}"),
    }
    let mut req = video(json!({}));
    req.references = (0..5)
        .map(|_| shotline_plugin::protocol::Reference {
            kind: shotline_plugin::protocol::MediaKind::Image,
            path: "/x.png".into(),
        })
        .collect();
    match p.quote(&req).await {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("references[4]")),
        other => panic!("{other:?}"),
    }
    assert!(
        p.usage("2026-01-01T00:00:00Z")
            .await
            .unwrap()
            .charges
            .is_empty()
    );
    assert_eq!(p.rates().await.unwrap().rates[0].unit, "fake-credit");
}

#[tokio::test]
async fn only_declared_env_reaches_the_plugin() {
    // cargo sets CARGO_PKG_NAME for this test process; it must not leak through.
    assert!(std::env::var("CARGO_PKG_NAME").is_ok());
    let p = fake_process(vec![(KEY_VAR.into(), "secret".into())]);
    let dir = tempfile::tempdir().unwrap();
    let job = p
        .run(&video(json!({"fake_polls": 0})), dir.path())
        .await
        .unwrap();
    p.poll(&job).await.unwrap();
    let data = p.fetch(&job, dir.path()).await.unwrap().data.unwrap();
    let env: Vec<String> = serde_json::from_value(data["env"].clone()).unwrap();
    assert!(env.contains(&KEY_VAR.to_string()));
    assert!(!env.contains(&"CARGO_PKG_NAME".to_string()), "{env:?}");
}

#[cfg(unix)]
mod broken {
    use super::*;

    fn sh(script: &str) -> ProcessPlugin {
        ProcessPlugin::new("sh", "/bin/sh", vec!["-c".into(), script.into()], vec![])
    }

    fn message(r: shotline_plugin::Result<shotline_plugin::protocol::Description>) -> String {
        match r {
            Err(Error::Broken { message, .. }) => message,
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn crash_includes_stderr() {
        let m = message(sh("echo boom >&2; exit 3").describe().await);
        assert!(m.contains("plugin exited") && m.contains("boom"), "{m}");
    }

    #[tokio::test]
    async fn garbage_on_stdout() {
        let m = message(sh("read l; echo hello; sleep 1").describe().await);
        assert!(m.contains("isn't a response"), "{m}");
    }

    #[tokio::test]
    async fn hang_times_out_and_restarts() {
        let mut p = sh("sleep 5");
        p.timeout_scale = 0.002;
        let m = message(p.describe().await);
        assert!(m.contains("no reply to describe"), "{m}");
    }

    #[tokio::test]
    async fn method_not_found_is_unsupported() {
        let p =
            sh(r#"read l; echo '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}}'"#);
        assert!(matches!(p.usage("x").await, Err(Error::Unsupported(m)) if m == "usage"));
    }

    #[tokio::test]
    async fn missing_executable() {
        let p = ProcessPlugin::new("nope", "/no/such/plugin", vec![], vec![]);
        assert!(message(p.describe().await).contains("can't start"));
    }
}

#[tokio::test]
async fn describe_is_cached_until_the_executable_changes() {
    let cache = tempfile::tempdir().unwrap();
    let found = Found {
        name: "fake".into(),
        exec: fake_exe(),
        args: vec![],
        source: Source::Path,
    };
    let first = describe_all(vec![found.clone()], cache.path()).await;
    assert_eq!(first[0].description.as_ref().unwrap().name, "fake");

    // Doctor the cached answer: if it's used, the name changes.
    let file = cache.path().join("fake.json");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("\"name\":\"fake\"", "\"name\":\"cached\""),
    )
    .unwrap();
    let second = describe_all(vec![found], cache.path()).await;
    assert_eq!(second[0].description.as_ref().unwrap().name, "cached");
}
