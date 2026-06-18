#[tokio::main]
async fn main() {
    if let Err(error) = matrix::run_enter_cli().await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
