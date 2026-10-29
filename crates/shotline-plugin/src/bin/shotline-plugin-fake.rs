#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    shotline_plugin::serve(shotline_plugin::fake::FakePlugin::default()).await
}
