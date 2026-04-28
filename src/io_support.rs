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
    fs::write(path, bytes).map_err(|error| {
        AppError::new(23, format!("failed to write '{}': {error}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("mirza-{name}-{}-{stamp}", std::process::id()));
        path
    }

    #[test]
    fn read_input_bytes_reads_file_contents() {
        let path = unique_path("read-bytes");
        fs::write(&path, b"hello").unwrap();
        let bytes = read_input_bytes(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn read_input_bytes_returns_error_for_missing_file() {
        let path = unique_path("missing");
        let error = read_input_bytes(&path).unwrap_err();
        assert_eq!(error.code(), 26);
    }

    #[test]
    fn create_output_writer_accepts_stdout_when_path_is_none() {
        assert!(create_output_writer(None).is_ok());
    }

    #[test]
    fn create_output_writer_accepts_stdout_marker() {
        assert!(create_output_writer(Some(Path::new("-"))).is_ok());
    }

    #[test]
    fn create_output_writer_writes_to_file_path() {
        let path = unique_path("writer-file");
        let mut writer = create_output_writer(Some(&path)).unwrap();
        writer.write_all(b"out").unwrap();
        drop(writer);
        let written = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(written, b"out");
    }

    #[test]
    fn write_all_to_path_persists_bytes() {
        let path = unique_path("write-all");
        write_all_to_path(&path, b"saved").unwrap();
        let written = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(written, b"saved");
    }
}
