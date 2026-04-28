use crate::cli::{Cli, ColorMode, OutputSection, OutputStyle};
use crate::error::AppError;
use crate::io_support::{create_output_writer, read_input_bytes, write_all_to_path};
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{Client, Request, Response};
use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, RANGE, REFERER,
    USER_AGENT,
};
use reqwest::redirect::Policy;
use reqwest::{Method, Proxy, StatusCode, Version};
use serde_json::Value as JsonValue;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};
use url::Url;

struct ResponseSummary {
    body_bytes: u64,
    total_duration: Duration,
}

struct RenderedOutput {
    body: Vec<u8>,
    raw_body: Vec<u8>,
    summary: ResponseSummary,
}

struct DisplayOptions {
    style: OutputStyle,
    sections: DisplaySections,
    use_color: bool,
}

#[derive(Copy, Clone)]
struct DisplaySections {
    meta: bool,
    headers: bool,
    body: bool,
}

pub struct CapturedResponse {
    pub method: String,
    pub url: String,
    pub status: u16,
    pub reason: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub header_block: Vec<u8>,
    pub body: Vec<u8>,
    pub rendered: Vec<u8>,
    pub duration: Duration,
    pub body_bytes: u64,
    pub content_type: Option<String>,
    pub certificate_summary: Option<String>,
}

pub fn run(cli: &Cli) -> Result<i32, AppError> {
    validate_cli(cli)?;

    let url_input = cli
        .url
        .as_deref()
        .ok_or_else(|| AppError::new(2, "missing URL"))?;

    let mut url = parse_url(url_input)?;
    if cli.get
        && let Some(query) = build_query_string(cli)?
    {
        append_query_string(&mut url, &query);
    }

    let method = infer_method(cli)?;
    let resume_offset = resolve_resume_offset(cli)?;
    let headers = build_headers(cli, resume_offset)?;
    let client = build_client(cli)?;
    let started_at = Instant::now();
    let mut response = execute_with_retry(cli, &client, &method, &url, &headers)?;

    if cli.fail && response.status().has_client_or_server_error() {
        return Err(AppError::new(
            22,
            format!("request failed with status {}", response.status()),
        ));
    }

    if resume_offset.unwrap_or(0) > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(AppError::new(
            33,
            format!("server did not honor resume request: {}", response.status()),
        ));
    }

    let header_block = render_response_headers(&response);
    if let Some(path) = &cli.dump_header {
        write_all_to_path(path, &header_block)?;
    }

    let display_options = display_options(cli);
    let should_render = should_render_response(cli, &display_options);

    if should_render {
        let rendered = render_response_output(
            cli,
            &method,
            &url,
            &mut response,
            &header_block,
            started_at.elapsed(),
            &display_options,
        )?;
        let mut output = create_response_writer(cli.output.as_deref(), false)?;
        output.write_all(&rendered.body).map_err(|error| {
            AppError::new(23, format!("failed to write rendered response: {error}"))
        })?;
        output
            .flush()
            .map_err(|error| AppError::new(23, format!("failed to flush output: {error}")))?;
    } else {
        let mut output =
            create_response_writer(cli.output.as_deref(), resume_offset.unwrap_or(0) > 0)?;
        if cli.include {
            output.write_all(&header_block).map_err(|error| {
                AppError::new(23, format!("failed to write response headers: {error}"))
            })?;
        }

        if method != Method::HEAD {
            let limit_rate = parse_rate_limit(cli.limit_rate.as_deref())?;
            copy_response_body(&mut response, &mut output, limit_rate).map_err(|error| {
                AppError::new(23, format!("failed to write response body: {error}"))
            })?;
            output
                .flush()
                .map_err(|error| AppError::new(23, format!("failed to flush output: {error}")))?;
        }
    }

    Ok(0)
}

pub fn execute_capture(cli: &Cli) -> Result<CapturedResponse, AppError> {
    validate_cli(cli)?;

    let url_input = cli
        .url
        .as_deref()
        .ok_or_else(|| AppError::new(2, "missing URL"))?;

    let mut url = parse_url(url_input)?;
    if cli.get
        && let Some(query) = build_query_string(cli)?
    {
        append_query_string(&mut url, &query);
    }

    let method = infer_method(cli)?;
    let resume_offset = resolve_resume_offset(cli)?;
    let headers = build_headers(cli, resume_offset)?;
    let client = build_client(cli)?;
    let started_at = Instant::now();
    let mut response = execute_with_retry(cli, &client, &method, &url, &headers)?;

    if cli.fail && response.status().has_client_or_server_error() {
        return Err(AppError::new(
            22,
            format!("request failed with status {}", response.status()),
        ));
    }

    if resume_offset.unwrap_or(0) > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(AppError::new(
            33,
            format!("server did not honor resume request: {}", response.status()),
        ));
    }

    let header_block = render_response_headers(&response);
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let header_pairs = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect::<Vec<_>>();

    let display_options = display_options(cli);
    let rendered = render_response_output(
        cli,
        &method,
        &url,
        &mut response,
        &header_block,
        started_at.elapsed(),
        &display_options,
    )?;

    Ok(CapturedResponse {
        method: method.as_str().to_owned(),
        url: url.as_str().to_owned(),
        status: response.status().as_u16(),
        reason: response
            .status()
            .canonical_reason()
            .unwrap_or("")
            .to_owned(),
        version: version_string(response.version()).to_owned(),
        headers: header_pairs,
        header_block,
        body: rendered.raw_body,
        rendered: rendered.body,
        duration: rendered.summary.total_duration,
        body_bytes: rendered.summary.body_bytes,
        content_type,
        certificate_summary: if url.scheme().eq_ignore_ascii_case("https") {
            Some(
                "TLS certificate details are unavailable via the current reqwest backend"
                    .to_owned(),
            )
        } else {
            None
        },
    })
}

