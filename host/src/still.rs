//! Standbilder verlustfrei („build to lossless“, Menü „Bildqualität“).
//!
//! Solange sich etwas bewegt, läuft das Video wie immer. Steht das Bild still, schickt
//! USBCD ein pixelgenaues Bild des Tablet-Monitors hinterher (RGBA, zlib-komprimiert);
//! die App legt es über das Video, bis das nächste Videobild kommt. Stehende Inhalte sind
//! dann exakt – Farbwerte wie am PC, volle Farbauflösung (kein Farbsaum an roter oder
//! blauer Schrift), keine Kompressionsspuren.
//!
//! Nachricht STILL: [Breite u32][Höhe u32][Format u8 = 1: RGBA][zlib-Daten]

use std::time::{Duration, Instant};

pub const FORMAT_RGBA_ZLIB: u8 = 1;

/// So lange muss das Bild stillstehen, bevor ein Standbild kommt.
const IDLE: Duration = Duration::from_millis(300);
/// Kam die nächste Änderung schneller als das (z. B. blinkender Text-Cursor), wartet der
/// nächste Versuch länger – sonst wechselte das Bild ständig zwischen scharf und weich.
const SHORT_LIVED: Duration = Duration::from_millis(1500);
const IDLE_BACKOFF: Duration = Duration::from_secs(4);

/// Ablauf je Übertragung (läuft im Video-Thread).
pub struct Stills {
    enabled: bool,
    hmonitor: u64,
    width: u32,
    height: u32,
    cursor: bool,
    /// zuletzt gesendetes Videobild
    last_frame: Instant,
    /// fortlaufende Nummer der Videobilder (ein Standbild gilt nur bis zum nächsten)
    generation: u64,
    /// laufende Aufnahme: (Nummer beim Start, Ergebnis)
    pending: Option<(u64, std::sync::mpsc::Receiver<Option<Vec<u8>>>)>,
    /// liegt gerade ein Standbild auf dem Tablet? (seit wann)
    shown: Option<Instant>,
    idle: Duration,
}

impl Stills {
    pub fn new(enabled: bool, hmonitor: u64, width: u32, height: u32, cursor: bool) -> Stills {
        if enabled {
            crate::log!("Standbilder verlustfrei: an");
        }
        Stills {
            enabled,
            hmonitor,
            width,
            height,
            cursor,
            last_frame: Instant::now(),
            generation: 0,
            pending: None,
            shown: None,
            idle: IDLE,
        }
    }

    /// Wartet gerade eine Aufnahme auf ihr Ergebnis? (dann öfter nachsehen)
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Ein Videobild wurde gesendet – ein liegendes Standbild verschwindet damit auf dem Tablet.
    pub fn on_frame(&mut self) {
        self.generation += 1;
        self.last_frame = Instant::now();
        if let Some(t) = self.shown.take() {
            self.idle = if t.elapsed() < SHORT_LIVED {
                IDLE_BACKOFF
            } else {
                IDLE
            };
        }
    }

    /// Regelmäßig aufrufen: startet bei Stillstand eine Aufnahme und liefert das fertige
    /// Standbild, solange seitdem kein neues Videobild kam.
    pub fn poll(&mut self) -> Option<Vec<u8>> {
        if !self.enabled {
            return None;
        }
        if let Some((generation, rx)) = &self.pending {
            match rx.try_recv() {
                Ok(Some(data)) => {
                    let current = *generation == self.generation;
                    self.pending = None;
                    if current {
                        self.shown = Some(Instant::now());
                        return Some(data);
                    }
                }
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.enabled = false;
                    crate::log!("Standbilder verlustfrei: Aufnahme klappt nicht – für diese Übertragung aus");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            return None;
        }
        if self.shown.is_none() && self.last_frame.elapsed() >= self.idle {
            let (tx, rx) = std::sync::mpsc::channel();
            let (hm, w, h, cursor) = (self.hmonitor, self.width, self.height, self.cursor);
            std::thread::spawn(move || {
                let _ = tx.send(capture(hm, w, h, cursor).map(|rgba| encode(w, h, &rgba)));
            });
            self.pending = Some((self.generation, rx));
        }
        None
    }
}

/// Nachricht STILL zusammensetzen (schnelle Kompression: Bildschirminhalte schrumpfen stark).
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(rgba, 1);
    let mut v = Vec::with_capacity(9 + z.len());
    v.extend_from_slice(&width.to_be_bytes());
    v.extend_from_slice(&height.to_be_bytes());
    v.push(FORMAT_RGBA_ZLIB);
    v.extend_from_slice(&z);
    v
}

