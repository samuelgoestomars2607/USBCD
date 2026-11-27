//! Treiber-Installation für das Setup (wie `devcon install`): legt bei Bedarf ein
//! Root-Gerät mit der Hardware-ID an und installiert den Treiber aus der INF.
//!
//!   USBCD.exe --install-driver <pfad\zur.inf> <Hardware-ID>
//!   USBCD.exe --remove-device <Hardware-ID>

use crate::util::{from_wide, wide};
use anyhow::{bail, Result};
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW, SetupDiSetDeviceRegistryPropertyW,
    UpdateDriverForPlugAndPlayDevicesW, DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE,
    DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, INSTALLFLAG_FORCE, SPDRP_HARDWAREID,
    SP_DEVINFO_DATA,
};

const MAX_CLASS_NAME_LEN: usize = 32;

unsafe fn hardware_ids(devs: HDEVINFO, info: &mut SP_DEVINFO_DATA) -> Vec<String> {
    let mut buf = vec![0u16; 2048];
    let mut ty = 0u32;
    let mut needed = 0u32;
    if SetupDiGetDeviceRegistryPropertyW(
        devs,
        info,
        SPDRP_HARDWAREID,
        &mut ty,
        buf.as_mut_ptr() as *mut u8,
        (buf.len() * 2) as u32,
        &mut needed,
    ) == 0
    {
        return Vec::new();
    }
    buf.split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Ruft `f` für jedes vorhandene Gerät mit passender Hardware-ID auf.
pub(crate) fn for_each_device(
    hwid: &str,
    mut f: impl FnMut(HDEVINFO, &mut SP_DEVINFO_DATA),
) -> usize {
    let mut n = 0;
    unsafe {
        let devs =
            SetupDiGetClassDevsW(null(), null(), null_mut(), DIGCF_ALLCLASSES | DIGCF_PRESENT);
        if devs == INVALID_HANDLE_VALUE as HDEVINFO {
            return 0;
        }
        let mut i = 0;
        loop {
            let mut info: SP_DEVINFO_DATA = zeroed();
            info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(devs, i, &mut info) == 0 {
                break;
            }
            i += 1;
            if hardware_ids(devs, &mut info)
                .iter()
                .any(|h| h.eq_ignore_ascii_case(hwid))
            {
                n += 1;
                f(devs, &mut info);
            }
        }
        SetupDiDestroyDeviceInfoList(devs);
    }
    n
}

const INVALID_HANDLE_VALUE: isize = -1;

pub fn device_exists(hwid: &str) -> bool {
    for_each_device(hwid, |_, _| {}) > 0
}

/// Nur diese Hardware-IDs darf USBCD installieren/entfernen – so kann
/// nie versehentlich ein fremder Treiber (z. B. Parsec) betroffen sein.
const ALLOWED: &[&str] = &[crate::vdd::HWID];

fn allowed(hwid: &str) -> bool {
    ALLOWED.iter().any(|a| a.eq_ignore_ascii_case(hwid))
}

pub fn install(inf: &str, hwid: &str) -> Result<bool> {
    if !allowed(hwid) {
        bail!("Hardware-ID {hwid} ist nicht freigegeben");
    }
    let inf_path = std::fs::canonicalize(inf)
        .map(|p| p.to_string_lossy().trim_start_matches(r"\\?\").to_string())
        .unwrap_or_else(|_| inf.to_string());
    let inf_w = wide(&inf_path);
    let hwid_w = wide(hwid);
    unsafe {
        if !device_exists(hwid) {
            let mut class_guid: GUID = zeroed();
            let mut class_name = [0u16; MAX_CLASS_NAME_LEN];
            if SetupDiGetINFClassW(
                inf_w.as_ptr(),
                &mut class_guid,
                class_name.as_mut_ptr(),
                MAX_CLASS_NAME_LEN as u32,
                null_mut(),
            ) == 0
            {
                bail!("INF nicht lesbar: {inf_path}");
            }
            let devs = SetupDiCreateDeviceInfoList(&class_guid, null_mut());
            if devs == INVALID_HANDLE_VALUE as HDEVINFO {
                bail!("SetupDiCreateDeviceInfoList fehlgeschlagen");
            }
            let mut info: SP_DEVINFO_DATA = zeroed();
            info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            let ok = SetupDiCreateDeviceInfoW(
                devs,
                class_name.as_ptr(),
                &class_guid,
                null(),
                null_mut(),
                DICD_GENERATE_ID,
                &mut info,
            ) != 0
                && {
                    // REG_MULTI_SZ: ID + doppelte Null
                    let mut multi: Vec<u16> = hwid.encode_utf16().collect();
                    multi.extend_from_slice(&[0, 0]);
                    SetupDiSetDeviceRegistryPropertyW(
                        devs,
                        &mut info,
                        SPDRP_HARDWAREID,
                        multi.as_ptr() as *const u8,
                        (multi.len() * 2) as u32,
                    ) != 0
                }
                && SetupDiCallClassInstaller(DIF_REGISTERDEVICE, devs, &info) != 0;
            SetupDiDestroyDeviceInfoList(devs);
            if !ok {
                bail!(
                    "Gerät {hwid} ({}) konnte nicht angelegt werden",
                    from_wide(&class_name)
                );
            }
        }
        let mut reboot = 0;
        if UpdateDriverForPlugAndPlayDevicesW(
            null_mut(),
            hwid_w.as_ptr(),
            inf_w.as_ptr(),
            INSTALLFLAG_FORCE,
            &mut reboot,
        ) == 0
        {
            bail!(
                "Treiberinstallation für {hwid} fehlgeschlagen (Fehler {})",
                std::io::Error::last_os_error()
            );
        }
        Ok(reboot != 0)
    }
}

pub fn remove(hwid: &str) -> usize {
    if !allowed(hwid) {
        return 0;
    }
    for_each_device(hwid, |devs, info| unsafe {
        SetupDiCallClassInstaller(DIF_REMOVE, devs, info);
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_vdd_allowed() {
        assert!(super::allowed(r"ROOT\MTTVDD"));
        assert!(!super::allowed(r"Root\Parsec\VDA"));
        assert!(!super::allowed("Root\\Display"));
    }
}
