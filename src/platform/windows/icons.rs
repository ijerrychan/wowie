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
        bitmap_pixels(icon_info.hbmColor, icon_info.hbmMask, 32, 32)
    })();
    destroy_icon_resources(large_icon as _, &icon_info);
    result
}

fn bitmap_pixels(
    bitmap: *mut std::ffi::c_void,
    mask: *mut std::ffi::c_void,
    width: u32,
    height: u32,
) -> Option<super::super::IconData> {
    let mut pixels = read_bitmap_pixels(bitmap, width, height)?;
    if pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
        // Some legacy Win32 icons store RGB in the color bitmap and put all
        // transparency in the monochrome AND mask. Treating the unused alpha
        // channel as authoritative makes those icons fully invisible.
        if !mask.is_null() {
            if let Some(mask_pixels) = read_bitmap_pixels(mask, width, height) {
                for (pixel, mask_pixel) in pixels
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(mask_pixels.as_chunks::<4>().0)
                {
                    let transparent =
                        mask_pixel[0] > 127 || mask_pixel[1] > 127 || mask_pixel[2] > 127;
                    pixel[3] = if transparent { 0 } else { 255 };
                }
            } else {
                for pixel in pixels.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
            }
        } else {
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
        }
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

fn read_bitmap_pixels(bitmap: *mut std::ffi::c_void, width: u32, height: u32) -> Option<Vec<u8>> {
    use std::mem;
    use windows_sys::Win32::Graphics::Gdi::{
        GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    };
    if bitmap.is_null() {
        return None;
    }
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
    Some(pixels)
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
    // `SHGFI_USEFILEATTRIBUTES` deliberately avoids filesystem access for
    // ordinary files so an extension can resolve its default opener. For an
    // existing directory it would instead describe the path as a normal file
    // (FILE_ATTRIBUTE_NORMAL), producing no folder icon. Let Shell inspect
    // real directories and return their standard or customised icon.
    if _target.is_dir() {
        return application_icon_with_flags(_target, false);
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
    if !use_file_attributes && super::super::windows_rules::is_shell_namespace(_target) {
        // Shell namespace paths such as Control Panel items are virtual PIDLs,
        // not filesystem paths. Resolve them on an initialized COM apartment
        // before asking Shell32 for their icon.
        let _com = super::ComScope::initialize()?;
        // AppsFolder items (including Settings) can expose their actual icon
        // through the image factory while SHGetFileInfo returns no HICON.
        if let Some(icon) = shell_item_image(_target) {
            return Some(icon);
        }
        use windows::{
            core::PCWSTR,
            Win32::{System::Com::CoTaskMemFree, UI::Shell::SHParseDisplayName},
        };
        let owned_pidl = super::super::shell_id_list::decode(_target);
        let mut pidl = owned_pidl.as_ref().map_or(std::ptr::null_mut(), |value| {
            value.as_ptr().cast_mut().cast()
        });
        if owned_pidl.is_none()
            && unsafe {
                SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).is_err()
            }
            || pidl.is_null()
        {
            return None;
        }
        let ok = unsafe {
            SHGetFileInfoW(
                pidl.cast(),
                0,
                &mut file_info,
                mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON | windows_sys::Win32::UI::Shell::SHGFI_PIDL,
            )
        };
        if owned_pidl.is_none() {
            unsafe { CoTaskMemFree(Some(pidl.cast())) };
        }
        if ok == 0 || file_info.hIcon.is_null() {
            return None;
        }
    } else {
        let shell_flags = SHGFI_ICON
            | SHGFI_LARGEICON
            | if use_file_attributes {
                windows_sys::Win32::UI::Shell::SHGFI_USEFILEATTRIBUTES
            } else {
                0
            };
        let ok = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                if use_file_attributes { 0x80 } else { 0 },
                &mut file_info,
                mem::size_of::<SHFILEINFOW>() as u32,
                shell_flags,
            )
        };
        if ok == 0 || file_info.hIcon.is_null() {
            return None;
        }
    }
    let mut icon_info: ICONINFO = unsafe { mem::zeroed() };
    let result = (|| {
        if unsafe { GetIconInfo(file_info.hIcon, &mut icon_info) } == 0 {
            return None;
        }
        bitmap_pixels(icon_info.hbmColor, icon_info.hbmMask, 32, 32)
    })();
    destroy_icon_resources(file_info.hIcon as _, &icon_info);
    result
}

fn shell_item_image(target: &Path) -> Option<super::super::IconData> {
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::SIZE,
            UI::Shell::{
                IShellItemImageFactory, SHCreateItemFromIDList, SHCreateItemFromParsingName,
                SIIGBF_ICONONLY,
            },
        },
    };
    let wide = super::wide_path(target);
    let owned_pidl = super::super::shell_id_list::decode(target);
    let factory: IShellItemImageFactory = if let Some(pidl) = &owned_pidl {
        unsafe { SHCreateItemFromIDList(pidl.as_ptr().cast()).ok()? }
    } else {
        unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()? }
    };
    let bitmap = unsafe {
        factory
            .GetImage(SIZE { cx: 32, cy: 32 }, SIIGBF_ICONONLY)
            .ok()?
    };
    if bitmap.0.is_null() {
        return None;
    }
    let result = (|| {
        use windows_sys::Win32::Graphics::Gdi::{GetObjectW, BITMAP};
        let mut information: BITMAP = unsafe { std::mem::zeroed() };
        if unsafe {
            GetObjectW(
                bitmap.0,
                std::mem::size_of::<BITMAP>() as i32,
                (&mut information as *mut BITMAP).cast(),
            )
        } == 0
        {
            return None;
        }
        let width = u32::try_from(information.bmWidth).ok()?;
        let height = u32::try_from(information.bmHeight).ok()?;
        if width == 0 || height == 0 || width > 128 || height > 128 {
            return None;
        }
        bitmap_pixels(bitmap.0, std::ptr::null_mut(), width, height)
    })();
    unsafe { windows_sys::Win32::Graphics::Gdi::DeleteObject(bitmap.0) };
    result
}
