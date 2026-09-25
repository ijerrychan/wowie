use anyhow::{Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use std::{sync::OnceLock, time::Duration};

const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

static CLIENT: OnceLock<Client> = OnceLock::new();

fn client() -> Result<&'static Client> {
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .user_agent("wowie/0.1")
        .build()
        .context("create HTTP client")?;
    let _ = CLIENT.set(client);
    Ok(CLIENT.get().expect("HTTP client initialized"))
}

fn request(url: &str, timeout: Duration) -> Result<RequestBuilder> {
    Ok(client()?.get(url).timeout(timeout))
}

pub fn get_json<T: for<'de> serde::Deserialize<'de>>(url: &str) -> Result<T> {
    send_json(request(url, Duration::from_secs(4))?, url)
}

/// Submit a small URL-encoded form through the shared bounded HTTP client.
/// Script plugins use this for APIs, such as DeepL, that intentionally expose
/// translation only through POST endpoints.
pub fn post_form_json<T: for<'de> serde::Deserialize<'de>>(
    url: &str,
    form: &std::collections::BTreeMap<String, String>,
) -> Result<T> {
    send_json(client()?.post(url).form(form), url)
}

/// Submit a JSON object through the shared bounded HTTP client.
pub fn post_json_with_headers<T: for<'de> serde::Deserialize<'de>>(
    url: &str,
    body: &serde_json::Value,
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<T> {
    let mut request = client()?.post(url).timeout(Duration::from_secs(4));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    send_json(request.json(body), url)
}

fn send_json<T: for<'de> serde::Deserialize<'de>>(request: RequestBuilder, url: &str) -> Result<T> {
    let bytes = send_bytes(request, url, MAX_JSON_BYTES)?;
    serde_json::from_slice(&bytes).context("decode HTTP JSON")
}

fn send_bytes(request: RequestBuilder, url: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let response = request.send().with_context(|| format!("request {url}"))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("HTTP request failed for {url} ({status})");
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        anyhow::bail!("HTTP response exceeds the {max_bytes}-byte limit");
    }
    crate::utils::read_bounded(response, max_bytes).context("read HTTP response")
}

pub fn get_bytes(url: &str, max_bytes: usize) -> Result<Vec<u8>> {
    send_bytes(request(url, Duration::from_secs(2))?, url, max_bytes)
}

/// Fetch a bounded UTF-8 response through the shared HTTP client.
pub fn get_text(url: &str, max_bytes: usize) -> Result<String> {
    let bytes = get_bytes(url, max_bytes)?;
    String::from_utf8(bytes).context("decode HTTP response as UTF-8")
}
