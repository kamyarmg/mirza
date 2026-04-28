use clap::{ArgAction, Parser, ValueEnum, ValueHint};
use std::path::PathBuf;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputStyle {
    Auto,
    Raw,
    Pretty,
    Json,
    Compact,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputSection {
    Meta,
    Headers,
    Body,
    All,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Parser, Debug)]
#[command(name = "mirza", version, about = "Native Rust curl-like HTTP client")]
pub struct Cli {
    #[arg(value_name = "URL")]
    pub url: Option<String>,

    #[arg(long = "interactive", visible_alias = "inteactive", action = ArgAction::SetTrue)]
    pub interactive: bool,

    #[arg(short = 'X', long = "request")]
    pub request: Option<String>,

    #[arg(short = 'I', long = "head", action = ArgAction::SetTrue)]
    pub head: bool,

    #[arg(short = 'i', long = "include", action = ArgAction::SetTrue)]
    pub include: bool,

    #[arg(short = 'L', long = "location", action = ArgAction::SetTrue)]
    pub location: bool,

    #[arg(short = 'k', long = "insecure", action = ArgAction::SetTrue)]
    pub insecure: bool,

    #[arg(short = 'v', long = "verbose", action = ArgAction::SetTrue)]
    pub verbose: bool,

    #[arg(short = 's', long = "silent", action = ArgAction::SetTrue)]
    pub silent: bool,

    #[arg(short = 'S', long = "show-error", action = ArgAction::SetTrue)]
    pub show_error: bool,

    #[arg(long = "fail", action = ArgAction::SetTrue)]
    pub fail: bool,

    #[arg(long = "compressed", action = ArgAction::SetTrue)]
    pub compressed: bool,

    #[arg(short = 'G', long = "get", action = ArgAction::SetTrue)]
    pub get: bool,

    #[arg(short = 'H', long = "header", action = ArgAction::Append)]
    pub headers: Vec<String>,

    #[arg(short = 'd', long = "data", action = ArgAction::Append)]
    pub data: Vec<String>,

    #[arg(long = "data-raw", action = ArgAction::Append)]
    pub data_raw: Vec<String>,

    #[arg(long = "data-binary", action = ArgAction::Append)]
    pub data_binary: Vec<String>,

    #[arg(short = 'F', long = "form", action = ArgAction::Append)]
    pub form: Vec<String>,

    #[arg(long = "json")]
    pub json: Option<String>,

    #[arg(short = 'T', long = "upload-file", value_hint = ValueHint::FilePath)]
    pub upload_file: Option<PathBuf>,

    #[arg(short = 'u', long = "user")]
    pub user: Option<String>,

    #[arg(short = 'A', long = "user-agent")]
    pub user_agent: Option<String>,

    #[arg(short = 'e', long = "referer")]
    pub referer: Option<String>,

    #[arg(short = 'x', long = "proxy")]
    pub proxy: Option<String>,

    #[arg(long = "connect-timeout")]
    pub connect_timeout: Option<f64>,

    #[arg(short = 'm', long = "max-time")]
    pub max_time: Option<f64>,

    #[arg(long = "retry", default_value_t = 0)]
    pub retry: u32,

    #[arg(short = 'C', long = "continue-at")]
    pub continue_at: Option<String>,

    #[arg(short = 'r', long = "range")]
    pub range: Option<String>,

    #[arg(long = "limit-rate")]
    pub limit_rate: Option<String>,

    #[arg(long = "output-style", value_enum, default_value_t = OutputStyle::Auto)]
    pub output_style: OutputStyle,

    #[arg(long = "show", value_enum, value_delimiter = ',', action = ArgAction::Append)]
    pub show: Vec<OutputSection>,

    #[arg(long = "color", value_enum, default_value_t = ColorMode::Auto)]
    pub color: ColorMode,

    #[arg(short = 'o', long = "output", value_hint = ValueHint::FilePath)]
    pub output: Option<PathBuf>,

    #[arg(short = 'D', long = "dump-header", value_hint = ValueHint::FilePath)]
    pub dump_header: Option<PathBuf>,

    #[arg(long = "http1.1", conflicts_with = "http2")]
    pub http1_1: bool,

    #[arg(long = "http2", conflicts_with = "http1_1")]
    pub http2: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sets_url() {
        let cli = Cli::parse_from(["mirza", "https://example.com"]);
        assert_eq!(cli.url.as_deref(), Some("https://example.com"));
        assert!(!cli.interactive);
    }

    #[test]
    fn parse_sets_interactive_mode() {
        let cli = Cli::parse_from(["mirza", "--interactive"]);
        assert!(cli.interactive);
    }

    #[test]
    fn parse_collects_headers() {
        let cli = Cli::parse_from([
            "mirza",
            "-H",
            "x-one: 1",
            "-H",
            "x-two: 2",
            "https://example.com",
        ]);
        assert_eq!(cli.headers, vec!["x-one: 1", "x-two: 2"]);
    }

    #[test]
    fn parse_rejects_conflicting_http_flags() {
        assert!(
            Cli::try_parse_from(["mirza", "--http1.1", "--http2", "https://example.com",]).is_err()
        );
    }

    #[test]
    fn parse_sets_retry_count() {
        let cli = Cli::parse_from(["mirza", "--retry", "2", "https://example.com"]);
        assert_eq!(cli.retry, 2);
    }

    #[test]
    fn parse_sets_continue_at() {
        let cli = Cli::parse_from(["mirza", "-C", "-", "-o", "out.bin", "https://example.com"]);
        assert_eq!(cli.continue_at.as_deref(), Some("-"));
    }

    #[test]
    fn parse_sets_output_style_and_color() {
        let cli = Cli::parse_from([
            "mirza",
            "--output-style",
            "pretty",
            "--color",
            "always",
            "https://example.com",
        ]);
        assert_eq!(cli.output_style, OutputStyle::Pretty);
        assert_eq!(cli.color, ColorMode::Always);
    }

    #[test]
    fn parse_collects_output_sections() {
        let cli = Cli::parse_from([
            "mirza",
            "--show",
            "meta,headers",
            "--show",
            "body",
            "https://example.com",
        ]);
        assert_eq!(
            cli.show,
            vec![
                OutputSection::Meta,
                OutputSection::Headers,
                OutputSection::Body
            ]
        );
    }
}