fn should_render_response(cli: &Cli, options: &DisplayOptions) -> bool {
    if matches!(options.style, OutputStyle::Raw) {
        return false;
    }

    match cli.output.as_deref() {
        Some(path) if path.as_os_str() != "-" => false,
        _ => !cli.silent,
    }
}

fn display_options(cli: &Cli) -> DisplayOptions {
    DisplayOptions {
        style: resolve_output_style(cli),
        sections: resolve_display_sections(cli),
        use_color: resolve_color_mode(cli),
    }
}

fn resolve_output_style(cli: &Cli) -> OutputStyle {
    match cli.output_style {
        OutputStyle::Auto => {
            if is_stdout_terminal() {
                OutputStyle::Pretty
            } else {
                OutputStyle::Raw
            }
        }
        style => style,
    }
}

fn resolve_display_sections(cli: &Cli) -> DisplaySections {
    if cli.show.is_empty() {
        return DisplaySections {
            meta: true,
            headers: cli.include,
            body: true,
        };
    }

    let mut sections = DisplaySections {
        meta: false,
        headers: false,
        body: false,
    };

    for section in &cli.show {
        match section {
            OutputSection::Meta => sections.meta = true,
            OutputSection::Headers => sections.headers = true,
            OutputSection::Body => sections.body = true,
            OutputSection::All => {
                sections.meta = true;
                sections.headers = true;
                sections.body = true;
            }
        }
    }

    sections
}

fn resolve_color_mode(cli: &Cli) -> bool {
    match cli.color {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => is_stdout_terminal(),
    }
}

fn is_stdout_terminal() -> bool {
    io::stdout().is_terminal()
}

fn render_response_output(
    cli: &Cli,
    method: &Method,
    url: &Url,
    response: &mut Response,
    header_block: &[u8],
    total_duration: Duration,
    options: &DisplayOptions,
) -> Result<RenderedOutput, AppError> {
    let limit_rate = parse_rate_limit(cli.limit_rate.as_deref())?;
    let mut body = Vec::new();

    if method != Method::HEAD {
        copy_response_body(response, &mut body, limit_rate).map_err(|error| {
            AppError::new(23, format!("failed to buffer response body: {error}"))
        })?;
    }

    let summary = ResponseSummary {
        body_bytes: body.len() as u64,
        total_duration,
    };
    let rendered = match options.style {
        OutputStyle::Pretty | OutputStyle::Auto => render_pretty_response(
            method,
            url,
            response,
            header_block,
            &body,
            &summary,
            options,
        ),
        OutputStyle::Json => render_json_response(
            method,
            url,
            response,
            header_block,
            &body,
            &summary,
            options,
        ),
        OutputStyle::Compact => render_compact_response(response, &body, &summary, options),
        OutputStyle::Raw => body.clone(),
    };

    Ok(RenderedOutput {
        body: rendered,
        raw_body: body,
        summary,
    })
}

fn render_pretty_response(
    method: &Method,
    url: &Url,
    response: &Response,
    header_block: &[u8],
    body: &[u8],
    summary: &ResponseSummary,
    options: &DisplayOptions,
) -> Vec<u8> {
    let mut rendered = String::new();

    if options.sections.meta {
        rendered.push_str(&format_title("Response", options.use_color));
        rendered.push_str(&format_kv(
            "Status",
            &status_badge(response.status(), options.use_color),
        ));
        rendered.push_str(&format_kv("Method", method.as_str()));
        rendered.push_str(&format_kv("URL", url.as_str()));
        rendered.push_str(&format_kv("Version", version_string(response.version())));
        rendered.push_str(&format_kv("Time", &format_duration(summary.total_duration)));
        rendered.push_str(&format_kv("Bytes", &summary.body_bytes.to_string()));
        if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
            rendered.push_str(&format_kv(
                "Content-Type",
                &String::from_utf8_lossy(content_type.as_bytes()),
            ));
        }
        rendered.push('\n');
    }

    if options.sections.headers {
        rendered.push_str(&format_title("Headers", options.use_color));
        rendered.push_str(&String::from_utf8_lossy(header_block));
        if !rendered.ends_with("\n\n") {
            rendered.push('\n');
        }
    }

    if options.sections.body {
        rendered.push_str(&format_title("Body", options.use_color));
        rendered.push_str(&format_body(body, response, true, true, options.use_color));
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
    }

    rendered.into_bytes()
}

fn render_json_response(
    method: &Method,
    url: &Url,
    response: &Response,
    header_block: &[u8],
    body: &[u8],
    summary: &ResponseSummary,
    options: &DisplayOptions,
) -> Vec<u8> {
    let mut object = serde_json::Map::new();

    if options.sections.meta {
        object.insert(
            "meta".to_owned(),
            serde_json::json!({
                "status": response.status().as_u16(),
                "reason": response.status().canonical_reason(),
                "method": method.as_str(),
                "url": url.as_str(),
                "version": version_string(response.version()),
                "duration_ms": summary.total_duration.as_millis(),
                "body_bytes": summary.body_bytes,
            }),
        );
    }

    if options.sections.headers {
        let mut headers = serde_json::Map::new();
        for line in String::from_utf8_lossy(header_block).lines().skip(1) {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(
                    name.trim().to_owned(),
                    serde_json::Value::String(value.trim().to_owned()),
                );
            }
        }
        object.insert("headers".to_owned(), JsonValue::Object(headers));
    }

    if options.sections.body {
        object.insert("body".to_owned(), json_body_value(body));
    }

    serde_json::to_vec_pretty(&JsonValue::Object(object)).unwrap_or_else(|_| b"{}".to_vec())
}

