use anyhow::{Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use std::{sync::OnceLock, time::Duration};

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
    request(url, Duration::from_secs(4))?
        .send()
        .with_context(|| format!("request {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP request failed for {url}"))?
        .json()
        .context("decode HTTP JSON")
}

pub fn get_bytes(url: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let response = request(url, Duration::from_secs(2))?
        .send()
        .with_context(|| format!("request {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP request failed for {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        anyhow::bail!("HTTP response exceeds the {max_bytes}-byte limit");
    }
    let bytes = response.bytes().context("read HTTP response")?;
    if bytes.len() > max_bytes {
        anyhow::bail!("HTTP response exceeds the {max_bytes}-byte limit");
    }
    Ok(bytes.to_vec())
}
