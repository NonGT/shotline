#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    shotline_plugin::serve(local::Local::default()).await
}