fn render_compact_response(
    response: &Response,
    body: &[u8],
    summary: &ResponseSummary,
    options: &DisplayOptions,
) -> Vec<u8> {
    let mut rendered = String::new();

    if options.sections.meta {
        rendered.push_str(&format!(
            "{} {} | {} | {} bytes\n",
            status_badge(response.status(), options.use_color),
            response.status().canonical_reason().unwrap_or(""),
            format_duration(summary.total_duration),
            summary.body_bytes,
        ));
    }

    if options.sections.headers {
        for (name, value) in response.headers() {
            rendered.push_str(name.as_str());
            rendered.push_str(": ");
            rendered.push_str(&String::from_utf8_lossy(value.as_bytes()));
            rendered.push('\n');
        }
    }

    if options.sections.body {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(&format_body(
            body,
            response,
            false,
            false,
            options.use_color,
        ));
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
    }

    rendered.into_bytes()
}

fn format_title(title: &str, use_color: bool) -> String {
    let decorated = if use_color {
        paint(title, "1;36")
    } else {
        title.to_owned()
    };
    format!("== {decorated} ==\n")
}

fn format_kv(key: &str, value: &str) -> String {
    format!("{key:>12}: {value}\n")
}

fn status_badge(status: StatusCode, use_color: bool) -> String {
    let plain = format!("{}", status.as_u16());
    if !use_color {
        return plain;
    }

    let color = if status.is_success() {
        "1;32"
    } else if status.is_redirection() {
        "1;34"
    } else if status.is_client_error() {
        "1;33"
    } else {
        "1;31"
    };
    paint(&plain, color)
}

fn paint(text: &str, code: &str) -> String {
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

fn format_duration(duration: Duration) -> String {
    if duration.as_millis() < 1000 {
        return format!("{} ms", duration.as_millis());
    }

    format!("{:.2} s", duration.as_secs_f64())
}

fn format_body(
    body: &[u8],
    response: &Response,
    pretty: bool,
    color_json_keys: bool,
    use_color: bool,
) -> String {
    if body.is_empty() {
        return "<empty>\n".to_owned();
    }

    if pretty
        && is_json_response(response)
        && let Ok(json) = serde_json::from_slice::<JsonValue>(body)
    {
        let rendered = if color_json_keys && use_color {
            format_json_with_colored_keys(&json, 0)
        } else if let Ok(rendered) = serde_json::to_string_pretty(&json) {
            rendered
        } else {
            String::from_utf8_lossy(body).into_owned()
        };
        return format!("{rendered}\n");
    }

    if let Ok(text) = std::str::from_utf8(body) {
        return if pretty {
            format!("{}\n", indent_lines(text.trim_end(), "  "))
        } else {
            text.to_owned()
        };
    }

    format!("<binary body: {} bytes>\n", body.len())
}

fn format_json_with_colored_keys(value: &JsonValue, level: usize) -> String {
    match value {
        JsonValue::Object(map) => {
            if map.is_empty() {
                return "{}".to_owned();
            }

            let indent = "  ".repeat(level);
            let child_indent = "  ".repeat(level + 1);
            let mut lines = Vec::with_capacity(map.len());

            for (key, entry_value) in map {
                lines.push(format!(
                    "{child_indent}{}: {}",
                    paint(&format!("\"{key}\""), "1;33"),
                    format_json_with_colored_keys(entry_value, level + 1)
                ));
            }

            format!("{{\n{}\n{indent}}}", lines.join(",\n"))
        }
        JsonValue::Array(items) => {
            if items.is_empty() {
                return "[]".to_owned();
            }

            let indent = "  ".repeat(level);
            let child_indent = "  ".repeat(level + 1);
            let lines = items
                .iter()
                .map(|item| {
                    format!(
                        "{child_indent}{}",
                        format_json_with_colored_keys(item, level + 1)
                    )
                })
                .collect::<Vec<_>>();

            format!("[\n{}\n{indent}]", lines.join(",\n"))
        }
        JsonValue::String(text) => format!("\"{text}\""),
        JsonValue::Number(number) => number.to_string(),
        JsonValue::Bool(boolean) => boolean.to_string(),
        JsonValue::Null => "null".to_owned(),
    }
}

fn indent_lines(input: &str, prefix: &str) -> String {
    input
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("json"))
}

fn json_body_value(body: &[u8]) -> JsonValue {
    serde_json::from_slice(body)
        .unwrap_or_else(|_| JsonValue::String(String::from_utf8_lossy(body).into_owned()))
}

fn validate_cli(cli: &Cli) -> Result<(), AppError> {
    let mut payload_modes = 0;
    if !cli.form.is_empty() {
        payload_modes += 1;
    }
    if cli.json.is_some() {
        payload_modes += 1;
    }
    if cli.upload_file.is_some() {
        payload_modes += 1;
    }
    if !cli.data.is_empty() || !cli.data_raw.is_empty() || !cli.data_binary.is_empty() {
        payload_modes += 1;
    }

    if payload_modes > 1 {
        return Err(AppError::new(
            2,
            "choose only one payload mode among data, json, form, or upload-file",
        ));
    }

    if cli.continue_at.is_some() && cli.range.is_some() {
        return Err(AppError::new(
            2,
            "--continue-at cannot be used together with --range",
        ));
    }

    if cli.continue_at.is_some() && cli.upload_file.is_some() {
        return Err(AppError::new(
            2,
            "--continue-at is only supported for downloads",
        ));
    }

    if cli.continue_at.is_some() && cli.output.is_none() {
        return Err(AppError::new(
            2,
            "--continue-at requires --output with a file path",
        ));
    }

    if let Some(output) = cli.output.as_deref()
        && output.as_os_str() == "-"
        && cli.continue_at.is_some()
    {
        return Err(AppError::new(
            2,
            "--continue-at requires --output with a file path",
        ));
    }

    Ok(())
}

