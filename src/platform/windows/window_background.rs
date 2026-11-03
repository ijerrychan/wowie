//! Fill native exposed pixels while Slint is waiting to present its next frame.
//! The brush is per HWND: changing a window-class brush also changes other
//! windows and can leave light windows with a dark fallback (or vice versa).
use std::cell::Cell;
use windows_sys::Win32::{
    Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, GetDC, ReleaseDC, HDC},
    UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, GetClientRect, GetPropW, GetWindowLongPtrW,
        IsWindowVisible, RemovePropW, SetPropW, SetWindowLongPtrW, GWLP_WNDPROC, SWP_SHOWWINDOW,
        WINDOWPOS, WM_ERASEBKGND, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_WINDOWPOSCHANGED, WNDPROC,
    },
};

const BACKGROUND_PROPERTY: windows_sys::core::PCWSTR = windows_sys::w!("Wowie.WindowBackground");

struct Background {
    color: Cell<COLORREF>,
    size: Cell<(i32, i32)>,
    previous_proc: WNDPROC,
    painting: Cell<bool>,
}

pub(super) fn set_background(hwnd: HWND, color: slint::Color) {
    let color = color.red() as u32 | (color.green() as u32) << 8 | (color.blue() as u32) << 16;
    // Called only on the HWND's UI thread. The allocation belongs to the
    // subclass until WM_NCDESTROY; failed installation frees it immediately.
    unsafe {
        let data = GetPropW(hwnd, BACKGROUND_PROPERTY) as *mut Background;
        if !data.is_null() {
            (*data).color.set(color);
        } else {
            let previous_proc = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
            if previous_proc == 0 {
                return;
            }
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            let data = Box::into_raw(Box::new(Background {
                color: Cell::new(color),
                size: Cell::new((rect.right, rect.bottom)),
                previous_proc: std::mem::transmute::<isize, WNDPROC>(previous_proc),
                painting: Cell::new(false),
            }));
            if SetPropW(hwnd, BACKGROUND_PROPERTY, data as _) == 0 {
                drop(Box::from_raw(data));
                return;
            }
            if SetWindowLongPtrW(hwnd, GWLP_WNDPROC, background_proc as *const () as isize) == 0 {
                RemovePropW(hwnd, BACKGROUND_PROPERTY);
                drop(Box::from_raw(data));
                return;
            }
        }
        // A retained hidden window may still contain the previous theme/frame.
        // Seed it before mapping, rather than showing white/black until redraw.
        if IsWindowVisible(hwnd) == 0 {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            paint_rect(hwnd, &rect, color);
        }
    }
}

unsafe fn fill_rect(dc: HDC, rect: &RECT, color: COLORREF) -> bool {
    if dc.is_null() || rect.right <= rect.left || rect.bottom <= rect.top {
        return false;
    }
    let brush = CreateSolidBrush(color);
    if brush.is_null() {
        return false;
    }
    let painted = FillRect(dc, rect, brush) != 0;
    DeleteObject(brush);
    painted
}

unsafe fn paint_rect(hwnd: HWND, rect: &RECT, color: COLORREF) {
    let dc = GetDC(hwnd);
    if !dc.is_null() {
        fill_rect(dc, rect, color);
        ReleaseDC(hwnd, dc);
    }
}

