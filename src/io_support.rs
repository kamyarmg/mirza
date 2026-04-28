use crate::error::AppError;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

pub fn read_input_bytes(path: &Path) -> Result<Vec<u8>, AppError> {
    if path.as_os_str() == "-" {
        let mut buffer = Vec::new();
        io::stdin()
            .read_to_end(&mut buffer)
            .map_err(|error| AppError::new(26, format!("failed to read stdin: {error}")))?;
        return Ok(buffer);
    }

    fs::read(path)
        .map_err(|error| AppError::new(26, format!("failed to read '{}': {error}", path.display())))
}

pub fn create_output_writer(path: Option<&Path>) -> Result<Box<dyn Write>, AppError> {
    match path {
        None => Ok(Box::new(io::stdout())),
        Some(path) if path.as_os_str() == "-" => Ok(Box::new(io::stdout())),
        Some(path) => File::create(path)
            .map(|file| Box::new(file) as Box<dyn Write>)
            .map_err(|error| {
                AppError::new(
                    23,
                    format!("failed to create '{}': {error}", path.display()),
                )
            }),
    }
}

pub fn write_all_to_path(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    fs::write(path, bytes)
        .map_err(|error| AppError::new(23, format!("failed to write '{}': {error}", path.display())))
}