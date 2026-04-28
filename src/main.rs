mod app;
mod cli;
mod error;
mod io_support;

use clap::Parser;
use cli::Cli;
use std::process::exit;

fn main() {
    let cli = Cli::parse();
    let should_print_errors = !cli.silent || cli.show_error;

    match app::run(&cli) {
        Ok(code) => exit(code),
        Err(error) => {
            if should_print_errors {
                eprintln!("mirza: {}", error.message());
            }
            exit(error.code());
        }
    }
}
