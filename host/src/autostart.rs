//! Autostart beim Anmelden über den Registry-Eintrag
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Run\USBCD.
//! USBCD startet damit ganz normal ohne Administratorrechte.

use crate::util::wide;
use anyhow::{bail, Result};
use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
    RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Hier merkt sich Windows, ob ein Autostart im Task-Manager deaktiviert wurde.
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const VALUE: &str = "USBCD";

pub fn command_line() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("\"{}\"", exe.to_string_lossy().trim_start_matches(r"\\?\"))
}

/// Ist der Autostart eingerichtet und nicht im Task-Manager abgeschaltet?
pub fn enabled() -> bool {
    let (key, val) = (wide(RUN), wide(VALUE));
    let mut size = 0u32;
    let present = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        )
    } == ERROR_SUCCESS;
    if !present {
        return false;
    }
    let ak = wide(APPROVED);
    let mut data = [0u8; 12];
    let mut len = data.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            ak.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            data.as_mut_ptr() as *mut c_void,
            &mut len,
        )
    };
    // ungerades erstes Byte = im Task-Manager deaktiviert
    r != ERROR_SUCCESS || len == 0 || data[0] & 1 == 0
}

pub fn enable() -> Result<()> {
    let (key, val) = (wide(RUN), wide(VALUE));
    let cmd = wide(&command_line());
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            REG_SZ,
            cmd.as_ptr() as *const c_void,
            (cmd.len() * 2) as u32,
        )
    };
    if r != ERROR_SUCCESS {
        bail!("Autostart-Eintrag nicht anlegbar (Fehler {r})");
    }
    // eine frühere Abschaltung im Task-Manager aufheben
    let ak = wide(APPROVED);
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, ak.as_ptr(), val.as_ptr()) };
    crate::log!("Autostart eingerichtet: {}", command_line());
    Ok(())
}

pub fn disable() {
    let (key, val) = (wide(RUN), wide(VALUE));
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr()) };
    crate::log!("Autostart entfernt");
}
