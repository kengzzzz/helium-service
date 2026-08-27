#[tokio::main]
async fn main() {
    if let Err(error) = helium_service::drift::check_upstream_compatibility().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