fn infer_method(cli: &Cli) -> Result<Method, AppError> {
    if let Some(method) = &cli.request {
        return Method::from_bytes(method.as_bytes())
            .map_err(|error| AppError::new(2, format!("invalid HTTP method '{method}': {error}")));
    }

    if cli.head {
        return Ok(Method::HEAD);
    }

    if cli.upload_file.is_some() {
        return Ok(Method::PUT);
    }

    if cli.get {
        return Ok(Method::GET);
    }

    if !cli.form.is_empty()
        || cli.json.is_some()
        || !cli.data.is_empty()
        || !cli.data_raw.is_empty()
        || !cli.data_binary.is_empty()
    {
        return Ok(Method::POST);
    }

    Ok(Method::GET)
}

fn parse_url(input: &str) -> Result<Url, AppError> {
    Url::parse(input)
        .or_else(|_| Url::parse(&format!("http://{input}")))
        .map_err(|error| AppError::new(3, format!("invalid URL '{input}': {error}")))
}

fn append_query_string(url: &mut Url, query: &str) {
    match url.query() {
        Some(existing) if !existing.is_empty() => {
            let merged = format!("{existing}&{query}");
            url.set_query(Some(&merged));
        }
        _ => url.set_query(Some(query)),
    }
}

fn build_client(cli: &Cli) -> Result<Client, AppError> {
    let redirect_policy = if cli.location {
        Policy::limited(10)
    } else {
        Policy::none()
    };

    let mut builder = Client::builder()
        .danger_accept_invalid_certs(cli.insecure)
        .redirect(redirect_policy);

    if let Some(timeout) = cli.connect_timeout {
        builder = builder.connect_timeout(duration_from_secs(timeout)?);
    }
    if let Some(timeout) = cli.max_time {
        builder = builder.timeout(duration_from_secs(timeout)?);
    }
    if let Some(proxy) = &cli.proxy {
        builder = builder.proxy(
            Proxy::all(proxy)
                .map_err(|error| AppError::new(5, format!("invalid proxy '{proxy}': {error}")))?,
        );
    }
    if cli.http1_1 {
        builder = builder.http1_only();
    }

    builder
        .build()
        .map_err(|error| AppError::new(1, format!("failed to create HTTP client: {error}")))
}

fn duration_from_secs(value: f64) -> Result<Duration, AppError> {
    if value.is_sign_negative() || !value.is_finite() {
        return Err(AppError::new(2, format!("invalid timeout value: {value}")));
    }

    Ok(Duration::from_secs_f64(value))
}

fn build_headers(cli: &Cli, resume_offset: Option<u64>) -> Result<HeaderMap, AppError> {
    let mut headers = HeaderMap::new();

    for raw in &cli.headers {
        let (name, value) = raw.split_once(':').ok_or_else(|| {
            AppError::new(2, format!("invalid header '{raw}', expected Name: Value"))
        })?;

        let header_name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|error| AppError::new(2, format!("invalid header name '{name}': {error}")))?;
        let header_value = HeaderValue::from_str(value.trim_start()).map_err(|error| {
            AppError::new(2, format!("invalid header value for '{name}': {error}"))
        })?;

        headers.append(header_name, header_value);
    }

    if let Some(user_agent) = &cli.user_agent {
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(user_agent)
                .map_err(|error| AppError::new(2, format!("invalid user-agent value: {error}")))?,
        );
    }

    if let Some(referer) = &cli.referer {
        headers.insert(
            REFERER,
            HeaderValue::from_str(referer)
                .map_err(|error| AppError::new(2, format!("invalid referer value: {error}")))?,
        );
    }

    if cli.compressed {
        headers.insert(
            ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, br, deflate, zstd"),
        );
    }

    if cli.json.is_some() {
        if !headers.contains_key(CONTENT_TYPE) {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        if !headers.contains_key(ACCEPT) {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }
    }

    if (!cli.data.is_empty() || !cli.data_raw.is_empty() || !cli.data_binary.is_empty())
        && !headers.contains_key(CONTENT_TYPE)
    {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
    }

    if let Some(range) = build_range_header_value(cli, resume_offset) {
        headers.insert(
            RANGE,
            HeaderValue::from_str(&range).map_err(|error| {
                AppError::new(2, format!("invalid range value '{range}': {error}"))
            })?,
        );
    }

    Ok(headers)
}

fn build_range_header_value(cli: &Cli, resume_offset: Option<u64>) -> Option<String> {
    if let Some(offset) = resume_offset.filter(|offset| *offset > 0) {
        return Some(format!("bytes={offset}-"));
    }

    cli.range.clone()
}

fn resolve_resume_offset(cli: &Cli) -> Result<Option<u64>, AppError> {
    let Some(continue_at) = cli.continue_at.as_deref() else {
        return Ok(None);
    };

    let output_path = cli
        .output
        .as_deref()
        .ok_or_else(|| AppError::new(2, "--continue-at requires --output with a file path"))?;

    if continue_at == "-" {
        return match fs::metadata(output_path) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Some(0)),
            Err(error) => Err(AppError::new(
                23,
                format!("failed to inspect '{}': {error}", output_path.display()),
            )),
        };
    }

    continue_at.parse::<u64>().map(Some).map_err(|error| {
        AppError::new(
            2,
            format!("invalid continue offset '{continue_at}': {error}"),
        )
    })
}

fn parse_user_password(input: Option<&str>) -> Option<(String, Option<String>)> {
    let raw = input?;
    let (username, password) = match raw.split_once(':') {
        Some((user, password)) => (user.to_owned(), Some(password.to_owned())),
        None => (raw.to_owned(), None),
    };
    Some((username, password))
}

fn build_request_body(cli: &Cli) -> Result<Option<Vec<u8>>, AppError> {
    let mut segments = Vec::new();

    for item in &cli.data {
        segments.push(read_data_segment(item, true)?);
    }
    for item in &cli.data_raw {
        segments.push(read_data_segment(item, false)?);
    }
    for item in &cli.data_binary {
        segments.push(read_data_segment(item, true)?);
    }

    if segments.is_empty() {
        return Ok(None);
    }

    Ok(Some(join_segments(&segments)))
}

