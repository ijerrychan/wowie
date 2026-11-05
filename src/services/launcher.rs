use crate::result::SearchResult;
use anyhow::Result;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

pub fn launch(item: &SearchResult) -> Result<()> {
    if let Some(crate::result::Action::SwitchWindow { hwnd, process_id }) = item.primary_action() {
        return crate::platform::switch_window(*hwnd, *process_id);
    }
    #[cfg(target_os = "windows")]
    {
        shell_execute(
            &item.launch_target,
            &item.launch_arguments,
            item.launch_working_directory.as_deref(),
            None,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = std::process::Command::new("open");
        command.arg(&item.launch_target);
        if let Some(directory) = &item.launch_working_directory {
            command.current_dir(directory);
        }
        command.spawn()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(&item.launch_target);
        if let Some(directory) = &item.launch_working_directory {
            command.current_dir(directory);
        }
        command.spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    anyhow::bail!("launch is not supported on this platform")
}

/// Open a preview link using the platform's default browser. Only explicit
/// HTTP(S) URLs are accepted; preview data must never turn into arbitrary
/// shell commands.
pub fn open_url(url: &str) -> Result<()> {
    let url = url.trim();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        anyhow::bail!("preview_link_unsupported");
    }
    if url.chars().any(char::is_whitespace) {
        anyhow::bail!("preview_link_invalid");
    }
    #[cfg(target_os = "windows")]
    {
        shell_execute(std::path::Path::new(url), "", None, None)
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    anyhow::bail!("opening links is not supported on this platform")
}

pub fn launch_entry(entry: &crate::result::LaunchEntry, as_admin: bool) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        shell_execute(
            &entry.target,
            &entry.arguments,
            entry.working_directory.as_deref(),
            as_admin.then_some("runas"),
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (entry, as_admin);
        anyhow::bail!("administrator launch is only supported on Windows")
    }
}

#[cfg(target_os = "windows")]
fn shell_execute(
    file: &std::path::Path,
    arguments: &str,
    directory: Option<&std::path::Path>,
    verb: Option<&str>,
) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::GetLastError,
        UI::Shell::{ShellExecuteExW, SEE_MASK_IDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW},
    };

    let pidl = crate::platform::shell_id_list::decode(file);
    if file.to_string_lossy().starts_with("shell:pidl:") && pidl.is_none() {
        anyhow::bail!("invalid Shell identity list");
    }
    let file = wide_null_os(file);
    let args = wide_null_str(arguments);
    let verb = verb.map(wide_null_str);
    let directory = directory.map(wide_null_os);
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | if pidl.is_some() { SEE_MASK_IDLIST } else { 0 },
        lpVerb: verb
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr()),
        lpFile: if pidl.is_some() {
            std::ptr::null()
        } else {
            file.as_ptr()
        },
        lpIDList: pidl.as_ref().map_or(std::ptr::null_mut(), |value| {
            value.as_ptr().cast_mut().cast()
        }),
        lpParameters: if arguments.is_empty() {
            std::ptr::null()
        } else {
            args.as_ptr()
        },
        lpDirectory: directory
            .as_ref()
            .map_or(std::ptr::null(), |value| value.as_ptr()),
        nShow: 1,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut execute) } == 0 {
        anyhow::bail!("ShellExecuteExW failed with Windows error {}", unsafe {
            GetLastError()
        });
    }
    Ok(())
}

pub fn open_containing_directory(item: &SearchResult) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        if !item.launch_target.is_absolute() || !item.launch_target.exists() {
            anyhow::bail!("the selected item has no supported filesystem location");
        }
        use windows::{
            core::PCWSTR,
            Win32::{
                System::Com::CoTaskMemFree,
                UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName},
            },
        };
        #[link(name = "ole32")]
        unsafe extern "system" {
            fn CoInitializeEx(reserved: *const std::ffi::c_void, co_init: u32) -> i32;
            fn CoUninitialize();
        }
        const COINIT_APARTMENTTHREADED: u32 = 0x2;
        let com_result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED) };
        if com_result < 0 && com_result as u32 != 0x80010106 {
            anyhow::bail!("could not initialize COM for Explorer: HRESULT 0x{com_result:08x}");
        }
        let uninitialize = com_result >= 0;
        let wide_target = wide_null_os(&item.launch_target);
        let mut pidl = std::ptr::null_mut();
        let parse_result =
            unsafe { SHParseDisplayName(PCWSTR(wide_target.as_ptr()), None, &mut pidl, 0, None) };
        if let Err(error) = parse_result {
            if uninitialize {
                unsafe { CoUninitialize() };
            }
            return Err(anyhow::anyhow!("could not resolve target: {error}"));
        }
        if pidl.is_null() {
            if uninitialize {
                unsafe { CoUninitialize() };
            }
            anyhow::bail!("could not resolve target for Explorer");
        }
        let result = unsafe { SHOpenFolderAndSelectItems(pidl, None, 0) };
        unsafe { CoTaskMemFree(Some(pidl.cast())) };
        if uninitialize {
            unsafe { CoUninitialize() };
        }
        result.map_err(|error| anyhow::anyhow!("could not open containing directory: {error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = item;
        anyhow::bail!("opening a containing directory is only supported on Windows")
    }
}

#[cfg(target_os = "windows")]
fn wide_null_os(value: &std::path::Path) -> Vec<u16> {
    value
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(target_os = "windows")]
fn wide_null_str(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn launch_arguments_are_encoded_as_null_terminated_utf16() {
        assert_eq!(
            wide_null_str("--profile 测试"),
            vec![45, 45, 112, 114, 111, 102, 105, 108, 101, 32, 27979, 35797, 0]
        );
    }
}