/// BGRX (Windows) → RGBA, deckend.
pub fn bgrx_to_rgba(bgrx: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bgrx.len());
    for px in bgrx.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    out
}

/// Bildschirmfoto des Tablet-Monitors (mit Mauszeiger, wenn er im Video auch zu sehen ist).
/// None, wenn der Monitor nicht (mehr) genau diese Größe hat oder Windows nichts liefert.
#[cfg(windows)]
fn capture(hmonitor: u64, width: u32, height: u32, cursor: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC,
        GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, HGDIOBJ, HMONITOR, MONITORINFO, SRCCOPY,
    };
    unsafe {
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hmonitor as HMONITOR, &mut mi) == 0 {
            return None;
        }
        let r = mi.rcMonitor;
        if (r.right - r.left) as u32 != width || (r.bottom - r.top) as u32 != height {
            return None;
        }
        let (w, h) = (width as i32, height as i32);
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // von oben nach unten
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(mem, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        let mut result = None;
        if !mem.is_null() && !bmp.is_null() && !bits.is_null() {
            let old = SelectObject(mem, bmp as HGDIOBJ);
            if BitBlt(mem, 0, 0, w, h, screen, r.left, r.top, SRCCOPY | CAPTUREBLT) != 0 {
                if cursor {
                    draw_cursor(mem, r.left, r.top);
                }
                GdiFlush();
                let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                result = Some(bgrx_to_rgba(px));
            }
            SelectObject(mem, old);
        }
        if !bmp.is_null() {
            DeleteObject(bmp as HGDIOBJ);
        }
        if !mem.is_null() {
            DeleteDC(mem);
        }
        ReleaseDC(std::ptr::null_mut(), screen);
        result
    }
}

/// Mauszeiger an seiner Stelle einzeichnen (wie im Video; auch invertierende Zeiger).
#[cfg(windows)]
unsafe fn draw_cursor(dc: windows_sys::Win32::Graphics::Gdi::HDC, left: i32, top: i32) {
    use windows_sys::Win32::Graphics::Gdi::DeleteObject;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, ICONINFO,
    };
    let mut ci: CURSORINFO = std::mem::zeroed();
    ci.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
    if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 || ci.hCursor.is_null() {
        return;
    }
    let mut ii: ICONINFO = std::mem::zeroed();
    if GetIconInfo(ci.hCursor, &mut ii) == 0 {
        return;
    }
    let (hx, hy) = (ii.xHotspot as i32, ii.yHotspot as i32);
    if !ii.hbmMask.is_null() {
        DeleteObject(ii.hbmMask as _);
    }
    if !ii.hbmColor.is_null() {
        DeleteObject(ii.hbmColor as _);
    }
    DrawIconEx(
        dc,
        ci.ptScreenPos.x - left - hx,
        ci.ptScreenPos.y - top - hy,
        ci.hCursor,
        0,
        0,
        0,
        std::ptr::null_mut(),
        DI_NORMAL,
    );
}

#[cfg(not(windows))]
fn capture(_hmonitor: u64, _width: u32, _height: u32, _cursor: bool) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_and_encodes() {
        assert_eq!(
            bgrx_to_rgba(&[1, 2, 3, 0, 9, 8, 7, 0]),
            [3, 2, 1, 255, 7, 8, 9, 255]
        );
        let rgba = vec![200u8; 4 * 4 * 4];
        let msg = encode(4, 4, &rgba);
        assert_eq!(&msg[..9], &[0, 0, 0, 4, 0, 0, 0, 4, FORMAT_RGBA_ZLIB]);
        let back = miniz_oxide::inflate::decompress_to_vec_zlib(&msg[9..]).unwrap();
        assert_eq!(back, rgba);
    }

    #[test]
    fn backs_off_when_changes_follow_quickly() {
        let mut s = Stills::new(true, 0, 4, 4, false);
        assert_eq!(s.idle, IDLE);
        s.shown = Some(Instant::now());
        s.on_frame(); // gleich wieder geändert (blinkender Cursor)
        assert_eq!(s.idle, IDLE_BACKOFF);
        s.shown = Some(Instant::now() - SHORT_LIVED - Duration::from_millis(1));
        s.on_frame();
        assert_eq!(s.idle, IDLE);
    }
}