fn build_query_string(cli: &Cli) -> Result<Option<String>, AppError> {
    let body = build_request_body(cli)?;
    Ok(body.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

fn parse_rate_limit(value: Option<&str>) -> Result<Option<u64>, AppError> {
    let Some(raw) = value else {
        return Ok(None);
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::new(2, "invalid --limit-rate value"));
    }

    let digits_len = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits_len == 0 {
        return Err(AppError::new(
            2,
            format!("invalid --limit-rate value '{trimmed}'"),
        ));
    }

    let number = trimmed[..digits_len].parse::<u64>().map_err(|error| {
        AppError::new(
            2,
            format!("invalid --limit-rate value '{trimmed}': {error}"),
        )
    })?;

    let suffix = trimmed[digits_len..].to_ascii_lowercase();
    let multiplier = match suffix.as_str() {
        "" => 1,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        _ => {
            return Err(AppError::new(
                2,
                format!("invalid --limit-rate suffix '{suffix}'"),
            ));
        }
    };

    Ok(Some(number.saturating_mul(multiplier)))
}

fn read_data_segment(value: &str, allow_file_reference: bool) -> Result<Vec<u8>, AppError> {
    if allow_file_reference && let Some(path) = value.strip_prefix('@') {
        return read_input_bytes(Path::new(path));
    }

    Ok(value.as_bytes().to_vec())
}

fn join_segments(segments: &[Vec<u8>]) -> Vec<u8> {
    let mut joined = Vec::new();

    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            joined.extend_from_slice(b"&");
        }
        joined.extend_from_slice(segment);
    }

    joined
}

fn build_form(items: &[String]) -> Result<Form, AppError> {
    let mut form = Form::new();

    for raw in items {
        let (name, value) = raw.split_once('=').ok_or_else(|| {
            AppError::new(
                2,
                format!("invalid form field '{raw}', expected name=value"),
            )
        })?;

        if let Some(path) = value.strip_prefix('@') {
            form = form.part(name.to_owned(), build_file_part(path)?);
        } else if let Some(path) = value.strip_prefix('<') {
            let content =
                String::from_utf8(read_input_bytes(Path::new(path))?).map_err(|error| {
                    AppError::new(
                        26,
                        format!("form file '{path}' is not valid UTF-8: {error}"),
                    )
                })?;
            form = form.text(name.to_owned(), content);
        } else {
            form = form.text(name.to_owned(), value.to_owned());
        }
    }

    Ok(form)
}

fn build_file_part(raw: &str) -> Result<Part, AppError> {
    let (path_text, mime) = match raw.split_once(";type=") {
        Some((path, mime)) => (path, Some(mime)),
        None => (raw, None),
    };

    let path = Path::new(path_text);
    let bytes = read_input_bytes(path)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("upload.bin")
        .to_owned();

    let part = Part::bytes(bytes).file_name(file_name);

    match mime {
        Some(mime) => part
            .mime_str(mime)
            .map_err(|error| AppError::new(2, format!("invalid MIME type '{mime}': {error}"))),
        None => Ok(part),
    }
}

fn build_request(
    cli: &Cli,
    client: &Client,
    method: &Method,
    url: &Url,
    headers: &HeaderMap,
) -> Result<Request, AppError> {
    let mut request_builder = client
        .request(method.clone(), url.clone())
        .headers(headers.clone());

    if cli.http1_1 {
        request_builder = request_builder.version(Version::HTTP_11);
    }
    if cli.http2 {
        request_builder = request_builder.version(Version::HTTP_2);
    }

    if let Some((username, password)) = parse_user_password(cli.user.as_deref()) {
        request_builder = request_builder.basic_auth(username, password);
    }

    if !cli.form.is_empty() {
        request_builder = request_builder.multipart(build_form(&cli.form)?);
    } else if let Some(upload_path) = &cli.upload_file {
        request_builder = request_builder.body(read_input_bytes(upload_path)?);
    } else if let Some(json_body) = &cli.json {
        request_builder = request_builder.body(json_body.clone());
    } else if !cli.get
        && let Some(body) = build_request_body(cli)?
    {
        request_builder = request_builder.body(body);
    }

    request_builder
        .build()
        .map_err(|error| AppError::new(3, format!("failed to build request: {error}")))
}

fn execute_with_retry(
    cli: &Cli,
    client: &Client,
    method: &Method,
    url: &Url,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    let mut remaining_retries = cli.retry;

    loop {
        let request = build_request(cli, client, method, url, headers)?;

        if cli.verbose {
            print_request_trace(&request);
        }

        match client.execute(request) {
            Ok(response) => {
                if cli.verbose {
                    print_response_trace(&response);
                }

                if remaining_retries > 0 && should_retry_status(response.status()) {
                    remaining_retries -= 1;
                    sleep(retry_delay());
                    continue;
                }

                return Ok(response);
            }
            Err(error) => {
                let mapped_error = map_request_error(error);
                if remaining_retries > 0 && should_retry_error(&mapped_error) {
                    remaining_retries -= 1;
                    sleep(retry_delay());
                    continue;
                }

                return Err(mapped_error);
            }
        }
    }
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn should_retry_error(error: &AppError) -> bool {
    matches!(error.code(), 1 | 7 | 28)
}

fn retry_delay() -> Duration {
    #[cfg(test)]
    {
        Duration::from_millis(1)
    }

    #[cfg(not(test))]
    {
        Duration::from_secs(1)
    }
}

fn create_response_writer(path: Option<&Path>, append: bool) -> Result<Box<dyn Write>, AppError> {
    if !append {
        return create_output_writer(path);
    }

    match path {
        Some(path) if path.as_os_str() != "-" => OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map(|file| Box::new(file) as Box<dyn Write>)
            .map_err(|error| {
                AppError::new(
                    23,
                    format!("failed to open '{}' for append: {error}", path.display()),
                )
            }),
        _ => create_output_writer(path),
    }
}

fn copy_response_body<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    limit_rate: Option<u64>,
) -> io::Result<u64> {
    let Some(limit_rate) = limit_rate else {
        return io::copy(reader, writer);
    };

    let started_at = Instant::now();
    let mut transferred = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(transferred);
        }

        writer.write_all(&buffer[..read])?;
        transferred += read as u64;

        let expected_elapsed = Duration::from_secs_f64(transferred as f64 / limit_rate as f64);
        let elapsed = started_at.elapsed();
        if expected_elapsed > elapsed {
            sleep(expected_elapsed - elapsed);
        }
    }
}

