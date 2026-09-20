//! Windows icon extraction and default-association lookup.

use std::path::Path;

/// Native icon extraction entry point. The pixel conversion is intentionally
/// kept behind the platform boundary so callers can cache the result without
/// depending on HICON/HBITMAP types.
pub(crate) fn application_icon(_target: &Path) -> Option<super::super::IconData> {
    application_icon_with_flags(_target, false)
}

pub(crate) fn indexed_application_icon(
    target: &Path,
    index: i32,
) -> Option<super::super::IconData> {
    use std::{ffi::c_void, mem, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    let wide: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut large_icon: *mut c_void = std::ptr::null_mut();
    let extracted = unsafe {
        ExtractIconExW(
            wide.as_ptr(),
            index,
            &mut large_icon,
            std::ptr::null_mut(),
            1,
        )
    };
    if extracted == 0 || large_icon.is_null() {
        return None;
    }
    let mut icon_info: ICONINFO = unsafe { mem::zeroed() };
    let result = (|| {
        if unsafe { GetIconInfo(large_icon as _, &mut icon_info) } == 0 {
            return None;
        }
        bitmap_pixels(icon_info.hbmColor, 32, 32)
    })();
    destroy_icon_resources(large_icon as _, &icon_info);
    result
}

fn bitmap_pixels(
    bitmap: *mut std::ffi::c_void,
    width: u32,
    height: u32,
) -> Option<super::super::IconData> {
    use std::mem;
    use windows_sys::Win32::Graphics::Gdi::{
        GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    };
    let mut pixels = vec![0_u8; (width * height * 4) as usize];
    let mut bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..unsafe { mem::zeroed() }
        },
        bmiColors: [unsafe { mem::zeroed() }],
    };
    let dc = unsafe { GetDC(std::ptr::null_mut()) };
    let copied = !dc.is_null()
        && unsafe {
            GetDIBits(
                dc,
                bitmap,
                0,
                height,
                pixels.as_mut_ptr().cast(),
                &mut bitmap_info,
                DIB_RGB_COLORS,
            )
        } != 0;
    if !dc.is_null() {
        unsafe { ReleaseDC(std::ptr::null_mut(), dc) };
    }
    if !copied {
        return None;
    }
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(super::super::IconData {
        width,
        height,
        rgba: pixels,
    })
}

fn delete_icon_bitmaps(icon_info: &windows_sys::Win32::UI::WindowsAndMessaging::ICONINFO) {
    if !icon_info.hbmColor.is_null() {
        unsafe { windows_sys::Win32::Graphics::Gdi::DeleteObject(icon_info.hbmColor as _) };
    }
    if !icon_info.hbmMask.is_null() {
        unsafe { windows_sys::Win32::Graphics::Gdi::DeleteObject(icon_info.hbmMask as _) };
    }
}

fn destroy_icon_resources(
    icon: *mut std::ffi::c_void,
    icon_info: &windows_sys::Win32::UI::WindowsAndMessaging::ICONINFO,
) {
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(icon as _);
    }
    delete_icon_bitmaps(icon_info);
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ExtractIconExW(
        file: *const u16,
        index: i32,
        large: *mut *mut std::ffi::c_void,
        small: *mut *mut std::ffi::c_void,
        count: u32,
    ) -> u32;
}

pub(crate) fn default_open_icon(_target: &Path) -> Option<super::super::IconData> {
    let is_web_url = _target
        .to_string_lossy()
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        || _target
            .to_string_lossy()
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"));
    if is_web_url {
        // URL associations are exposed through the browser's HTML file
        // association on Windows. Query a synthetic HTML name so Shell32
        // returns the actual default browser icon instead of a generic URL
        // shortcut icon.
        return application_icon_with_flags(Path::new("wowie-default-browser.html"), true);
    }
    application_icon_with_flags(_target, true)
}

fn application_icon_with_flags(
    _target: &Path,
    use_file_attributes: bool,
) -> Option<super::super::IconData> {
    use std::{mem, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::UI::{
        Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON},
        WindowsAndMessaging::{GetIconInfo, ICONINFO},
    };
    let wide: Vec<u16> = _target.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file_info: SHFILEINFOW = unsafe { mem::zeroed() };
    let shell_flags = SHGFI_ICON
        | SHGFI_LARGEICON
        | if use_file_attributes {
            windows_sys::Win32::UI::Shell::SHGFI_USEFILEATTRIBUTES
        } else {
            0
        };
    let mut ok = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            if use_file_attributes { 0x80 } else { 0 },
            &mut file_info,
            mem::size_of::<SHFILEINFOW>() as u32,
            shell_flags,
        )
    };
    if !use_file_attributes
        && (ok == 0 || file_info.hIcon.is_null())
        && super::super::windows_rules::is_shell_namespace(_target)
    {
        use windows::{
            core::PCWSTR,
            Win32::{System::Com::CoTaskMemFree, UI::Shell::SHParseDisplayName},
        };
        let mut pidl = std::ptr::null_mut();
        if unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).is_ok() }
            && !pidl.is_null()
        {
            ok = unsafe {
                SHGetFileInfoW(
                    pidl.cast(),
                    0,
                    &mut file_info,
                    mem::size_of::<SHFILEINFOW>() as u32,
                    SHGFI_ICON | SHGFI_LARGEICON | windows_sys::Win32::UI::Shell::SHGFI_PIDL,
                )
            };
            unsafe { CoTaskMemFree(Some(pidl.cast())) };
        }
    }
    if ok == 0 || file_info.hIcon.is_null() {
        return None;
    }
    let mut icon_info: ICONINFO = unsafe { mem::zeroed() };
    let result = (|| {
        if unsafe { GetIconInfo(file_info.hIcon, &mut icon_info) } == 0 {
            return None;
        }
        bitmap_pixels(icon_info.hbmColor, 32, 32)
    })();
    destroy_icon_resources(file_info.hIcon as _, &icon_info);
    result
}
