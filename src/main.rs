mod args;
mod auth;
mod server;

pub type BoxResult<T> = Result<T, Box<dyn std::error::Error>>;

use crate::args::{matches, Args};
use crate::server::serve;

#[tokio::main]
async fn main() {
    run().await.unwrap_or_else(handle_err)
}

async fn run() -> BoxResult<()> {
    let args = Args::parse(matches())?;
    serve(args).await
}

fn handle_err<T>(err: Box<dyn std::error::Error>) -> T {
    eprintln!("error: {}", err);
    std::process::exit(1);
}
