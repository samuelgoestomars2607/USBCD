//! Hintergrundbild beim An- und Abmelden des Tablet-Monitors erhalten.
//!
//! Ändert sich die Monitor-Anordnung, verliert Windows manchmal das Hintergrundbild
//! (schwarzer Desktop) – das passiert mit vielen virtuellen Monitoren. USBCD merkt
//! sich deshalb vor dem Einschalten des Tablet-Monitors das Bild jedes Bildschirms
//! und setzt es danach über die offizielle Windows-Schnittstelle (IDesktopWallpaper)
//! wieder. Der Tablet-Monitor bekommt das Bild des Hauptbildschirms.
//! Diashow, Windows-Blickpunkt und einfarbige Hintergründe bleiben unangetastet.

use crate::win::{wide, ComPtr};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::Mutex;
use windows_sys::core::{GUID, HRESULT, PCWSTR, PWSTR};
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED,
};

const CLSID_DESKTOP_WALLPAPER: GUID = GUID::from_u128(0xc2cf3110_460e_4fc1_b9d0_8a1c0c9cc4bd);
const IID_IDESKTOP_WALLPAPER: GUID = GUID::from_u128(0xb92b56a9_8b55_4e14_9a89_0199bbb6f93b);
const DSS_SLIDESHOW: i32 = 2;

/// Bild je Monitor (Geräte-Pfad → Bilddatei) vor dem Einschalten des Tablet-Monitors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub per_monitor: BTreeMap<String, String>,
}

static LAST: Mutex<Option<Snapshot>> = Mutex::new(None);

/// IDesktopWallpaper (COM-Zeiger siehe win.rs).
struct Wp(ComPtr);
impl std::ops::Deref for Wp {
    type Target = ComPtr;
    fn deref(&self) -> &ComPtr {
        &self.0
    }
}
impl Wp {
    unsafe fn new() -> Option<Wp> {
        let mut p = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_DESKTOP_WALLPAPER,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IDESKTOP_WALLPAPER,
            &mut p,
        );
        (hr >= 0 && !p.is_null()).then_some(Wp(ComPtr(p)))
    }
    unsafe fn take(s: PWSTR) -> String {
        if s.is_null() {
            return String::new();
        }
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        let r = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
        CoTaskMemFree(s as *const c_void);
        r
    }
    /// Aktive Monitore (Geräte-Pfad).
    unsafe fn monitors(&self) -> Vec<String> {
        let count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT = self.f(6);
        let at: unsafe extern "system" fn(*mut c_void, u32, *mut PWSTR) -> HRESULT = self.f(5);
        let rect: unsafe extern "system" fn(*mut c_void, PCWSTR, *mut RECT) -> HRESULT = self.f(7);
        let mut n = 0u32;
        if count(self.0 .0, &mut n) < 0 {
            return Vec::new();
        }
        let mut v = Vec::new();
        for i in 0..n.min(16) {
            let mut p: PWSTR = std::ptr::null_mut();
            if at(self.0 .0, i, &mut p) >= 0 {
                let id = Self::take(p);
                let w = wide(&id);
                let mut r: RECT = std::mem::zeroed();
                // nur Monitore, die gerade zum Desktop gehören
                if rect(self.0 .0, w.as_ptr(), &mut r) == 0 && r.right > r.left {
                    v.push(id);
                }
            }
        }
        v
    }
    unsafe fn get(&self, monitor: &str) -> String {
        let get: unsafe extern "system" fn(*mut c_void, PCWSTR, *mut PWSTR) -> HRESULT = self.f(4);
        let w = wide(monitor);
        let mut p: PWSTR = std::ptr::null_mut();
        if get(self.0 .0, w.as_ptr(), &mut p) >= 0 {
            Self::take(p)
        } else {
            String::new()
        }
    }
    unsafe fn set(&self, monitor: &str, path: &str) -> bool {
        let set: unsafe extern "system" fn(*mut c_void, PCWSTR, PCWSTR) -> HRESULT = self.f(3);
        let (m, p) = (wide(monitor), wide(path));
        set(self.0 .0, m.as_ptr(), p.as_ptr()) >= 0
    }
    unsafe fn slideshow(&self) -> bool {
        let status: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT = self.f(17);
        let mut s = 0i32;
        status(self.0 .0, &mut s) >= 0 && s & DSS_SLIDESHOW != 0
    }
}
/// In einem eigenen STA-Thread ausführen (Shell-COM).
fn on_sta<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    std::thread::spawn(move || unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let r = f();
        CoUninitialize();
        r
    })
    .join()
    .ok()
}

