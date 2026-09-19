//! Windows ShellLink resolution and fallback `.lnk` parsing.

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::com::{wide_path, ComScope};

const SHELL_LINK_CLSID: windows::core::GUID =
    windows::core::GUID::from_u128(0x00021401_0000_0000_c000_000000000046);

pub(crate) fn resolve_shortcut(path: &Path) -> super::super::ShortcutInfo {
    if let Some(shortcut) = resolve_shortcut_native(path) {
        return shortcut;
    }

    // Keep the binary parser as a fallback for environments where COM cannot
    // be initialized. The legacy path above is the authoritative resolver.
    let Ok(data) = fs::read(path) else {
        return super::super::ShortcutInfo::default();
    };
    if data.len() < 0x4c || &data[..4] != b"\x4c\x00\x00\x00" {
        return super::super::ShortcutInfo::default();
    }

    let target = u32_at(&data, 0x48)
        .and_then(|offset| (offset != 0).then_some(offset as usize))
        .and_then(|offset| parse_link_info(&data, offset));
    super::super::ShortcutInfo {
        target,
        ..Default::default()
    }
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
    let header_size = u32_at(data, offset + 4)? as usize;
    if size < 0x1c || offset.checked_add(size)? > data.len() {
        return None;
    }
    let ansi_local_offset = u32_at(data, offset + 0x10).unwrap_or_default() as usize;
    let ansi_suffix_offset = u32_at(data, offset + 0x18).unwrap_or_default() as usize;
    let unicode_local_offset = (header_size >= 0x24)
        .then(|| u32_at(data, offset + 0x1c).unwrap_or_default() as usize)
        .filter(|value| *value != 0);
    let unicode_suffix_offset = (header_size >= 0x24)
        .then(|| u32_at(data, offset + 0x20).unwrap_or_default() as usize)
        .filter(|value| *value != 0);
    if ansi_local_offset == 0 && unicode_local_offset.is_none() {
        return None;
    }
    let base = if let Some(local_offset) = unicode_local_offset {
        read_utf16_z(data, offset + local_offset)
    } else {
        read_ansi_z(data, offset + ansi_local_offset)
    }?;
    let suffix = if let Some(suffix_offset) = unicode_suffix_offset {
        read_utf16_z(data, offset + suffix_offset).unwrap_or_default()
    } else if ansi_suffix_offset == 0 {
        String::new()
    } else {
        read_ansi_z(data, offset + ansi_suffix_offset).unwrap_or_default()
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
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
