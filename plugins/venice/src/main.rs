#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    shotline_plugin::serve(venice::from_env()).await
}