/// Windows-Blickpunkt (Spotlight) als Desktop-Hintergrund?
fn spotlight() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\DesktopSpotlight\Settings");
    let val = wide("EnabledState");
    let mut v = 0u32;
    let mut len = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut v as *mut u32 as *mut c_void,
            &mut len,
        ) == 0
            && v != 0
    }
}

fn usable(p: &str) -> bool {
    !p.trim().is_empty() && std::path::Path::new(p).is_file()
}

/// Bild je Monitor merken (vor dem Einschalten des Tablet-Monitors).
pub fn remember() {
    let snap = on_sta(|| unsafe {
        let wp = Wp::new()?;
        let mut s = Snapshot::default();
        for m in wp.monitors() {
            let p = wp.get(&m);
            if usable(&p) {
                s.per_monitor.insert(m, p);
            }
        }
        Some(s)
    })
    .flatten();
    if let Some(s) = snap {
        if !s.per_monitor.is_empty() {
            if let Ok(mut g) = LAST.lock() {
                *g = Some(s);
            }
        }
    }
}

/// Was nach einer Änderung der Anordnung zu setzen ist: (Monitor, Bild).
/// Monitore mit fehlendem Bild bekommen ihr gemerktes Bild bzw. das des ersten
/// Monitors; mit `refresh` werden auch vorhandene Bilder neu gesetzt (Windows zeichnet
/// sie dann neu, falls nur die Anzeige schwarz geblieben ist).
pub fn plan(
    snap: &Snapshot,
    current: &[(String, String)],
    refresh: bool,
    exists: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let fallback = snap
        .per_monitor
        .values()
        .find(|p| exists(p))
        .cloned()
        .or_else(|| current.iter().map(|(_, p)| p).find(|p| exists(p)).cloned());
    let Some(fallback) = fallback else {
        return Vec::new(); // einfarbiger Hintergrund o. Ä. – nichts anfassen
    };
    let mut v = Vec::new();
    for (m, p) in current {
        if exists(p) {
            if refresh {
                v.push((m.clone(), p.clone()));
            }
        } else {
            let want = snap
                .per_monitor
                .get(m)
                .filter(|s| exists(s))
                .cloned()
                .unwrap_or_else(|| fallback.clone());
            v.push((m.clone(), want));
        }
    }
    v
}

/// Nach dem Ein- oder Ausschalten des Tablet-Monitors aufrufen (wartet kurz, bis
/// Windows die neue Anordnung übernommen hat).
pub fn restore_later() {
    let Some(snap) = LAST.lock().ok().and_then(|g| g.clone()) else {
        return;
    };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        if spotlight() {
            return;
        }
        let done = on_sta(move || unsafe {
            let wp = Wp::new()?;
            if wp.slideshow() {
                return Some(0);
            }
            let current: Vec<(String, String)> = wp
                .monitors()
                .into_iter()
                .map(|m| (m.clone(), wp.get(&m)))
                .collect();
            let mut n = 0;
            for (m, p) in plan(&snap, &current, true, usable) {
                if wp.set(&m, &p) {
                    n += 1;
                }
            }
            Some(n)
        })
        .flatten();
        if let Some(n) = done.filter(|n| *n > 0) {
            crate::log!("Hintergrundbild auf {n} Bildschirm(en) wiederhergestellt");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(v: &[(&str, &str)]) -> Snapshot {
        Snapshot {
            per_monitor: v
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }
    fn cur(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }
    fn exists(p: &str) -> bool {
        p.ends_with(".jpg")
    }

    #[test]
    fn fills_black_monitors() {
        let s = snap(&[("main", "a.jpg")]);
        let c = cur(&[("main", ""), ("tablet", "")]);
        assert_eq!(
            plan(&s, &c, false, exists),
            cur(&[("main", "a.jpg"), ("tablet", "a.jpg")])
        );
    }

    #[test]
    fn keeps_user_changes_and_refreshes() {
        // Nutzer hat inzwischen ein neues Bild gewählt: bleibt, wird nur neu gesetzt
        let s = snap(&[("main", "a.jpg")]);
        let c = cur(&[("main", "b.jpg"), ("tablet", "")]);
        assert_eq!(
            plan(&s, &c, true, exists),
            cur(&[("main", "b.jpg"), ("tablet", "a.jpg")])
        );
        assert_eq!(plan(&s, &c, false, exists), cur(&[("tablet", "a.jpg")]));
    }

    #[test]
    fn solid_color_untouched() {
        let s = Snapshot::default();
        let c = cur(&[("main", ""), ("tablet", "")]);
        assert!(plan(&s, &c, true, exists).is_empty());
    }
}