unsafe extern "system" fn background_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let data = GetPropW(hwnd, BACKGROUND_PROPERTY) as *mut Background;
    if data.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    // The property is installed before replacing the procedure and is removed
    // only after the procedure is restored during destruction.
    let background = &*data;
    let previous_proc = background.previous_proc;
    if message == WM_NCDESTROY {
        if GetWindowLongPtrW(hwnd, GWLP_WNDPROC) == background_proc as *const () as isize {
            SetWindowLongPtrW(
                hwnd,
                GWLP_WNDPROC,
                previous_proc
                    .map(|proc| proc as *const () as isize)
                    .unwrap_or(0),
            );
        }
        let result = CallWindowProcW(previous_proc, hwnd, message, wparam, lparam);
        RemovePropW(hwnd, BACKGROUND_PROPERTY);
        drop(Box::from_raw(data));
        return result;
    }
    if message == WM_ERASEBKGND {
        // Winit draws Slint before its WM_PAINT handler calls DefWindowProc.
        // A nested erase at that point must not overwrite the submitted frame.
        if background.painting.get() {
            return 1;
        }
        let mut rect = RECT::default();
        GetClientRect(hwnd, &mut rect);
        if fill_rect(wparam as HDC, &rect, background.color.get()) {
            return 1;
        }
    }
    if message == WM_PAINT {
        let was_painting = background.painting.replace(true);
        let result = CallWindowProcW(previous_proc, hwnd, message, wparam, lparam);
        if GetPropW(hwnd, BACKGROUND_PROPERTY) == data as _ {
            (*data).painting.set(was_painting);
        }
        return result;
    }
    if message == WM_SIZE {
        let mut rect = RECT::default();
        GetClientRect(hwnd, &mut rect);
        let (old_width, old_height) = background.size.replace((rect.right, rect.bottom));
        let color = background.color.get();
        // Fill only newly exposed strips; preserve the presented search row
        // and other existing content during compact -> results expansion.
        if rect.bottom > old_height {
            paint_rect(
                hwnd,
                &RECT {
                    top: old_height,
                    ..rect
                },
                color,
            );
        }
        if rect.right > old_width {
            paint_rect(
                hwnd,
                &RECT {
                    left: old_width,
                    bottom: old_height.min(rect.bottom),
                    ..rect
                },
                color,
            );
        }
    }
    if message == WM_WINDOWPOSCHANGED && lparam != 0 {
        let position = &*(lparam as *const WINDOWPOS);
        if position.flags & SWP_SHOWWINDOW != 0 {
            let color = background.color.get();
            let result = CallWindowProcW(previous_proc, hwnd, message, wparam, lparam);
            // WM_SHOWWINDOW arrives before mapping. At WINDOWPOSCHANGED the
            // client DC is drawable, so cover the retained/empty native surface
            // with the correct canvas until the explicitly requested redraw.
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            paint_rect(hwnd, &rect, color);
            return result;
        }
    }
    CallWindowProcW(previous_proc, hwnd, message, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::{
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, GetPixel, SelectObject, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, SendMessageW, SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE,
            SWP_NOZORDER, WS_POPUP,
        },
    };

    struct HiddenWindow(HWND);
    impl HiddenWindow {
        fn new() -> Self {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    100,
                    100,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null());
            Self(hwnd)
        }
    }
    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    struct Pixels {
        dc: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }
    impl Pixels {
        fn new() -> Self {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 200,
                    biHeight: -200,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            unsafe {
                let dc = CreateCompatibleDC(std::ptr::null_mut());
                assert!(!dc.is_null());
                let mut bits = std::ptr::null_mut();
                let bitmap = CreateDIBSection(
                    dc,
                    &info,
                    DIB_RGB_COLORS,
                    &mut bits,
                    std::ptr::null_mut(),
                    0,
                );
                assert!(!bitmap.is_null());
                let previous = SelectObject(dc, bitmap);
                Self {
                    dc,
                    bitmap,
                    previous,
                }
            }
        }
        fn erase(&self, window: HWND) {
            assert_eq!(
                unsafe { SendMessageW(window, WM_ERASEBKGND, self.dc as usize, 0) },
                1
            );
        }
        fn at(&self, x: i32, y: i32) -> COLORREF {
            unsafe { GetPixel(self.dc, x, y) }
        }
    }
    impl Drop for Pixels {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.previous);
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
            }
        }
    }

    #[test]
    fn native_background_is_per_window_and_tracks_theme_and_resize() {
        let dark = HiddenWindow::new();
        let light = HiddenWindow::new();
        let dark_color = slint::Color::from_rgb_u8(0x20, 0x20, 0x20);
        let light_color = slint::Color::from_rgb_u8(0xf8, 0xf9, 0xfb);
        set_background(dark.0, dark_color);
        set_background(light.0, light_color);
        let pixels = Pixels::new();
        pixels.erase(dark.0);
        assert_eq!(pixels.at(50, 50), 0x202020);
        pixels.erase(light.0);
        assert_eq!(pixels.at(50, 50), 0xfbf9f8);
        pixels.erase(dark.0);
        assert_eq!(pixels.at(50, 50), 0x202020);

        // Theme changes update the existing subclass rather than stacking one.
        set_background(dark.0, light_color);
        pixels.erase(dark.0);
        assert_eq!(pixels.at(50, 50), 0xfbf9f8);
        unsafe {
            SetWindowPos(
                dark.0,
                std::ptr::null_mut(),
                0,
                0,
                150,
                150,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
            );
        }
        pixels.erase(dark.0);
        assert_eq!(pixels.at(140, 140), 0xfbf9f8);
        assert_eq!(unsafe { IsWindowVisible(dark.0) }, 0);
    }

    thread_local! {
        static ORIGINAL_TEST_PROC: Cell<WNDPROC> = const { Cell::new(None) };
    }

    unsafe extern "system" fn draw_then_erase_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_PAINT && wparam != 0 {
            // Model winit's order: present the scene, then let the default
            // WM_PAINT processing issue an erase. The scene must survive.
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            fill_rect(wparam as HDC, &rect, 0x557799);
            SendMessageW(hwnd, WM_ERASEBKGND, wparam, 0);
            return 0;
        }
        ORIGINAL_TEST_PROC
            .with(|previous| CallWindowProcW(previous.get(), hwnd, message, wparam, lparam))
    }

    #[test]
    fn native_erase_during_paint_does_not_cover_the_rendered_frame() {
        let window = HiddenWindow::new();
        unsafe {
            let previous = SetWindowLongPtrW(
                window.0,
                GWLP_WNDPROC,
                draw_then_erase_proc as *const () as isize,
            );
            ORIGINAL_TEST_PROC
                .with(|saved| saved.set(std::mem::transmute::<isize, WNDPROC>(previous)));
        }
        set_background(window.0, slint::Color::from_rgb_u8(0x20, 0x20, 0x20));
        let pixels = Pixels::new();
        unsafe {
            SendMessageW(window.0, WM_PAINT, pixels.dc as usize, 0);
        }
        assert_eq!(pixels.at(50, 50), 0x557799);
        // A subsequent erase outside WM_PAINT still fills the canvas.
        pixels.erase(window.0);
        assert_eq!(pixels.at(50, 50), 0x202020);
    }
}
