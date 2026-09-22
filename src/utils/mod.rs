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
