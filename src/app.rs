use crate::cli::Cli;
use crate::error::AppError;
use crate::io_support::{create_output_writer, read_input_bytes, write_all_to_path};
use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{Client, Request};
use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, REFERER, USER_AGENT,
};
use reqwest::redirect::Policy;
use reqwest::{Method, Proxy, StatusCode, Version};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;
use url::Url;

pub fn run(cli: &Cli) -> Result<i32, AppError> {
    validate_cli(cli)?;

    let url_input = cli
        .url
        .as_deref()
        .ok_or_else(|| AppError::new(2, "missing URL"))?;

    let mut url = parse_url(url_input)?;
    if cli.get {
        if let Some(query) = build_query_string(cli)? {
            append_query_string(&mut url, &query);
        }
    }

    let method = infer_method(cli)?;
    let headers = build_headers(cli)?;
    let client = build_client(cli)?;
    let mut request_builder = client.request(method.clone(), url).headers(headers);

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
    } else if !cli.get {
        if let Some(body) = build_request_body(cli)? {
            request_builder = request_builder.body(body);
        }
    }

    let request = request_builder
        .build()
        .map_err(|error| AppError::new(3, format!("failed to build request: {error}")))?;

    if cli.verbose {
        print_request_trace(&request);
    }

    let mut response = client.execute(request).map_err(map_request_error)?;

    if cli.verbose {
        print_response_trace(&response);
    }

    if cli.fail && response.status().is_client_error_or_server_error() {
        return Err(AppError::new(
            22,
            format!("request failed with status {}", response.status()),
        ));
    }

    let header_block = render_response_headers(&response);
    if let Some(path) = &cli.dump_header {
        write_all_to_path(path, &header_block)?;
    }

    let mut output = create_output_writer(cli.output.as_deref())?;
    if cli.include {
        output.write_all(&header_block).map_err(|error| {
            AppError::new(23, format!("failed to write response headers: {error}"))
        })?;
    }

    if method != Method::HEAD {
        io::copy(&mut response, &mut output).map_err(|error| {
            AppError::new(23, format!("failed to write response body: {error}"))
        })?;
        output
            .flush()
            .map_err(|error| AppError::new(23, format!("failed to flush output: {error}")))?;
    }

    Ok(0)
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

fn build_headers(cli: &Cli) -> Result<HeaderMap, AppError> {
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

    Ok(headers)
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

fn read_data_segment(value: &str, allow_file_reference: bool) -> Result<Vec<u8>, AppError> {
    if allow_file_reference {
        if let Some(path) = value.strip_prefix('@') {
            return read_input_bytes(Path::new(path));
        }
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

fn render_response_headers(response: &reqwest::blocking::Response) -> Vec<u8> {
    let mut rendered = Vec::new();
    let status_line = format!(
        "{} {} {}\r\n",
        version_string(response.version()),
        response.status().as_u16(),
        response.status().canonical_reason().unwrap_or("")
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
        version_string(request.version())
    );
    for (name, value) in request.headers() {
        eprintln!(
            "> {}: {}",
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes())
        );
    }
    eprintln!(">");
}

fn print_response_trace(response: &reqwest::blocking::Response) {
    eprintln!(
        "< {} {} {}",
        version_string(response.version()),
        response.status().as_u16(),
        response.status().canonical_reason().unwrap_or("")
    );
    for (name, value) in response.headers() {
        eprintln!(
            "< {}: {}",
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes())
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
    fn is_client_error_or_server_error(self) -> bool;
}

impl StatusCodeExt for StatusCode {
    fn is_client_error_or_server_error(self) -> bool {
        self.is_client_error() || self.is_server_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn infer_post_when_data_exists() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "https://example.com"]);
        assert_eq!(infer_method(&cli).unwrap(), Method::POST);
    }

    #[test]
    fn infer_put_for_upload() {
        let cli = Cli::parse_from(["mirza", "-T", "payload.txt", "https://example.com"]);
        assert_eq!(infer_method(&cli).unwrap(), Method::PUT);
    }

    #[test]
    fn parse_url_adds_http_scheme() {
        let url = parse_url("example.com").unwrap();
        assert_eq!(url.as_str(), "http://example.com/");
    }

    #[test]
    fn join_segments_uses_ampersand() {
        let joined = join_segments(&[b"a=1".to_vec(), b"b=2".to_vec()]);
        assert_eq!(joined, b"a=1&b=2");
    }
}