fn render_response_headers(response: &reqwest::blocking::Response) -> Vec<u8> {
    let mut rendered = Vec::new();
    let status_line = format!(
        "{} {} {}\r\n",
        version_string(response.version()),
        response.status().as_u16(),
        response.status().canonical_reason().unwrap_or(""),
    );
    rendered.extend_from_slice(status_line.as_bytes());

    for (name, value) in response.headers() {
        rendered.extend_from_slice(name.as_str().as_bytes());
        rendered.extend_from_slice(b": ");
        rendered.extend_from_slice(value.as_bytes());
        rendered.extend_from_slice(b"\r\n");
    }

    rendered.extend_from_slice(b"\r\n");
    rendered
}

fn print_request_trace(request: &Request) {
    eprintln!(
        "> {} {} {}",
        request.method(),
        request.url(),
        version_string(request.version()),
    );
    for (name, value) in request.headers() {
        eprintln!(
            "> {}: {}",
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes()),
        );
    }
    eprintln!(">");
}

fn print_response_trace(response: &reqwest::blocking::Response) {
    eprintln!(
        "< {} {} {}",
        version_string(response.version()),
        response.status().as_u16(),
        response.status().canonical_reason().unwrap_or(""),
    );
    for (name, value) in response.headers() {
        eprintln!(
            "< {}: {}",
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes()),
        );
    }
    eprintln!("<");
}

fn version_string(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/?",
    }
}

fn map_request_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        return AppError::new(28, format!("request timed out: {error}"));
    }
    if error.is_builder() {
        return AppError::new(3, format!("request build error: {error}"));
    }
    if error.is_connect() {
        return AppError::new(7, format!("connection failed: {error}"));
    }
    AppError::new(1, format!("request failed: {error}"))
}

trait StatusCodeExt {
    fn has_client_or_server_error(self) -> bool;
}

