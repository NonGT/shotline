//! Needs ffmpeg for the real runs; they pass without doing anything when it isn't installed.

mod common;

use std::path::PathBuf;

use common::{clip, finish, have_ffmpeg, probe};
use local::Local;
use serde_json::json;
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::protocol::{ErrorKind, Op, Request};
use shotline_plugin::{Error, Plugin};

fn request(model: &str, op: Op, video: PathBuf) -> Request {
    let mut r = Request {
        op,
        model: model.into(),
        ..Request::default()
    };
    r.inputs.insert("video".into(), video);
    r
}

#[tokio::test]
async fn conforms() {
    let dir = tempfile::tempdir().unwrap();
    let checks = conformance::check(&Local::default(), &Options::offline(dir.path())).await;
    let failed: Vec<_> = checks.iter().filter(|c| c.problem.is_some()).collect();
    assert!(failed.is_empty(), "{failed:#?}");
}

#[tokio::test]
async fn free_and_checked() {
    let dir = tempfile::tempdir().unwrap();
    let l = Local::default();
    let video = dir.path().join("v.mp4");
    std::fs::write(&video, b"x").unwrap();
    let q = l
        .quote(&request(local::LANCZOS, Op::VideoUpscale, video.clone()))
        .await
        .unwrap();
    assert_eq!((q.amount, q.unit.as_str()), (0.0, "USD"));
    match l
        .quote(&request(local::LANCZOS, Op::VideoInterpolate, video))
        .await
    {
        Err(Error::Plugin(e)) => assert_eq!(e.field.as_deref(), Some("op")),
        other => panic!("{other:?}"),
    }
    match l
        .quote(&request(
            local::LANCZOS,
            Op::VideoUpscale,
            dir.path().join("gone.mp4"),
        ))
        .await
    {
        Err(Error::Plugin(e)) => assert_eq!(e.kind, ErrorKind::Validation),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn interpolates_and_upscales() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let l = Local::default();
    let video = clip(dir.path());

    let out = finish(
        &l,
        &request(local::MINTERPOLATE, Op::VideoInterpolate, video.clone()),
        &dir.path().join("a"),
    )
    .await;
    assert_eq!(probe(&out), "64,36,24/1");

    let mut req = request(local::LANCZOS, Op::VideoUpscale, video.clone());
    req.params.insert("scale".into(), json!(3));
    let out = finish(&l, &req, &dir.path().join("b")).await;
    assert_eq!(probe(&out), "192,108,12/1");
}
