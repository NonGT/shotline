#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    shotline_plugin::serve(elevenlabs::from_env()).await
}