impl StatusCodeExt for StatusCode {
    fn has_client_or_server_error(self) -> bool {
        self.is_client_error() || self.is_server_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::TcpListener;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

    fn unique_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("mirza-{name}-{}-{stamp}", std::process::id()));
        path
    }

    fn spawn_server(response: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        (format!("http://{address}"), handle)
    }

    fn spawn_timeout_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(StdDuration::from_millis(200));
        });

        (format!("http://{address}"), handle)
    }

    fn ok_response() -> &'static str {
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Test: yes\r\nConnection: close\r\n\r\nok"
    }

    #[test]
    fn run_returns_error_when_url_is_missing() {
        let cli = Cli::parse_from(["mirza"]);
        assert_eq!(run(&cli).unwrap_err().code(), 2);
    }

    #[test]
    fn run_writes_response_body_to_output_file() {
        let (url, handle) = spawn_server(ok_response());
        let output_path = unique_path("run-output");
        let cli = Cli::parse_from(["mirza", "-o", output_path.to_str().unwrap(), url.as_str()]);
        run(&cli).unwrap();
        handle.join().unwrap();
        let written = fs::read(&output_path).unwrap();
        fs::remove_file(&output_path).unwrap();
        assert_eq!(written, b"ok");
    }

    #[test]
    fn run_retries_after_server_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for response in [
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer);
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });

        let output_path = unique_path("retry-success");
        let cli = Cli::parse_from([
            "mirza",
            "--retry",
            "1",
            "-o",
            output_path.to_str().unwrap(),
            &format!("http://{address}"),
        ]);

        run(&cli).unwrap();
        handle.join().unwrap();
        let written = fs::read(&output_path).unwrap();
        fs::remove_file(&output_path).unwrap();
        assert_eq!(written, b"ok");
    }

    #[test]
    fn run_resumes_download_into_existing_file() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let read = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).to_ascii_lowercase();
            assert!(request.contains("range: bytes=2-"));
            stream
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 2\r\nConnection: close\r\n\r\ncd",
                )
                .unwrap();
            stream.flush().unwrap();
        });

        let output_path = unique_path("resume-output");
        fs::write(&output_path, b"ab").unwrap();
        let cli = Cli::parse_from([
            "mirza",
            "-C",
            "-",
            "-o",
            output_path.to_str().unwrap(),
            &format!("http://{address}"),
        ]);

        run(&cli).unwrap();
        handle.join().unwrap();
        let written = fs::read(&output_path).unwrap();
        fs::remove_file(&output_path).unwrap();
        assert_eq!(written, b"abcd");
    }

    #[test]
    fn validate_cli_rejects_multiple_payload_modes() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "--json", "{}", "https://example.com"]);
        assert_eq!(validate_cli(&cli).unwrap_err().code(), 2);
    }

    #[test]
    fn validate_cli_rejects_range_and_continue_at_together() {
        let cli = Cli::parse_from([
            "mirza",
            "-C",
            "0",
            "-r",
            "bytes=0-10",
            "-o",
            "file.txt",
            "https://example.com",
        ]);
        assert_eq!(validate_cli(&cli).unwrap_err().code(), 2);
    }

    #[test]
    fn infer_method_returns_post_when_data_exists() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "https://example.com"]);
        assert_eq!(infer_method(&cli).unwrap(), Method::POST);
    }

    #[test]
    fn infer_method_returns_put_for_upload() {
        let cli = Cli::parse_from(["mirza", "-T", "payload.txt", "https://example.com"]);
        assert_eq!(infer_method(&cli).unwrap(), Method::PUT);
    }

    #[test]
    fn infer_method_returns_custom_method_when_requested() {
        let cli = Cli::parse_from(["mirza", "-X", "PATCH", "https://example.com"]);
        assert_eq!(infer_method(&cli).unwrap(), Method::PATCH);
    }

    #[test]
    fn infer_method_returns_head_for_head_flag() {
        let cli = Cli::parse_from(["mirza", "-I", "https://example.com"]);
        assert_eq!(infer_method(&cli).unwrap(), Method::HEAD);
    }

    #[test]
    fn infer_method_returns_get_for_get_flag() {
        let cli = Cli::parse_from(["mirza", "-G", "https://example.com"]);
        assert_eq!(infer_method(&cli).unwrap(), Method::GET);
    }

    #[test]
    fn parse_url_adds_http_scheme() {
        let url = parse_url("example.com").unwrap();
        assert_eq!(url.as_str(), "http://example.com/");
    }

    #[test]
    fn parse_url_rejects_invalid_url() {
        assert!(parse_url("://bad url").is_err());
    }

    #[test]
    fn append_query_string_sets_missing_query() {
        let mut url = Url::parse("https://example.com").unwrap();
        append_query_string(&mut url, "a=1");
        assert_eq!(url.query(), Some("a=1"));
    }

    #[test]
    fn append_query_string_appends_existing_query() {
        let mut url = Url::parse("https://example.com?x=1").unwrap();
        append_query_string(&mut url, "a=1");
        assert_eq!(url.query(), Some("x=1&a=1"));
    }

    #[test]
    fn build_client_accepts_default_configuration() {
        let cli = Cli::parse_from(["mirza", "https://example.com"]);
        assert!(build_client(&cli).is_ok());
    }

    #[test]
    fn build_client_rejects_invalid_proxy() {
        let cli = Cli::parse_from(["mirza", "-x", "://bad-proxy", "https://example.com"]);
        assert_eq!(build_client(&cli).unwrap_err().code(), 5);
    }

    #[test]
    fn duration_from_secs_converts_positive_values() {
        assert_eq!(
            duration_from_secs(1.5).unwrap(),
            StdDuration::from_millis(1500)
        );
    }

    #[test]
    fn duration_from_secs_rejects_negative_values() {
        assert_eq!(duration_from_secs(-1.0).unwrap_err().code(), 2);
    }

    #[test]
    fn build_headers_includes_custom_header() {
        let cli = Cli::parse_from(["mirza", "-H", "x-test: 1", "https://example.com"]);
        let headers = build_headers(&cli, None).unwrap();
        assert_eq!(headers.get("x-test").unwrap(), "1");
    }

    #[test]
    fn build_headers_adds_json_content_type() {
        let cli = Cli::parse_from(["mirza", "--json", "{}", "https://example.com"]);
        let headers = build_headers(&cli, None).unwrap();
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "application/json");
    }

    #[test]
    fn build_headers_adds_form_content_type_for_data() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "https://example.com"]);
        let headers = build_headers(&cli, None).unwrap();
        assert_eq!(
            headers.get(CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded"
        );
    }

    #[test]
    fn build_headers_adds_explicit_range_header() {
        let cli = Cli::parse_from(["mirza", "-r", "bytes=5-10", "https://example.com"]);
        let headers = build_headers(&cli, None).unwrap();
        assert_eq!(headers.get(RANGE).unwrap(), "bytes=5-10");
    }

    #[test]
    fn build_headers_adds_resume_range_header() {
        let cli = Cli::parse_from(["mirza", "-o", "file.txt", "https://example.com"]);
        let headers = build_headers(&cli, Some(7)).unwrap();
        assert_eq!(headers.get(RANGE).unwrap(), "bytes=7-");
    }

    #[test]
    fn resolve_resume_offset_uses_explicit_value() {
        let cli = Cli::parse_from(["mirza", "-C", "12", "-o", "file.txt", "https://example.com"]);
        assert_eq!(resolve_resume_offset(&cli).unwrap(), Some(12));
    }

    #[test]
    fn resolve_resume_offset_reads_existing_file_size() {
        let path = unique_path("resume-offset");
        fs::write(&path, b"abcdef").unwrap();
        let cli = Cli::parse_from([
            "mirza",
            "-C",
            "-",
            "-o",
            path.to_str().unwrap(),
            "https://example.com",
        ]);
        let offset = resolve_resume_offset(&cli).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(offset, Some(6));
    }

    #[test]
    fn parse_user_password_splits_password() {
        assert_eq!(
            parse_user_password(Some("kami:secret")),
            Some(("kami".to_string(), Some("secret".to_string())))
        );
    }

    #[test]
    fn parse_user_password_keeps_missing_password_empty() {
        assert_eq!(
            parse_user_password(Some("kami")),
            Some(("kami".to_string(), None))
        );
    }

    #[test]
    fn build_request_body_returns_none_without_data() {
        let cli = Cli::parse_from(["mirza", "https://example.com"]);
        assert_eq!(build_request_body(&cli).unwrap(), None);
    }

    #[test]
    fn build_request_body_joins_segments() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "-d", "b=2", "https://example.com"]);
        assert_eq!(build_request_body(&cli).unwrap(), Some(b"a=1&b=2".to_vec()));
    }

    #[test]
    fn build_query_string_returns_request_body_text() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "https://example.com"]);
        assert_eq!(build_query_string(&cli).unwrap(), Some("a=1".to_string()));
    }

    #[test]
    fn parse_rate_limit_parses_suffixes() {
        assert_eq!(parse_rate_limit(Some("2K")).unwrap(), Some(2048));
    }

    #[test]
    fn parse_rate_limit_rejects_invalid_suffix() {
        assert_eq!(parse_rate_limit(Some("2Q")).unwrap_err().code(), 2);
    }

    #[test]
    fn read_data_segment_returns_literal_bytes() {
        assert_eq!(
            read_data_segment("plain", false).unwrap(),
            b"plain".to_vec()
        );
    }

    #[test]
    fn read_data_segment_reads_file_reference() {
        let path = unique_path("data-segment");
        fs::write(&path, b"file-body").unwrap();
        let token = format!("@{}", path.display());
        let bytes = read_data_segment(&token, true).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(bytes, b"file-body".to_vec());
    }

    #[test]
    fn join_segments_uses_ampersand() {
        let joined = join_segments(&[b"a=1".to_vec(), b"b=2".to_vec()]);
        assert_eq!(joined, b"a=1&b=2");
    }

    #[test]
    fn build_form_accepts_text_field() {
        assert!(build_form(&["name=value".to_string()]).is_ok());
    }

    #[test]
    fn build_form_rejects_field_without_separator() {
        assert_eq!(build_form(&["broken".to_string()]).unwrap_err().code(), 2);
    }

    #[test]
    fn build_file_part_accepts_existing_file() {
        let path = unique_path("part-file");
        fs::write(&path, b"payload").unwrap();
        let part = build_file_part(path.to_str().unwrap());
        fs::remove_file(&path).unwrap();
        assert!(part.is_ok());
    }

    #[test]
    fn build_file_part_rejects_invalid_mime() {
        let path = unique_path("part-mime");
        fs::write(&path, b"payload").unwrap();
        let spec = format!("{};type=bad mime", path.display());
        let error = build_file_part(&spec).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.code(), 2);
    }

    #[test]
    fn copy_response_body_writes_payload_without_rate_limit() {
        let mut reader = Cursor::new(b"payload".to_vec());
        let mut writer = Vec::new();
        let written = copy_response_body(&mut reader, &mut writer, None).unwrap();
        assert_eq!((written, writer), (7, b"payload".to_vec()));
    }

    #[test]
    fn copy_response_body_writes_payload_with_rate_limit() {
        let mut reader = Cursor::new(b"payload".to_vec());
        let mut writer = Vec::new();
        let written = copy_response_body(&mut reader, &mut writer, Some(1024)).unwrap();
        assert_eq!((written, writer), (7, b"payload".to_vec()));
    }

    #[test]
    fn render_response_headers_contains_status_line() {
        let (url, handle) = spawn_server(ok_response());
        let response = reqwest::blocking::get(url).unwrap();
        let rendered = render_response_headers(&response);
        handle.join().unwrap();
        assert!(
            String::from_utf8(rendered)
                .unwrap()
                .starts_with("HTTP/1.1 200 OK\r\n")
        );
    }

    #[test]
    fn render_pretty_response_formats_json_body() {
        let (url, handle) = spawn_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 16\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"name\":\"mirza\"}",
        );
        let response = reqwest::blocking::get(&url).unwrap();
        let header_block = render_response_headers(&response);
        let rendered = render_pretty_response(
            &Method::GET,
            &Url::parse(&url).unwrap(),
            &response,
            &header_block,
            br#"{"name":"mirza"}"#,
            &ResponseSummary {
                body_bytes: 16,
                total_duration: StdDuration::from_millis(42),
            },
            &DisplayOptions {
                style: OutputStyle::Pretty,
                sections: DisplaySections {
                    meta: true,
                    headers: true,
                    body: true,
                },
                use_color: false,
            },
        );
        handle.join().unwrap();
        let rendered = String::from_utf8(rendered).unwrap();
        assert!(rendered.contains("== Response =="));
        assert!(rendered.contains("== Headers =="));
        assert!(rendered.contains("== Body =="));
        assert!(rendered.contains("  \"name\": \"mirza\""));
    }

    #[test]
    fn resolve_display_sections_defaults_to_meta_and_body() {
        let cli = Cli::parse_from(["mirza", "https://example.com"]);
        let sections = resolve_display_sections(&cli);
        assert!(sections.meta);
        assert!(sections.body);
        assert!(!sections.headers);
    }

    #[test]
    fn print_request_trace_does_not_panic() {
        let request = Client::new()
            .request(Method::GET, "https://example.com")
            .build()
            .unwrap();
        assert!(catch_unwind(AssertUnwindSafe(|| print_request_trace(&request))).is_ok());
    }

    #[test]
    fn print_response_trace_does_not_panic() {
        let (url, handle) = spawn_server(ok_response());
        let response = reqwest::blocking::get(url).unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| print_response_trace(&response)));
        handle.join().unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn version_string_returns_http11_name() {
        assert_eq!(version_string(Version::HTTP_11), "HTTP/1.1");
    }

    #[test]
    fn map_request_error_maps_builder_errors_to_code_three() {
        let error = Client::new()
            .request(Method::GET, "http://[::1")
            .build()
            .unwrap_err();
        assert_eq!(map_request_error(error).code(), 3);
    }

    #[test]
    fn map_request_error_maps_connect_errors_to_code_seven() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = Client::new()
            .get(format!("http://{address}"))
            .send()
            .unwrap_err();
        assert_eq!(map_request_error(error).code(), 7);
    }

    #[test]
    fn map_request_error_maps_timeouts_to_code_twenty_eight() {
        let (url, handle) = spawn_timeout_server();
        let error = Client::builder()
            .timeout(StdDuration::from_millis(50))
            .build()
            .unwrap()
            .get(url)
            .send()
            .unwrap_err();
        handle.join().unwrap();
        assert_eq!(map_request_error(error).code(), 28);
    }

    #[test]
    fn status_code_ext_returns_true_for_client_errors() {
        assert!(StatusCode::BAD_REQUEST.has_client_or_server_error());
    }

    #[test]
    fn status_code_ext_returns_false_for_success_status() {
        assert!(!StatusCode::OK.has_client_or_server_error());
    }
}
