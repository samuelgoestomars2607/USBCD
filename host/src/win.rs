//! Kleine Bausteine für Windows-Aufrufe: Zeichenketten (UTF-16), Handles und COM-Zeiger,
//! die beim Verwerfen von selbst freigegeben werden.

use std::ffi::c_void;
use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Windows-Handle (Datei, Pipe, Prozess, Ereignis …), das beim Verwerfen geschlossen wird.
/// Null und `INVALID_HANDLE_VALUE` gelten als „kein Handle“ und werden nicht geschlossen.
pub struct Handle(pub HANDLE);

impl Handle {
    pub fn is_valid(&self) -> bool {
        !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.is_valid() {
            // SAFETY: Das Handle gehört nur diesem Wert und wird genau einmal geschlossen.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Eigentümer eines COM-Zeigers. Ruft Methoden über die Vtable auf (ohne COM-Bibliothek)
/// und gibt das Objekt beim Verwerfen frei (`Release`). Null = kein Objekt.
pub struct ComPtr(pub *mut c_void);

impl ComPtr {
    /// Methode Nummer `idx` der Vtable (0 QueryInterface, 1 AddRef, 2 Release, dann die
    /// Methoden der Schnittstelle in der Reihenfolge ihrer Deklaration).
    ///
    /// # Safety
    /// `self.0` zeigt auf ein lebendes COM-Objekt, die Vtable hat mindestens `idx + 1`
    /// Einträge, und `F` ist genau die Signatur dieser Methode (`extern "system"`, erstes
    /// Argument der Objektzeiger).
    pub unsafe fn f<F: Copy>(&self, idx: usize) -> F {
        let vtbl = *(self.0 as *const *const usize);
        let p = *vtbl.add(idx);
        std::mem::transmute_copy::<usize, F>(&p)
    }

    /// Andere Schnittstelle desselben Objekts (QueryInterface).
    ///
    /// # Safety
    /// `self.0` zeigt auf ein lebendes COM-Objekt.
    pub unsafe fn query(&self, iid: &GUID) -> Option<ComPtr> {
        let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
            self.f(0);
        let mut p = std::ptr::null_mut();
        (qi(self.0, iid, &mut p) >= 0 && !p.is_null()).then_some(ComPtr(p))
    }
}

impl Drop for ComPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Wir halten genau eine Referenz auf das Objekt und geben sie einmal frei.
            unsafe {
                let release: unsafe extern "system" fn(*mut c_void) -> u32 = self.f(2);
                release(self.0);
            }
        }
    }
}
