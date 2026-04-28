mod app;
mod cli;
mod error;
mod io_support;

use clap::Parser;
use cli::Cli;
use error::AppError;
use std::process::exit;

fn main() {
    let cli = Cli::parse();
    let should_print_errors = !cli.silent || cli.show_error;

    exit(exit_code_for(app::run(&cli), should_print_errors));
}

fn exit_code_for(result: Result<i32, AppError>, should_print_errors: bool) -> i32 {
    match result {
        Ok(code) => code,
        Err(error) => {
            if should_print_errors {
                eprintln!("mirza: {}", error.message());
            }
            error.code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_for_success_returns_code() {
        assert_eq!(exit_code_for(Ok(7), false), 7);
    }

    #[test]
    fn exit_code_for_error_returns_error_code() {
        assert_eq!(exit_code_for(Err(AppError::new(9, "boom")), false), 9);
    }
}
