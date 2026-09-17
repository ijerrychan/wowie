use crate::result::SearchResult;
use anyhow::Result;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

pub fn launch(item: &SearchResult) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::{
            Foundation::GetLastError,
            UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW},
        };
        let file = wide_null_os(&item.launch_target);
        let args = wide_null_str(&item.launch_arguments);
        let directory = item
            .launch_working_directory
            .as_ref()
            .map(|path| wide_null_os(path));
        let mut execute = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpFile: file.as_ptr(),
            lpParameters: if item.launch_arguments.is_empty() {
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
