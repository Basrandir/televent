#[tokio::main]
async fn main() {
    if let Err(e) = televent::run().await {
        eprintln!("Application error: {:?}", e);
        std::process::exit(1);
    }
}
