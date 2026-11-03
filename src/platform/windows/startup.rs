use anyhow::{Context, Result};
use std::{mem::size_of, os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND},
    System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ,
    },
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "Wowie";

pub(crate) fn set_run_at_sign_in(enabled: bool) -> Result<()> {
    let key_name = wide(RUN_KEY);
    let value_name = wide(VALUE_NAME);

    if enabled {
        let executable = std::env::current_exe().context("locate Wowie executable")?;
        if !executable.is_file() {
            anyhow::bail!("Wowie executable does not exist: {}", executable.display());
        }
        let command = quoted_executable(&executable);
        let byte_length = command
            .len()
            .checked_mul(size_of::<u16>())
            .and_then(|length| u32::try_from(length).ok())
            .context("Wowie executable path is too long for the Windows registry")?;

        let mut key: HKEY = std::ptr::null_mut();
        let result = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key_name.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if result != 0 {
            return Err(registry_error("create Wowie sign-in startup entry", result));
        }

        let result = unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                byte_length,
            )
        };
        unsafe { RegCloseKey(key) };
        if result != 0 {
            return Err(registry_error("write Wowie sign-in startup entry", result));
        }
        return Ok(());
    }

    let mut key: HKEY = std::ptr::null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        return Ok(());
    }
    if result != 0 {
        return Err(registry_error("open Wowie sign-in startup entry", result));
    }

    let result = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
    unsafe { RegCloseKey(key) };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if result != 0 {
        return Err(registry_error("remove Wowie sign-in startup entry", result));
    }
    Ok(())
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn quoted_executable(executable: &Path) -> Vec<u16> {
    let mut command = Vec::new();
    command.push(b'"' as u16);
    command.extend(executable.as_os_str().encode_wide());
    command.extend("\"".encode_utf16());
    command.push(0);
    command
}

fn registry_error(action: &str, code: u32) -> anyhow::Error {
    anyhow::anyhow!(
        "{action}: {}",
        std::io::Error::from_raw_os_error(code as i32)
    )
}
