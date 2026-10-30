use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use local::Local;
use shotline_plugin::Plugin;
use shotline_plugin::protocol::{Request, State};

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn clip(dir: &Path) -> PathBuf {
    let p = dir.join("in.mp4");
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x36:rate=12:duration=1"
            ])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&p)
            .status()
            .unwrap()
            .success()
    );
    p
}

pub fn probe(p: &Path) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args([
            "-show_entries",
            "stream=width,height,r_frame_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().into()
}

pub async fn finish(l: &Local, req: &Request, dir: &Path) -> PathBuf {
    let job = l.run(req, dir).await.unwrap();
    for _ in 0..600 {
        let s = l.poll(&job).await.unwrap();
        match s.state {
            State::Done => {
                l.fetch(&job, dir).await.unwrap();
                return dir.join("out.mp4");
            }
            State::Failed => panic!("{:?}", s.error),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("never finished");
}
