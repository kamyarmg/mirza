#[derive(Debug)]
pub struct AppError {
    code: i32,
    message: String,
}

impl AppError {
    pub(crate) fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> i32 {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_returns_constructor_code() {
        let error = AppError::new(12, "boom");
        assert_eq!(error.code(), 12);
    }

    #[test]
    fn message_returns_constructor_message() {
        let error = AppError::new(12, "boom");
        assert_eq!(error.message(), "boom");
    }
}
