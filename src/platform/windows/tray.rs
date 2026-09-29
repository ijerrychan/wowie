use super::super::{PlatformEvent, TrayMenuLabels, TrayRegistration};
use std::{
    mem,
    sync::{Arc, Mutex},
};

const WM_TRAY: u32 = 0x8001;
const WM_COMMAND: u32 = 0x0111;
const WM_LBUTTONUP: usize = 0x0202;
const WM_RBUTTONUP: usize = 0x0205;
const WM_NULL: u32 = 0x0000;
const WM_QUIT: u32 = 0x0012;
const ID_SETTINGS: usize = 1000;
const ID_EXIT: usize = 1001;
const NIF_MESSAGE: u32 = 0x00000001;
const NIF_ICON: u32 = 0x00000002;
const NIF_TIP: u32 = 0x00000004;
const NIM_ADD: u32 = 0x00000000;
const NIM_DELETE: u32 = 0x00000002;
const MF_STRING: u32 = 0x00000000;
const MF_SEPARATOR: u32 = 0x00000800;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_RETURNCMD: u32 = 0x0100;
const GWLP_USERDATA: i32 = -21;
const APP_ICON_RESOURCE: usize = 1;

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Message {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    point: Point,
    private: u32,
}

#[repr(C)]
struct NotifyIconData {
    cb_size: u32,
    hwnd: isize,
    id: u32,
    flags: u32,
    callback_message: u32,
    icon: isize,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    timeout_or_version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u8; 16],
    balloon_icon: isize,
}

#[repr(C)]
struct WindowClass {
    style: u32,
    proc: Option<unsafe extern "system" fn(isize, u32, usize, isize) -> isize>,
    extra_class: i32,
    extra_window: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu: *const u16,
    name: *const u16,
}

struct CallbackState {
    sender: std::sync::mpsc::Sender<PlatformEvent>,
    labels: Arc<Mutex<TrayMenuLabels>>,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WindowClass) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        parameter: isize,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, message: u32, wparam: usize, lparam: isize) -> isize;
    fn DestroyWindow(hwnd: isize) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn PostMessageW(hwnd: isize, message: u32, wparam: usize, lparam: isize) -> i32;
    fn PostQuitMessage(exit_code: i32);
    fn PeekMessageW(message: *mut Message, hwnd: isize, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(message: *const Message) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
    fn CreatePopupMenu() -> isize;
    fn AppendMenuW(menu: isize, flags: u32, id: usize, text: *const u16) -> i32;
    fn TrackPopupMenu(
        menu: isize,
        flags: u32,
        x: i32,
        y: i32,
        reserved: i32,
        hwnd: isize,
        rect: *const u8,
    ) -> usize;
    fn DestroyMenu(menu: isize) -> i32;
    fn LoadIconW(instance: isize, name: usize) -> isize;
    fn GetModuleHandleW(module_name: *const u16) -> isize;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *const NotifyIconData) -> i32;
}

unsafe extern "system" fn window_proc(
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    let state = || GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const CallbackState;
    if message == WM_TRAY {
        match lparam as usize {
            WM_LBUTTONUP => {
                if !state().is_null() {
                    let _ = (*state()).sender.send(PlatformEvent::TogglePalette);
                }
            }
            WM_RBUTTONUP if !state().is_null() => {
                if let Ok(labels) = (*state()).labels.lock() {
                    show_menu(hwnd, &labels);
                }
            }
            _ => {}
        }
        return 0;
    }
    if message == WM_COMMAND {
        if state().is_null() {
            return 0;
        }
        match wparam & 0xffff {
            ID_SETTINGS => {
                let _ = (*state()).sender.send(PlatformEvent::OpenSettings);
                return 0;
            }
            ID_EXIT => {
                let _ = (*state()).sender.send(PlatformEvent::Quit);
                PostQuitMessage(0);
                return 0;
            }
            _ => {}
        }
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

unsafe fn show_menu(hwnd: isize, labels: &TrayMenuLabels) {
    let menu = CreatePopupMenu();
    if menu == 0 {
        return;
    }
    let settings = wide(&labels.settings);
    let exit = wide(&labels.exit);
    AppendMenuW(menu, MF_STRING, ID_SETTINGS, settings.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, exit.as_ptr());
    let mut point = Point { x: 0, y: 0 };
    GetCursorPos(&mut point);
    SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        point.x,
        point.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    if command != 0 {
        PostMessageW(hwnd, WM_COMMAND, command, 0);
    }
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn start(
    labels: TrayMenuLabels,
    shutdown: &super::super::PlatformShutdown,
) -> TrayRegistration {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (updates, update_receiver) = std::sync::mpsc::channel::<TrayMenuLabels>();
    let labels = Arc::new(Mutex::new(labels));
    let labels_for_thread = Arc::clone(&labels);
    shutdown.spawn(move |signal| unsafe {
        let class_name = wide("WowieTrayWindow");
        let instance = GetModuleHandleW(std::ptr::null());
        let state = Box::into_raw(Box::new(CallbackState {
            sender,
            labels: labels_for_thread,
        }));
        let class = WindowClass {
            style: 0,
            proc: Some(window_proc),
            extra_class: 0,
            extra_window: 0,
            instance,
            icon: LoadIconW(instance, APP_ICON_RESOURCE),
            cursor: 0,
            background: 0,
            menu: std::ptr::null(),
            name: class_name.as_ptr(),
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            instance,
            0,
        );
        if hwnd == 0 {
            drop(Box::from_raw(state));
            return;
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        let mut tip = [0_u16; 128];
        let label = wide("wowie");
        let tip_len = label.len().min(tip.len());
        tip[..tip_len].copy_from_slice(&label[..tip_len]);
        let data = NotifyIconData {
            cb_size: mem::size_of::<NotifyIconData>() as u32,
            hwnd,
            id: 1,
            flags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            callback_message: WM_TRAY,
            icon: LoadIconW(instance, APP_ICON_RESOURCE),
            tip,
            state: 0,
            state_mask: 0,
            info: [0; 256],
            timeout_or_version: 0,
            info_title: [0; 64],
            info_flags: 0,
            guid: [0; 16],
            balloon_icon: 0,
        };
        if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(state));
            DestroyWindow(hwnd);
            return;
        }
        let mut message = Message {
            hwnd: 0,
            message: 0,
            wparam: 0,
            lparam: 0,
            time: 0,
            point: Point { x: 0, y: 0 },
            private: 0,
        };
        'message_loop: while !signal.is_stopped() {
            while let Ok(updated) = update_receiver.try_recv() {
                if let Ok(mut current) = (*state).labels.lock() {
                    *current = updated;
                }
            }
            while PeekMessageW(&mut message, 0, 0, 0, 1) != 0 {
                if message.message == WM_QUIT {
                    break 'message_loop;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let stop = signal.event();
            let wait = windows_sys::Win32::UI::WindowsAndMessaging::MsgWaitForMultipleObjectsEx(
                1,
                &stop,
                250,
                windows_sys::Win32::UI::WindowsAndMessaging::QS_ALLINPUT,
                windows_sys::Win32::UI::WindowsAndMessaging::MWMO_INPUTAVAILABLE,
            );
            if wait == 0 || wait == u32::MAX {
                break;
            }
        }
        Shell_NotifyIconW(NIM_DELETE, &data);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        drop(Box::from_raw(state));
        DestroyWindow(hwnd);
    });
    TrayRegistration {
        events: receiver,
        updates,
    }
}
