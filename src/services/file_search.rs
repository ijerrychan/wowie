//! Shared file-search backends used by built-in providers and legacy Lua APIs.
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        RwLock,
    },
    time::{Duration, Instant},
};

const EVERYTHING_QUERY_BUDGET: Duration = Duration::from_secs(3);
const CANCELLATION_POLL: Duration = Duration::from_millis(10);
const MAX_FILE_SEARCH_RESULTS: usize = 256;

pub(crate) struct FileSearchRuntimeConfig {
    backend: AtomicU8,
    enabled: AtomicBool,
    keyword: RwLock<String>,
}

impl Default for FileSearchRuntimeConfig {
    fn default() -> Self {
        Self {
            backend: AtomicU8::new(0),
            enabled: AtomicBool::new(true),
            keyword: RwLock::new("e".into()),
        }
    }
}

impl FileSearchRuntimeConfig {
    pub(crate) fn replace(&self, backend: &str, enabled: bool, keyword: &str) {
        let backend = match backend {
            "everything" => 1,
            "windows-index" => 2,
            _ => 0,
        };
        self.backend.store(backend, Ordering::Release);
        self.enabled.store(enabled, Ordering::Release);
        if let Ok(mut current) = self.keyword.write() {
            *current = normalize_keyword(keyword);
        }
    }

    pub(crate) fn backend(&self) -> &'static str {
        match self.backend.load(Ordering::Acquire) {
            1 => "everything",
            2 => "windows-index",
            _ => "auto",
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(crate) fn keyword(&self) -> String {
        self.keyword
            .read()
            .map(|keyword| keyword.clone())
            .unwrap_or_else(|_| "e".into())
    }
}

pub(crate) fn normalize_keyword(keyword: &str) -> String {
    let keyword = keyword.trim();
    if keyword.is_empty() || keyword.chars().any(char::is_whitespace) {
        "e".into()
    } else {
        keyword.to_owned()
    }
}

pub(crate) fn query_everything(
    query: &str,
    offset: usize,
    limit: usize,
    abort_reason: &dyn Fn() -> Option<&'static str>,
) -> Result<Vec<(String, String)>> {
    #[cfg(windows)]
    {
        use everything_ipc::wm::{EverythingClient, RequestFlags};

        if let Some(reason) = abort_reason() {
            anyhow::bail!(reason);
        }
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let client = EverythingClient::new().context("Everything IPC is unavailable")?;
        let receiver = client
            .query(query)
            .request_flags(RequestFlags::FileName | RequestFlags::Path)
            .offset(offset.min(u32::MAX as usize) as u32)
            .max_results(limit.min(MAX_FILE_SEARCH_RESULTS) as u32)
            .call()
            .context("Everything IPC query failed")?;
        let deadline = Instant::now() + EVERYTHING_QUERY_BUDGET;
        let list = loop {
            if let Some(reason) = abort_reason() {
                anyhow::bail!(reason);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!("Everything IPC query timed out");
            }
            match receiver.recv_timeout(remaining.min(CANCELLATION_POLL)) {
                Ok(results) => break results,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Everything IPC query was cancelled")
                }
            }
        };
        Ok(list
            .iter()
            .filter_map(|item| {
                let name = item.get_string(RequestFlags::FileName)?;
                let path = item.get_string(RequestFlags::Path)?;
                let full = PathBuf::from(path).join(&name);
                Some((name, full.to_string_lossy().into_owned()))
            })
            .collect())
    }
    #[cfg(not(windows))]
    {
        let _ = (query, offset, limit, abort_reason);
        Ok(Vec::new())
    }
}

pub(crate) fn query_file_search(
    query: &str,
    offset: usize,
    limit: usize,
    backend: &str,
    abort_reason: &dyn Fn() -> Option<&'static str>,
) -> Result<Vec<(String, String)>> {
    query_file_search_with_windows_hook(query, offset, limit, backend, abort_reason, &|| {})
}

pub(crate) fn query_file_search_with_windows_hook(
    query: &str,
    offset: usize,
    limit: usize,
    backend: &str,
    abort_reason: &dyn Fn() -> Option<&'static str>,
    before_windows_search: &dyn Fn(),
) -> Result<Vec<(String, String)>> {
    if let Some(reason) = abort_reason() {
        anyhow::bail!(reason);
    }
    #[cfg(windows)]
    {
        let limit = limit.min(MAX_FILE_SEARCH_RESULTS);
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        match backend {
            "everything" if !everything_ipc_available() => Ok(Vec::new()),
            "everything" => query_everything(query, offset, limit, abort_reason),
            "windows-index" => {
                before_windows_search();
                crate::platform::query_file_index(query, offset, limit, abort_reason)
            }
            _ => match query_everything(query, offset, limit, abort_reason) {
                Ok(rows) => Ok(rows),
                Err(error) if abort_reason().is_some() => Err(error),
                Err(_) => {
                    before_windows_search();
                    crate::platform::query_file_index(query, offset, limit, abort_reason)
                }
            },
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (
            query,
            offset,
            limit,
            backend,
            abort_reason,
            before_windows_search,
        );
        Ok(Vec::new())
    }
}

pub(crate) fn file_search_backend(backend: &str) -> &'static str {
    #[cfg(windows)]
    {
        let everything_available = everything_ipc_available();
        match backend {
            "windows-index" => "windows-index",
            "everything" if everything_available => "everything",
            "everything" => "everything-unavailable",
            _ if everything_available => "everything",
            _ => "windows-index",
        }
    }
    #[cfg(not(windows))]
    {
        let _ = backend;
        "unsupported"
    }
}

#[cfg(windows)]
fn everything_ipc_available() -> bool {
    everything_ipc::wm::EverythingClient::new().is_ok()
}

pub(crate) fn everything_icon_target() -> Option<String> {
    #[cfg(windows)]
    {
        static TARGET: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        TARGET
            .get_or_init(|| {
                find_everything_executable().map(|path| path.to_string_lossy().into_owned())
            })
            .clone()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn find_everything_executable() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates
            .extend(std::env::split_paths(&path).map(|directory| directory.join("Everything.exe")));
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = std::env::var_os(variable) {
            let root = PathBuf::from(root);
            candidates.push(root.join("Everything").join("Everything.exe"));
            if let Ok(entries) = std::fs::read_dir(&root) {
                candidates.extend(entries.flatten().filter_map(|entry| {
                    let name = entry.file_name();
                    name.to_string_lossy()
                        .to_ascii_lowercase()
                        .starts_with("everything")
                        .then(|| entry.path().join("Everything.exe"))
                }));
            }
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a running Everything instance"]
    fn everything_native_query_accepts_search_syntax() {
        let rows = query_everything("name:Cargo.toml", 0, 1, &|| None).unwrap();
        assert!(rows.len() <= 1);
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[test]
    fn cancelled_file_search_returns_before_calling_a_backend() {
        let result = query_file_search_with_windows_hook(
            "native-query",
            0,
            5,
            "everything",
            &|| Some("cancelled"),
            &|| panic!("cancelled query must not start Windows Search"),
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }
}
