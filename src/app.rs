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
    if cli.get
        && let Some(query) = build_query_string(cli)?
    {
        append_query_string(&mut url, &query);
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
    } else if !cli.get
        && let Some(body) = build_request_body(cli)?
    {
        request_builder = request_builder.body(body);
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

    if cli.fail && response.status().has_client_or_server_error() {
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
    use std::io::{Read, Write};
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
    fn validate_cli_rejects_multiple_payload_modes() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "--json", "{}", "https://example.com"]);
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
        let headers = build_headers(&cli).unwrap();
        assert_eq!(headers.get("x-test").unwrap(), "1");
    }

    #[test]
    fn build_headers_adds_json_content_type() {
        let cli = Cli::parse_from(["mirza", "--json", "{}", "https://example.com"]);
        let headers = build_headers(&cli).unwrap();
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "application/json");
    }

    #[test]
    fn build_headers_adds_form_content_type_for_data() {
        let cli = Cli::parse_from(["mirza", "-d", "a=1", "https://example.com"]);
        let headers = build_headers(&cli).unwrap();
        assert_eq!(
            headers.get(CONTENT_TYPE).unwrap(),
            "application/x-www-form-urlencoded"
        );
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
