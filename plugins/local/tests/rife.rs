//! RIFE through a stand-in `rife-ncnn-vulkan` that writes every frame twice. Its own test binary
//! because it changes PATH.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use local::Local;
use shotline_plugin::Plugin;
use shotline_plugin::protocol::{Op, Request};

mod common;

const STUB: &str = r#"#!/bin/sh
# rife-ncnn-vulkan -i in -o out [-m model]
in="$2"; out="$4"; n=1
for f in "$in"/*.png; do
  for _ in 1 2; do cp "$f" "$out/$(printf %08d $n).png"; n=$((n+1)); done
done
"#;

#[tokio::test]
async fn doubles_the_frame_rate() {
    if !common::have_ffmpeg() {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("rife-ncnn-vulkan");
    std::fs::write(&stub, STUB).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("PATH", path) };

    let l = Local::default();
    let d = l.describe().await.unwrap();
    assert!(d.models.iter().any(|m| m.id == local::RIFE));
    let mut req = Request {
        op: Op::VideoInterpolate,
        model: local::RIFE.into(),
        ..Request::default()
    };
    req.inputs.insert("video".into(), common::clip(dir.path()));
    let out = common::finish(&l, &req, &dir.path().join("job")).await;
    assert_eq!(common::probe(&out), "64,36,24/1");
    assert!(!dir.path().join("job/rife").exists());
}
