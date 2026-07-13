use std::process::ExitCode;

use clap::Parser;
use locale_forge::{cli::Cli, commands};

#[tokio::main]
async fn main() -> ExitCode {
    match commands::execute(Cli::parse()).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("错误: {error:#}");
            ExitCode::FAILURE
        }
    }
}
