//! Shared Windows COM scopes and UTF-16 path helpers.

use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::Path};

const RPC_E_CHANGED_MODE: u32 = 0x8001_0106;

pub(super) struct ComScope(bool);

pub(super) struct WinRtScope(bool);

impl WinRtScope {
    pub(super) fn initialize() -> Option<Self> {
        let result = unsafe { co_initialize_sta() };
        if result >= 0 {
            Some(Self(true))
        } else if result as u32 == RPC_E_CHANGED_MODE {
            Some(Self(false))
        } else {
            None
        }
    }
}

impl Drop for WinRtScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe { co_uninitialize() };
        }
    }
}

impl ComScope {
    pub(super) fn initialize() -> Option<Self> {
        let result = unsafe { co_initialize_sta() };
        if result >= 0 {
            Some(Self(true))
        } else if result as u32 == RPC_E_CHANGED_MODE {
            Some(Self(false))
        } else {
            None
        }
    }
}

unsafe fn co_initialize_sta() -> i32 {
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *const std::ffi::c_void, init_type: u32) -> i32;
    }
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED) }
}

unsafe fn co_uninitialize() {
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoUninitialize();
    }
    unsafe { CoUninitialize() };
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            unsafe { co_uninitialize() };
        }
    }
}

pub(super) fn wide_path(path: &Path) -> Vec<u16> {
    OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
