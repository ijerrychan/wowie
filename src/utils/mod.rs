pub(crate) mod background;
pub(crate) mod clipboard;
pub(crate) mod http;
pub(crate) mod i18n;

pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing parent"))?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing filename"))?;
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let from = from
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let to = to
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Read at most one byte past the limit, including streams of unknown length.
pub(crate) fn read_bounded(
    reader: impl std::io::Read,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "input exceeds byte limit",
        ));
    }
    Ok(bytes)
}

/// Shared metadata fingerprint for index restoration and directory monitoring.
/// Never follow directory links: junction cycles must not recurse indefinitely.
pub(crate) fn filesystem_fingerprint(roots: &[std::path::PathBuf]) -> u64 {
    use std::{fs, path::Path};
    fn add(state: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *state ^= u64::from(*byte);
            *state = state.wrapping_mul(0x100000001b3);
        }
    }
    fn visit(path: &Path, state: &mut u64) {
        add(state, path.to_string_lossy().as_bytes());
        let Ok(metadata) = fs::symlink_metadata(path) else {
            add(state, &[0]);
            return;
        };
        add(state, &metadata.len().to_le_bytes());
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_nanos())
            .unwrap_or_default();
        add(state, &modified.to_le_bytes());
        if !metadata.is_dir() || is_directory_link(&metadata) {
            return;
        }
        if let Ok(entries) = fs::read_dir(path) {
            let mut children = entries
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(&child, state);
            }
        }
    }
    let mut fingerprint = 0xcbf29ce484222325;
    for root in roots {
        visit(root, &mut fingerprint);
    }
    fingerprint
}

/// Return whether a path is a filesystem link/reparse point that must not be
/// traversed by discovery. Missing paths are treated as links so a concurrent
/// delete cannot turn a failed metadata read into an unsafe recursion.
pub(crate) fn is_filesystem_link(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| is_directory_link(&metadata))
        .unwrap_or(true)
}

fn is_directory_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::read_bounded;
    use std::io::{Cursor, ErrorKind};

    #[test]
    fn filesystem_fingerprint_tracks_renames_and_nested_changes() {
        use super::filesystem_fingerprint;
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };
        let root = std::env::temp_dir().join(format!(
            "wowie-fingerprint-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("nested")).unwrap();
        let roots = [root.clone()];
        fs::write(root.join("nested/a.txt"), b"same size").unwrap();
        let original = filesystem_fingerprint(&roots);
        assert_eq!(original, filesystem_fingerprint(&roots));
        fs::rename(root.join("nested/a.txt"), root.join("nested/b.txt")).unwrap();
        let renamed = filesystem_fingerprint(&roots);
        assert_ne!(original, renamed);
        fs::write(root.join("nested/b.txt"), b"changed contents and size").unwrap();
        assert_ne!(renamed, filesystem_fingerprint(&roots));
        fs::remove_dir_all(&root).unwrap();
        assert_ne!(original, filesystem_fingerprint(&roots));
        assert_ne!(filesystem_fingerprint(&roots), filesystem_fingerprint(&[]));
    }

    #[test]
    fn bounded_read_accepts_exact_limit_and_empty_input() {
        assert_eq!(read_bounded(&b"abc"[..], 3).unwrap(), b"abc");
        assert!(read_bounded(&b""[..], 0).unwrap().is_empty());
    }

    #[test]
    fn bounded_read_stops_before_consuming_oversized_stream() {
        let mut input = Cursor::new(vec![0; 1024]);
        assert_eq!(
            read_bounded(&mut input, 3).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(input.position(), 4);
        assert!(read_bounded(&b"a"[..], 0).is_err());
    }
}
