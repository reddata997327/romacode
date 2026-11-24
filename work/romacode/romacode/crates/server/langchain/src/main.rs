//! Binary entry point — thin wrapper over [`langchain_rust::cli`].

#[tokio::main]
async fn main() {
    if let Err(e) = langchain_rust::cli::run().await {
        eprintln!("\x1b[31merror>\x1b[0m {e}");
        std::process::exit(1);
    }
}
