//! Windows ShellLink resolution and fallback `.lnk` parsing.

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::com::{wide_path, ComScope};

const SHELL_LINK_CLSID: windows::core::GUID =
    windows::core::GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
const MAX_SHORTCUT_FILE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn resolve_shortcut(path: &Path) -> super::super::ShortcutInfo {
    if fs::metadata(path)
        .map(|metadata| metadata.len() > MAX_SHORTCUT_FILE_BYTES as u64)
        .unwrap_or(false)
    {
        eprintln!("ignoring oversized shortcut file: {}", path.display());
        return super::super::ShortcutInfo::default();
    }
    if let Some(shortcut) = resolve_shortcut_native(path) {
        return discard_icon_file_target(shortcut);
    }

    // Keep the binary parser as a fallback for environments where COM cannot
    // be initialized. The legacy path above is the authoritative resolver.
    let Ok(file) = fs::File::open(path) else {
        return super::super::ShortcutInfo::default();
    };
    let Ok(data) = crate::utils::read_bounded(file, MAX_SHORTCUT_FILE_BYTES) else {
        return super::super::ShortcutInfo::default();
    };
    if data.len() < 0x4c || &data[..4] != b"\x4c\x00\x00\x00" {
        return super::super::ShortcutInfo::default();
    }

    let target = u32_at(&data, 0x48)
        .and_then(|offset| (offset != 0).then_some(offset as usize))
        .and_then(|offset| parse_link_info(&data, offset));
    discard_icon_file_target(super::super::ShortcutInfo {
        target,
        ..Default::default()
    })
}

fn discard_icon_file_target(
    mut shortcut: super::super::ShortcutInfo,
) -> super::super::ShortcutInfo {
    if let Some(target) = shortcut.target.as_deref() {
        if crate::platform::windows_rules::is_icon_path(target) {
            if std::env::var_os("WOWIE_PROVIDER_DEBUG").is_some() {
                eprintln!(
                    "ignoring shortcut with icon-file target: {}",
                    target.display()
                );
            }
            shortcut.target = None;
        }
    }
    shortcut
}

fn resolve_shortcut_native(path: &Path) -> Option<super::super::ShortcutInfo> {
    use windows::{
        core::{Interface, PCWSTR},
        Win32::{
            Storage::FileSystem::WIN32_FIND_DATAW,
            System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ},
            UI::Shell::{IShellLinkW, SLGP_RAWPATH},
        },
    };

    let _com = ComScope::initialize()?;
    let link: IShellLinkW =
        unsafe { CoCreateInstance(&SHELL_LINK_CLSID, None, CLSCTX_INPROC_SERVER).ok()? };
    let persist: IPersistFile = link.cast().ok()?;
    let path = wide_path(path);
    unsafe { persist.Load(PCWSTR(path.as_ptr()), STGM_READ).ok()? };
    unsafe {
        link.Resolve(
            windows::Win32::Foundation::HWND(std::ptr::null_mut()),
            windows::Win32::UI::Shell::SLR_NO_UI.0 as u32,
        )
        .ok()?;
    }

    let mut target = vec![0_u16; 32_768];
    unsafe {
        link.GetPath(
            &mut target,
            std::ptr::null_mut::<WIN32_FIND_DATAW>(),
            SLGP_RAWPATH.0 as u32,
        )
        .ok()?;
    }
    let length = target.iter().position(|unit| *unit == 0)?;
    if length == 0 {
        return None;
    }
    let mut arguments = vec![0_u16; 32_768];
    unsafe { link.GetArguments(&mut arguments).ok()? };
    let argument_length = arguments.iter().position(|unit| *unit == 0).unwrap_or(0);
    let arguments =
        (argument_length > 0).then(|| String::from_utf16_lossy(&arguments[..argument_length]));
    let mut working_directory = vec![0_u16; 32_768];
    unsafe { link.GetWorkingDirectory(&mut working_directory).ok()? };
    let working_directory_length = working_directory
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(0);
    let working_directory = (working_directory_length > 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(
            &working_directory[..working_directory_length],
        ))
    });
    let mut icon_path_buffer = vec![0_u16; 32_768];
    let mut icon_index = 0_i32;
    let icon_path = unsafe {
        link.GetIconLocation(&mut icon_path_buffer, &mut icon_index)
            .ok()
    }
    .and_then(|_| {
        let length = icon_path_buffer.iter().position(|unit| *unit == 0)?;
        (length > 0).then(|| PathBuf::from(String::from_utf16_lossy(&icon_path_buffer[..length])))
    });
    let icon_index = icon_path.as_ref().map(|_| icon_index);

    Some(super::super::ShortcutInfo {
        target: Some(PathBuf::from(String::from_utf16_lossy(&target[..length]))),
        arguments,
        working_directory,
        icon_path,
        icon_index,
    })
}

