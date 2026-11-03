use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;
use windows::core::{RuntimeType, HSTRING};
use windows::Storage::{
    Search::{FolderDepth, IndexerOption, QueryOptions},
    StorageFolder,
};
use windows_future::{AsyncStatus, IAsyncOperation};
use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};

use super::com::WinRtMtaScope;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const MAX_INDEX_RESULTS: usize = 256;
const INDEX_QUERY_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Query the Windows Search index for file-name matches across indexed local
/// volumes. Unindexed files are deliberately omitted instead of enumerating
/// the filesystem.
pub(crate) fn query_file_index(
    query: &str,
    offset: usize,
    limit: usize,
    abort_reason: &dyn Fn() -> Option<&'static str>,
) -> Result<Vec<(String, String)>> {
    let query = query.trim();
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    if offset >= MAX_INDEX_RESULTS {
        return Ok(Vec::new());
    }

    if let Some(reason) = abort_reason() {
        anyhow::bail!("{reason}");
    }
    let _winrt = WinRtMtaScope::initialize().context("initialize Windows Runtime")?;
    let query_options = QueryOptions::new().context("create Windows Search query")?;
    query_options
        .SetFolderDepth(FolderDepth::Deep)
        .context("enable recursive Windows Search query")?;
    query_options
        .SetIndexerOption(IndexerOption::OnlyUseIndexer)
        .context("require the Windows Search index")?;

    // Double quotes are escaped by doubling them in Advanced Query Syntax.
    let escaped_query = query.replace('"', "\"\"");
    let filter = HSTRING::from(format!("System.FileName:~=\"{escaped_query}\""));
    query_options
        .SetUserSearchFilter(&filter)
        .context("set Windows Search file-name filter")?;

    let requested = offset.saturating_add(limit).min(MAX_INDEX_RESULTS) as u32;
    let mut rows = Vec::new();
    let mut successful_volumes = 0;
    let mut volume_errors = Vec::new();
    let mut folder_operations = Vec::new();

    for root in indexed_volume_roots() {
        match StorageFolder::GetFolderFromPathAsync(&HSTRING::from(&root)) {
            Ok(operation) => folder_operations.push((root, operation)),
            Err(error) => {
                volume_errors.push(format!("{root}: {error}"));
            }
        }
    }

    let folder_results = collect_async_results(folder_operations, "folder lookup", abort_reason);
    if let Some(reason) = abort_reason() {
        anyhow::bail!("{reason}");
    }
    let mut file_operations = Vec::with_capacity(folder_results.len());
    for (root, folder_result) in folder_results {
        let folder = match folder_result {
            Ok(folder) => folder,
            Err(error) => {
                volume_errors.push(format!("{root}: {error:#}"));
                continue;
            }
        };
        let operation = folder
            .CreateFileQueryWithOptions(&query_options)
            .with_context(|| format!("create indexed file query for {root}"))
            .and_then(|query| {
                query
                    .GetFilesAsync(0, requested)
                    .with_context(|| format!("start indexed file query for {root}"))
            });
        match operation {
            Ok(operation) => file_operations.push((root, operation)),
            Err(error) => volume_errors.push(format!("{root}: {error:#}")),
        }
    }

    let file_results = collect_async_results(file_operations, "file query", abort_reason);
    if let Some(reason) = abort_reason() {
        anyhow::bail!("{reason}");
    }
    for (root, files_result) in file_results {
        let files = match files_result {
            Ok(files) => files,
            Err(error) => {
                volume_errors.push(format!("{root}: {error:#}"));
                continue;
            }
        };
        successful_volumes += 1;
        for file in &files {
            let Ok(path) = file.Path() else {
                continue;
            };
            let path = path.to_string();
            // These are filesystem names and paths, so preserve their Unicode
            // text instead of applying the UI language to user data.
            let name = PathBuf::from(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            rows.push((name, path));
        }
    }

    if successful_volumes == 0 && !volume_errors.is_empty() {
        anyhow::bail!(
            "Windows Search could not query any local volume: {}",
            volume_errors.join("; ")
        );
    }

    rows.sort_by_cached_key(|row| (row.1.to_lowercase(), row.1.clone()));
    rows.dedup_by(|left, right| left.1.eq_ignore_ascii_case(&right.1));
    Ok(rows.into_iter().skip(offset).take(limit).collect())
}

fn collect_async_results<T: RuntimeType + 'static>(
    mut pending: Vec<(String, IAsyncOperation<T>)>,
    operation_name: &str,
    abort_reason: &dyn Fn() -> Option<&'static str>,
) -> Vec<(String, Result<T>)> {
    let mut completed = Vec::with_capacity(pending.len());
    while !pending.is_empty() && abort_reason().is_none() {
        let mut index = 0;
        while index < pending.len() && abort_reason().is_none() {
            match pending[index].1.Status() {
                Ok(AsyncStatus::Started) => index += 1,
                Ok(_) => {
                    let (root, operation) = pending.swap_remove(index);
                    let result = operation.GetResults().with_context(|| {
                        format!("complete Windows Search {operation_name} for {root}")
                    });
                    completed.push((root, result));
                }
                Err(error) => {
                    let (root, operation) = pending.swap_remove(index);
                    let _ = operation.Cancel();
                    completed.push((
                        root.clone(),
                        Err(error).with_context(|| {
                            format!("check Windows Search {operation_name} for {root}")
                        }),
                    ));
                }
            }
        }
        if !pending.is_empty() && abort_reason().is_none() {
            std::thread::sleep(INDEX_QUERY_POLL_INTERVAL);
        }
    }

    for (_, operation) in pending {
        let _ = operation.Cancel();
    }
    completed
}

fn indexed_volume_roots() -> Vec<String> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|index| mask & (1 << index) != 0)
        .filter_map(|index| {
            let letter = char::from(b'A' + index as u8);
            let root = format!("{letter}:\\");
            let wide_root = root
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
            let drive_type = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
            let is_local_volume =
                drive_type == DRIVE_FIXED || (drive_type == DRIVE_REMOVABLE && index > 1);
            is_local_volume.then_some(root)
        })
        .collect()
}
