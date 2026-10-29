#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    shotline_plugin::serve(wavespeed::from_env()).await
}