fn parse_link_info(data: &[u8], offset: usize) -> Option<PathBuf> {
    let size = u32_at(data, offset)? as usize;
    let end = offset.checked_add(size)?;
    if size < 0x1c || end > data.len() {
        return None;
    }
    let link_info = data.get(offset..end)?;
    let header_size = u32_at(link_info, 4)? as usize;
    if !(0x1c..=size).contains(&header_size) {
        return None;
    }
    let ansi_local_offset = u32_at(link_info, 0x10).unwrap_or_default() as usize;
    let ansi_local_offset = if ansi_local_offset >= header_size && ansi_local_offset < size {
        ansi_local_offset
    } else {
        0
    };
    let ansi_suffix_offset = u32_at(link_info, 0x18).unwrap_or_default() as usize;
    let ansi_suffix_offset = if ansi_suffix_offset >= header_size && ansi_suffix_offset < size {
        ansi_suffix_offset
    } else {
        0
    };
    let unicode_local_offset = (header_size >= 0x24)
        .then(|| u32_at(link_info, 0x1c).unwrap_or_default() as usize)
        .filter(|value| *value >= header_size && *value < size);
    let unicode_suffix_offset = (header_size >= 0x24)
        .then(|| u32_at(link_info, 0x20).unwrap_or_default() as usize)
        .filter(|value| *value >= header_size && *value < size);
    if ansi_local_offset == 0 && unicode_local_offset.is_none() {
        return None;
    }
    let base = if let Some(local_offset) = unicode_local_offset {
        read_utf16_z(link_info, local_offset)
    } else if ansi_local_offset < size {
        read_ansi_z(link_info, ansi_local_offset)
    } else {
        None
    }?;
    let suffix = if let Some(suffix_offset) = unicode_suffix_offset {
        read_utf16_z(link_info, suffix_offset).unwrap_or_default()
    } else if ansi_suffix_offset == 0 || ansi_suffix_offset >= size {
        String::new()
    } else {
        read_ansi_z(link_info, ansi_suffix_offset).unwrap_or_default()
    };
    Some(PathBuf::from(if suffix.is_empty() {
        base
    } else {
        format!("{base}\\{suffix}")
    }))
}

fn read_utf16_z(data: &[u8], offset: usize) -> Option<String> {
    let bytes = data.get(offset..)?;
    let end = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .position(|pair| *pair == [0, 0])?;
    String::from_utf16(
        &bytes[..end * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    )
    .ok()
}

fn read_ansi_z(data: &[u8], offset: usize) -> Option<String> {
    let bytes = data.get(offset..)?;
    let end = bytes.iter().position(|byte| *byte == 0)?;
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    Some(u32::from_le_bytes(data.get(offset..end)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::{parse_link_info, resolve_shortcut, MAX_SHORTCUT_FILE_BYTES};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn link_info_rejects_truncated_offsets() {
        let mut data = vec![0_u8; 0x1c];
        data[0..4].copy_from_slice(&(0x1c_u32).to_le_bytes());
        data[4..8].copy_from_slice(&(0x24_u32).to_le_bytes());
        assert!(parse_link_info(&data, 0).is_none());
    }

    #[test]
    fn truncated_shell_link_fixture_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "wowie-truncated-shortcut-{}-{}.lnk",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            include_bytes!("../../../tests/fixtures/windows/truncated-shell-link.lnk"),
        )
        .unwrap();
        assert!(resolve_shortcut(&path).target.is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn link_info_reads_unicode_local_base_and_ansi_suffix() {
        let mut data = vec![0_u8; 0x24 + 10 * 2 + 8];
        let size = data.len() as u32;
        data[0..4].copy_from_slice(&size.to_le_bytes());
        data[4..8].copy_from_slice(&(0x24_u32).to_le_bytes());
        data[0x1c..0x20].copy_from_slice(&(0x24_u32).to_le_bytes());
        data[0x18..0x1c].copy_from_slice(&(0x24_u32 + 10 * 2).to_le_bytes());
        let base = "C:\\工具";
        for (index, unit) in base.encode_utf16().enumerate() {
            data[0x24 + index * 2..0x26 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let suffix_offset = 0x24 + 10 * 2;
        data[suffix_offset..suffix_offset + 8].copy_from_slice(b"app.exe\0");
        assert_eq!(
            parse_link_info(&data, 0),
            Some(PathBuf::from("C:\\工具\\app.exe"))
        );
    }

    #[test]
    fn link_info_reads_ansi_local_base() {
        let mut data = vec![0_u8; 0x1c + 8 + 8];
        let size = data.len() as u32;
        data[0..4].copy_from_slice(&size.to_le_bytes());
        data[4..8].copy_from_slice(&(0x1c_u32).to_le_bytes());
        data[0x10..0x14].copy_from_slice(&(0x1c_u32).to_le_bytes());
        data[0x18..0x1c].copy_from_slice(&(0x1c_u32 + 8).to_le_bytes());
        data[0x1c..0x24].copy_from_slice(b"C:\\bin\0\0");
        data[0x24..0x2c].copy_from_slice(b"app.exe\0");
        assert_eq!(
            parse_link_info(&data, 0),
            Some(PathBuf::from("C:\\bin\\app.exe"))
        );
    }

    #[test]
    fn oversized_shortcut_is_ignored_before_fallback_read() {
        let path = std::env::temp_dir().join(format!(
            "wowie-shortcut-{}-{}-oversized.lnk",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, vec![0_u8; MAX_SHORTCUT_FILE_BYTES + 1]).unwrap();
        assert!(resolve_shortcut(&path).target.is_none());
        let _ = fs::remove_file(path);
    }
}
