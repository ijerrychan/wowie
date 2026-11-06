use crate::platform::VisibleWindow;
use anyhow::{bail, Result};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM},
    Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
    System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
        IsWindowVisible, SetForegroundWindow, ShowWindowAsync, GWL_EXSTYLE, SW_RESTORE,
        WS_EX_TOOLWINDOW,
    },
};

const MAX_WINDOWS: usize = 512;

fn eligible_window(
    visible: bool,
    tool_window: bool,
    cloaked: bool,
    titled: bool,
    pid: u32,
    own_pid: u32,
) -> bool {
    visible && !tool_window && !cloaked && titled && pid != 0 && pid != own_pid
}

pub fn visible_windows() -> Vec<VisibleWindow> {
    let mut values = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect),
            (&mut values as *mut Vec<VisibleWindow>) as LPARAM,
        );
    }
    values
}

unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
    let values = &mut *(data as *mut Vec<VisibleWindow>);
    if values.len() >= MAX_WINDOWS {
        return 0;
    }
    if IsWindow(hwnd) == 0 {
        return 1;
    }
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let own_pid = GetCurrentProcessId();
    if pid == 0 || pid == own_pid {
        return 1;
    }
    let visible = IsWindowVisible(hwnd) != 0;
    let tool_window = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0;
    let mut cloaked = 0u32;
    let cloaked_result = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED as u32,
        &mut cloaked as *mut _ as _,
        std::mem::size_of_val(&cloaked) as u32,
    );
    let is_shell = hwnd == windows_sys::Win32::UI::WindowsAndMessaging::GetShellWindow()
        || hwnd == windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow();
    let mut title = [0u16; 1024];
    let length = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
    if is_shell
        || !eligible_window(
            visible,
            tool_window,
            cloaked_result == 0 && cloaked != 0,
            length > 0,
            pid,
            own_pid,
        )
    {
        return 1;
    }
    let process_path = process_path(pid);
    let process_name = process_path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    values.push(VisibleWindow {
        hwnd: hwnd as usize as u64,
        process_id: pid,
        title: String::from_utf16_lossy(&title[..length as usize]),
        process_name,
        process_path,
    });
    1
}

fn process_path(pid: u32) -> Option<std::path::PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut path = [0u16; 1024];
        let mut length = path.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length);
        windows_sys::Win32::Foundation::CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let path = std::path::PathBuf::from(String::from_utf16_lossy(&path[..length as usize]));
        path.is_absolute().then_some(path)
    }
}

pub fn switch_window(raw: u64, process_id: u32) -> Result<()> {
    if raw == 0
        || raw > usize::MAX as u64
        || process_id == 0
        || process_id == unsafe { GetCurrentProcessId() }
    {
        bail!("window target is invalid");
    }
    let hwnd = raw as usize as HWND;
    if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
        bail!("window is no longer available");
    }
    let mut current_pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut current_pid);
    }
    if current_pid != process_id {
        bail!("window target changed; search again");
    }
    let mut cloaked = 0u32;
    let cloaked_result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut _ as _,
            std::mem::size_of_val(&cloaked) as u32,
        )
    };
    let shell = hwnd == unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetShellWindow() }
        || hwnd == unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow() };
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    let tool_window = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0 };
    let mut title = [0u16; 2];
    let titled = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) } > 0;
    if shell
        || !eligible_window(
            visible,
            tool_window,
            cloaked_result == 0 && cloaked != 0,
            titled,
            current_pid,
            unsafe { GetCurrentProcessId() },
        )
    {
        bail!("window is no longer eligible");
    }
    if unsafe { IsIconic(hwnd) } != 0 {
        unsafe {
            ShowWindowAsync(hwnd, SW_RESTORE);
        }
    }
    if unsafe { SetForegroundWindow(hwnd) } == 0 {
        bail!("Windows refused to activate the window");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::eligible_window;
    #[test]
    fn eligibility_filters_shell_window_states_but_keeps_minimized_window() {
        assert!(eligible_window(true, false, false, true, 2, 1));
        assert!(!eligible_window(false, false, false, true, 2, 1));
        assert!(!eligible_window(true, true, false, true, 2, 1));
        assert!(!eligible_window(true, false, true, true, 2, 1));
        assert!(!eligible_window(true, false, false, false, 2, 1));
        assert!(!eligible_window(true, false, false, true, 1, 1));
        assert!(!eligible_window(true, false, false, true, 0, 1));
    }
}
