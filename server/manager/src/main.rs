use risunest_sync_manager::cli::{self, Entry};

#[tokio::main]
async fn main() {
    cli::main(Entry::Console).await;
}
