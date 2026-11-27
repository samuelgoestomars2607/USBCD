//! Symbol im Infobereich der Taskleiste mit Menü.
//! Läuft im Hauptthread; die Tablet-Erkennung arbeitet in einem eigenen Thread.

use crate::i18n::tr;
use crate::state::set_shutdown;
use crate::state::status;
use crate::win::wide;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_LARGE_ICON,
    NIIF_NOSOUND, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, LoadIconW, LoadImageW,
    MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SendMessageTimeoutW, SetForegroundWindow, SetTimer, TrackPopupMenu, TranslateMessage, HICON,
    HMENU, IDI_APPLICATION, IDYES, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MB_ICONINFORMATION,
    MB_SETFOREGROUND, MB_YESNO, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    SMTO_ABORTIFHUNG, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND,
    WM_CONTEXTMENU, WM_COPYDATA, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONUP, WM_NULL,
    WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASSW, WS_OVERLAPPED,
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW_INFO: u32 = WM_APP + 2;
const WM_NOTE: u32 = WM_APP + 3;
const TIMER_ID: usize = 1;

const ID_STATUS: usize = 1;
const ID_SOUND: usize = 2;
const ID_DISPLAY: usize = 3;
const ID_FOLDER: usize = 4;
const ID_QUIT: usize = 5;
const ID_LAUNCH: usize = 6;
const ID_ABOUT: usize = 7;

static HWND_G: AtomicUsize = AtomicUsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static ICON: AtomicUsize = AtomicUsize::new(0);
static BIG_ICON: AtomicUsize = AtomicUsize::new(0);
static PENDING: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

/// Ersatz, wenn Windows-Benachrichtigungen nicht gehen: Info-Blase mit USBCD-Logo
/// (aus beliebigem Thread; wird angezeigt, sobald das Symbol existiert).
pub fn balloon_fallback(title: &str, text: &str) {
    if let Ok(mut g) = PENDING.lock() {
        g.push((title.to_string(), text.to_string()));
    }
    let h = HWND_G.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_NOTE, 0, 0);
        }
    }
}

fn copy_wstr(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

unsafe fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut n: NOTIFYICONDATAW = zeroed();
    n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    n.hWnd = hwnd;
    n.uID = 1;
    n
}

unsafe fn add_icon(hwnd: HWND) {
    let mut n = nid(hwnd);
    n.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    n.uCallbackMessage = WM_TRAY;
    n.hIcon = ICON.load(Ordering::Relaxed) as HICON;
    copy_wstr(&mut n.szTip, &tooltip());
    Shell_NotifyIconW(NIM_ADD, &n);
}

fn tooltip() -> String {
    format!("USBCD – {}", status())
}

unsafe fn update_tip(hwnd: HWND) {
    // nur bei Änderung an Windows geben (läuft jede Sekunde)
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let tip = tooltip();
    if let Ok(mut last) = LAST.lock() {
        if *last == tip {
            return;
        }
        *last = tip.clone();
    }
    let mut n = nid(hwnd);
    n.uFlags = NIF_TIP;
    copy_wstr(&mut n.szTip, &tip);
    Shell_NotifyIconW(NIM_MODIFY, &n);
}

unsafe fn balloon(hwnd: HWND, title: &str, text: &str) {
    let mut n = nid(hwnd);
    n.uFlags = NIF_INFO;
    // eigenes Logo statt des blauen Windows-„i“
    let big = BIG_ICON.load(Ordering::Relaxed);
    if big != 0 {
        n.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON | NIIF_NOSOUND;
        n.hBalloonIcon = big as HICON;
    } else {
        n.dwInfoFlags = NIIF_USER | NIIF_NOSOUND;
    }
    copy_wstr(&mut n.szInfoTitle, title);
    copy_wstr(&mut n.szInfo, text);
    Shell_NotifyIconW(NIM_MODIFY, &n);
}

/// Version, Copyright, Lizenz und Gewährleistungsausschluss (GPL: „Appropriate Legal
/// Notices“). Eigener Thread, damit das Fenster das Tray-Menü nicht aufhält; mehrfaches
/// Öffnen holt nur das vorhandene nach vorn.
fn show_about() {
    use std::sync::atomic::AtomicBool;
    static OPEN: AtomicBool = AtomicBool::new(false);
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let text = format!(
            "USBCD {}\nCopyright © 2026 Samuel E. Ramirez\n\n{}\n\n{}\n\n{}\ngithub.com/samuelgoestomars2607/USBCD\n\n{}",
            env!("CARGO_PKG_VERSION"),
            tr(
                "USBCD ist freie Software unter der GNU General Public License v3.0 oder später. \
                 Du darfst es benutzen, weitergeben und verändern.",
                "USBCD is free software under the GNU General Public License v3.0 or later. \
                 You may use, share and change it.",
            ),
            tr(
                "USBCD wird ohne jede Gewährleistung bereitgestellt, soweit das Gesetz es zulässt.",
                "USBCD comes with no warranty, to the extent permitted by law.",
            ),
            tr("Quellcode:", "Source code:"),
            tr(
                "Lizenztexte (auch der mitgelieferten Komponenten) jetzt öffnen?",
                "Open the license texts (including the bundled components) now?",
            ),
        );
        let (t, c) = (wide(&text), wide(tr("Über USBCD", "About USBCD")));
        let r = unsafe {
            MessageBoxW(
                null_mut(),
                t.as_ptr(),
                c.as_ptr(),
                MB_YESNO | MB_ICONINFORMATION | MB_SETFOREGROUND,
            )
        };
        if r == IDYES {
            if let Some(dir) = std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
            {
                for f in ["LICENSE.txt", "THIRD-PARTY.txt"] {
                    let p = dir.join(f);
                    if p.exists() {
                        open(&p.to_string_lossy());
                    }
                }
            }
        }
        OPEN.store(false, Ordering::SeqCst);
    });
}

fn open(target: &str) {
    let op = wide("open");
    let t = wide(target);
    unsafe {
        ShellExecuteW(
            null_mut(),
            op.as_ptr(),
            t.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}

// Menü-IDs der Einstellungen
const ID_AUDIO: usize = 10;
const ID_Q_LOW: usize = 20;
const ID_Q_STD: usize = 21;
const ID_Q_HIGH: usize = 22;
const ID_Q_MAX: usize = 23;
const ID_STILLS: usize = 24;
const ID_PEN_BASE: usize = 60; // 60..=62 → weich/linear/hart
const PEN_CURVES: [&str; 3] = ["soft", "linear", "hard"];
const ID_PENBTN_BASE: usize = 64; // 64..=65 → Rechtsklick/Radierer
const PEN_BUTTONS: [&str; 2] = ["right_click", "eraser"];
const ID_B_BASE: usize = 30; // 30..=33 → 100/75/50/25 %
const BRIGHTNESS: [u32; 4] = [100, 75, 50, 25];
const ID_AUTOSTART: usize = 40;
const ID_INPUT: usize = 50;
const ID_STATS: usize = 51;
const ID_PAUSE: usize = 52;
const ID_POWER: usize = 53;
const ID_WEBCAM: usize = 54;
const ID_DND: usize = 55;
const ID_VOLKEYS: usize = 56;
const ID_UPDATE: usize = 57;
const ID_UPDCHECK: usize = 58;
const ID_DEV_BASE: usize = 100; // 100.. → Geräte im Untermenü
const ID_DEV_NONE: usize = 99;
const COPYDATA_URI: usize = 0x5553_4244; // "USBD"

/// Geräte, wie sie im zuletzt geöffneten Menü standen: (Seriennummer, Name, neuer Wert)
static MENU_DEVICES: std::sync::Mutex<Vec<(String, String, bool)>> =
    std::sync::Mutex::new(Vec::new());

// Zustand von Bildschirm und Sperre (nur im Fenster-Thread geändert)
static DISPLAY_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn update_dark() {
    crate::state::set_pc_dark(
        DISPLAY_OFF.load(Ordering::Relaxed) || LOCKED.load(Ordering::Relaxed),
    );
}

static AUTOSTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Beim Start ermittelt.
pub fn set_autostart_known(on: bool) {
    AUTOSTART.store(on, Ordering::Relaxed);
}

unsafe fn append(m: HMENU, flags: u32, id: usize, text: &str) {
    let w = wide(text);
    AppendMenuW(
        m,
        flags,
        id,
        if flags & MF_SEPARATOR != 0 {
            null()
        } else {
            w.as_ptr()
        },
    );
}

fn check(on: bool) -> u32 {
    if on {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let mut pt: POINT = zeroed();
    GetCursorPos(&mut pt);
    let cfg = crate::config::current();

    let quality = CreatePopupMenu();
    let max = cfg.quality_max;
    append(
        quality,
        check(!max && cfg.bitrate_mbps > 0 && cfg.bitrate_mbps <= 12),
        ID_Q_LOW,
        tr("Sparsam (10 Mbit/s)", "Data saver (10 Mbit/s)"),
    );
    append(
        quality,
        check(!max && cfg.bitrate_mbps == 0),
        ID_Q_STD,
        tr("Standard (automatisch)", "Standard (automatic)"),
    );
    append(
        quality,
        check(!max && cfg.bitrate_mbps > 12),
        ID_Q_HIGH,
        tr("Hoch (40 Mbit/s)", "High (40 Mbit/s)"),
    );
    append(
        quality,
        check(max),
        ID_Q_MAX,
        tr(
            "Maximal (gleichbleibende Qualität, für starke PCs)",
            "Maximum (constant quality, for powerful PCs)",
        ),
    );
    append(quality, MF_SEPARATOR, 0, "");
    append(
        quality,
        check(cfg.lossless_stills),
        ID_STILLS,
        tr(
            "Standbilder verlustfrei (exakte Farben, für starke PCs)",
            "Lossless still images (exact colours, for powerful PCs)",
        ),
    );

    let pen = CreatePopupMenu();
    for (i, (curve, label)) in PEN_CURVES
        .iter()
        .zip([
            tr("Weich (wenig Kraft nötig)", "Soft (little force needed)"),
            tr("Linear (Standard)", "Linear (default)"),
            tr(
                "Hart (feiner bei leichtem Druck)",
                "Firm (finer at light pressure)",
            ),
        ])
        .enumerate()
    {
        append(
            pen,
            check(cfg.pen_pressure == *curve),
            ID_PEN_BASE + i,
            label,
        );
    }

    let pen_btn = CreatePopupMenu();
    for (i, (mode, label)) in PEN_BUTTONS
        .iter()
        .zip([
            tr(
                "Rechtsklick (Standard, auch per Klick in der Luft)",
                "Right-click (default, also by clicking in the air)",
            ),
            tr("Radierer, solange gedrückt", "Eraser while held"),
        ])
        .enumerate()
    {
        append(
            pen_btn,
            check(cfg.pen_button == *mode),
            ID_PENBTN_BASE + i,
            label,
        );
    }

    let bright = CreatePopupMenu();
    for (i, b) in BRIGHTNESS.iter().enumerate() {
        append(
            bright,
            check(cfg.tablet_brightness == *b),
            ID_B_BASE + i,
            &format!("{b} %"),
        );
    }

    // Geräte: neue zuerst, dann bekannte (Haken = als Bildschirm verwenden)
    let devices = CreatePopupMenu();
    let mut entries = Vec::new();
    for (serial, name) in crate::state::pending_devices() {
        if !cfg.devices.contains_key(&serial) {
            entries.push((
                format!(
                    "{} {name} – {}",
                    tr("Neu:", "New:"),
                    tr("als Bildschirm verwenden", "use as screen")
                ),
                MF_STRING,
                serial,
                name,
                true,
            ));
        }
    }
    for (serial, d) in &cfg.devices {
        let label = if d.use_as_display {
            d.name.clone()
        } else {
            format!("{} ({})", d.name, tr("nie verwenden", "never use"))
        };
        entries.push((
            label,
            check(d.use_as_display),
            serial.clone(),
            d.name.clone(),
            !d.use_as_display,
        ));
    }
    if entries.is_empty() {
        append(
            devices,
            MF_STRING | MF_GRAYED,
            ID_DEV_NONE,
            tr(
                "Noch keine Geräte angeschlossen",
                "No devices connected yet",
            ),
        );
    }
    if let Ok(mut g) = MENU_DEVICES.lock() {
        g.clear();
        for (i, (label, flags, serial, name, new_value)) in entries.into_iter().enumerate() {
            append(devices, flags, ID_DEV_BASE + i, &label);
            g.push((serial, name, new_value));
        }
    }

    let m = CreatePopupMenu();
    append(m, MF_STRING | MF_GRAYED, ID_STATUS, &status());
    if let Some(r) = crate::update::available() {
        // bleibt sichtbar, auch wenn die Benachrichtigung übersprungen wurde
        append(
            m,
            MF_STRING,
            ID_UPDATE,
            &tr("Update auf {v} installieren …", "Install update {v} …").replace("{v}", &r.version),
        );
    }
    append(m, MF_SEPARATOR, 0, "");
    append(
        m,
        MF_STRING,
        ID_LAUNCH,
        tr("Auf dem Tablet anzeigen", "Show on tablet"),
    );
    append(
        m,
        MF_STRING | MF_POPUP,
        devices as usize,
        tr("Geräte", "Devices"),
    );
    append(
        m,
        check(cfg.audio_enabled),
        ID_AUDIO,
        tr("Ton zum Tablet übertragen", "Send sound to tablet"),
    );
    append(
        m,
        check(cfg.tablet_input),
        ID_INPUT,
        tr("Touch und Stift am Tablet", "Touch and pen on the tablet"),
    );
    append(
        m,
        MF_STRING | MF_POPUP,
        pen as usize,
        tr("Stiftdruck", "Pen pressure"),
    );
    append(
        m,
        MF_STRING | MF_POPUP,
        pen_btn as usize,
        tr("Seitentaste des Stifts", "Pen side button"),
    );
    append(
        m,
        check(cfg.show_stats),
        ID_STATS,
        tr("Info-Anzeige auf dem Tablet", "Info overlay on the tablet"),
    );
    append(
        m,
        check(cfg.follow_pc_power),
        ID_POWER,
        tr(
            "Tablet mit dem PC-Bildschirm aus- und einschalten",
            "Turn tablet off and on with the PC screen",
        ),
    );
    append(
        m,
        check(cfg.webcam),
        ID_WEBCAM,
        tr(
            "Selfie-Kamera des Tablets als Webcam",
            "Tablet selfie camera as webcam",
        ),
    );
    append(
        m,
        check(cfg.tablet_dnd),
        ID_DND,
        tr(
            "„Nicht stören“ auf dem Tablet, solange es Monitor ist",
            "“Do not disturb” on the tablet while it is a monitor",
        ),
    );
    // wirkt nur, wenn der Ton zum Tablet geht
    let volkeys_flags = if cfg.audio_enabled && cfg.volume_mode != "off" {
        check(cfg.tablet_volume_keys)
    } else {
        check(cfg.tablet_volume_keys) | MF_GRAYED
    };
    append(
        m,
        volkeys_flags,
        ID_VOLKEYS,
        tr(
            "Lautstärketasten des Tablets regeln den Tablet-Lautsprecher",
            "Tablet volume buttons control the tablet speakers",
        ),
    );
    append(
        m,
        MF_STRING,
        ID_PAUSE,
        if crate::state::paused() {
            tr("Tablet wieder einschalten", "Switch tablet back on")
        } else {
            tr(
                "Tablet ausschalten (Bild und Ton)",
                "Switch tablet off (picture and sound)",
            )
        },
    );
    append(
        m,
        MF_STRING | MF_POPUP,
        quality as usize,
        tr("Bildqualität", "Picture quality"),
    );
    append(
        m,
        MF_STRING | MF_POPUP,
        bright as usize,
        tr("Tablet-Helligkeit", "Tablet brightness"),
    );
    append(m, MF_SEPARATOR, 0, "");
    append(
        m,
        MF_STRING,
        ID_SOUND,
        tr(
            "Soundeinstellungen (USBCD Tablet Lautsprecher wählen) …",
            "Sound settings (choose USBCD Tablet Speakers) …",
        ),
    );
    append(
        m,
        MF_STRING,
        ID_DISPLAY,
        tr("Anzeigeeinstellungen …", "Display settings …"),
    );
    append(
        m,
        MF_STRING,
        ID_FOLDER,
        tr("Einstellungen und Log öffnen", "Open settings and log"),
    );
    append(
        m,
        check(AUTOSTART.load(Ordering::Relaxed)),
        ID_AUTOSTART,
        tr(
            "Beim Anmelden automatisch starten",
            "Start automatically at sign-in",
        ),
    );
    append(
        m,
        check(cfg.update_check),
        ID_UPDCHECK,
        tr(
            "Automatisch nach Updates suchen",
            "Check for updates automatically",
        ),
    );
    append(m, MF_SEPARATOR, 0, "");
    append(m, MF_STRING, ID_ABOUT, tr("Über USBCD …", "About USBCD …"));
    append(m, MF_STRING, ID_QUIT, tr("USBCD beenden", "Quit USBCD"));

    apply_menu_theme(hwnd);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        m,
        TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(m); // zerstört auch die Untermenüs
}

/// Menü hell oder dunkel – passend zur Taskleiste (Einstellungen → Personalisierung → Farben →
/// „Windows-Standardmodus“). Klassische Windows-Menüs folgen dem nicht von selbst; Windows
/// bietet dafür nur nicht dokumentierte Funktionen in uxtheme.dll, die Explorer und viele
/// Taskleisten-Programme genauso nutzen. Fehlen sie, bleibt das Menü hell wie bisher.
/// Wird vor jedem Öffnen aufgerufen, damit ein Wechsel des Modus sofort wirkt.
unsafe fn apply_menu_theme(hwnd: HWND) {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    // Reihenfolge-Nummern in uxtheme.dll (seit Windows 10 1903 unverändert)
    const ALLOW_DARK_MODE_FOR_WINDOW: usize = 133;
    const SET_PREFERRED_APP_MODE: usize = 135;
    const FLUSH_MENU_THEMES: usize = 136;
    const FORCE_DARK: i32 = 2;
    const FORCE_LIGHT: i32 = 3;
    let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
    if lib.is_null() {
        return;
    }
    let (Some(allow), Some(mode), Some(flush)) = (
        GetProcAddress(lib, ALLOW_DARK_MODE_FOR_WINDOW as *const u8),
        GetProcAddress(lib, SET_PREFERRED_APP_MODE as *const u8),
        GetProcAddress(lib, FLUSH_MENU_THEMES as *const u8),
    ) else {
        return;
    };
    let allow: unsafe extern "system" fn(HWND, BOOL) -> BOOL = std::mem::transmute(allow);
    let mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(mode);
    let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
    let dark = system_uses_dark();
    allow(hwnd, dark as BOOL);
    mode(if dark { FORCE_DARK } else { FORCE_LIGHT });
    flush();
}

/// Lädt das Symbol für den Infobereich passend zur Taskleiste: nur das Croissant, auf
/// dunkler Taskleiste mit Verlauf, auf heller in Dunkelgrau (der helle Verlauf wäre dort
/// kaum zu sehen). Liefert true, wenn sich das Symbol geändert hat.
unsafe fn load_tray_icon() -> bool {
    // 0 = noch nicht geladen, 1 = dunkel, 2 = hell
    static MODE: AtomicU32 = AtomicU32::new(0);
    let mode = if system_uses_dark() { 1 } else { 2 };
    if MODE.swap(mode, Ordering::Relaxed) == mode {
        return false;
    }
    let dir = crate::config::exe_dir();
    let mut path = dir.join(if mode == 1 {
        "USBCD-tray.ico"
    } else {
        "USBCD-tray-light.ico"
    });
    if !path.exists() {
        path = dir.join("USBCD.ico");
    }
    let ico_path = wide(&path.to_string_lossy());
    let mut icon = LoadImageW(
        null_mut(),
        ico_path.as_ptr(),
        IMAGE_ICON,
        0,
        0,
        LR_LOADFROMFILE | LR_DEFAULTSIZE,
    );
    let fallback = LoadIconW(null_mut(), IDI_APPLICATION);
    if icon.is_null() {
        icon = fallback;
    }
    let old = ICON.swap(icon as usize, Ordering::Relaxed);
    // aus Datei geladene Symbole freigeben, das geteilte Windows-Symbol nicht
    if old != 0 && old != fallback as usize && old != icon as usize {
        DestroyIcon(old as HICON);
    }
    true
}

/// Ist `p` (nullterminiert, aus lParam) gleich `s`?
unsafe fn wstr_is(p: *const u16, s: &str) -> bool {
    if p.is_null() {
        return false;
    }
    let mut i = 0;
    for c in s.encode_utf16() {
        if *p.add(i) != c {
            return false;
        }
        i += 1;
    }
    *p.add(i) == 0
}

/// Nutzt die Taskleiste den dunklen Modus? (fehlt der Eintrag: hell)
fn system_uses_dark() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let val = wide("SystemUsesLightTheme");
    let mut v = 1u32;
    let mut len = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut v as *mut u32 as *mut std::ffi::c_void,
            &mut len,
        ) == 0
            && v == 0
    }
}

/// Menüauswahl für Einstellungen; liefert true, wenn behandelt.
fn handle_setting(id: usize) -> bool {
    use crate::config::update;
    match id {
        ID_AUDIO => {
            update(|c| c.audio_enabled = !c.audio_enabled);
            crate::state::mark_settings_dirty(); // Lautstärketasten hängen davon ab
            crate::state::request_reconnect();
        }
        ID_Q_LOW | ID_Q_STD | ID_Q_HIGH => {
            let v = match id {
                ID_Q_LOW => 10,
                ID_Q_HIGH => 40,
                _ => 0,
            };
            update(|c| {
                c.bitrate_mbps = v;
                c.quality_max = false;
            });
            crate::state::request_reconnect();
        }
        ID_Q_MAX => {
            update(|c| c.quality_max = true);
            crate::state::request_reconnect();
        }
        ID_STILLS => {
            update(|c| c.lossless_stills = !c.lossless_stills);
            crate::state::request_reconnect();
        }
        i if (ID_PEN_BASE..ID_PEN_BASE + PEN_CURVES.len()).contains(&i) => {
            let curve = PEN_CURVES[i - ID_PEN_BASE];
            update(|c| c.pen_pressure = curve.into());
        }
        i if (ID_PENBTN_BASE..ID_PENBTN_BASE + PEN_BUTTONS.len()).contains(&i) => {
            let mode = PEN_BUTTONS[i - ID_PENBTN_BASE];
            update(|c| c.pen_button = mode.into());
        }
        i if (ID_B_BASE..ID_B_BASE + BRIGHTNESS.len()).contains(&i) => {
            let b = BRIGHTNESS[i - ID_B_BASE];
            update(|c| c.tablet_brightness = b);
            crate::state::mark_settings_dirty();
        }
        ID_INPUT => {
            update(|c| c.tablet_input = !c.tablet_input);
            crate::state::mark_settings_dirty();
        }
        ID_STATS => {
            update(|c| c.show_stats = !c.show_stats);
            crate::state::mark_settings_dirty();
        }
        ID_POWER => update(|c| c.follow_pc_power = !c.follow_pc_power),
        ID_DND => {
            update(|c| c.tablet_dnd = !c.tablet_dnd);
            crate::state::mark_settings_dirty();
        }
        ID_VOLKEYS => {
            update(|c| c.tablet_volume_keys = !c.tablet_volume_keys);
            crate::state::mark_settings_dirty();
        }
        ID_UPDCHECK => {
            let on = !crate::config::current().update_check;
            update(|c| c.update_check = on);
            if on {
                crate::update::check_now();
            } else {
                crate::update::clear();
            }
        }
        ID_WEBCAM => {
            let on = !crate::config::current().webcam;
            update(|c| c.webcam = on);
            crate::camera::set_enabled(on, on);
            if !on {
                crate::notify::once(
                    "camon",
                    tr("Webcam ausgeschaltet", "Webcam turned off"),
                    tr(
                        "Die USBCD-Kamera ist aus Windows entfernt.",
                        "The USBCD camera has been removed from Windows.",
                    ),
                    std::time::Duration::ZERO,
                );
            }
        }
        ID_PAUSE => crate::state::set_paused(!crate::state::paused()),
        i if i >= ID_DEV_BASE => {
            let entry = MENU_DEVICES
                .lock()
                .ok()
                .and_then(|g| g.get(i - ID_DEV_BASE).cloned());
            if let Some((serial, name, use_it)) = entry {
                set_device(&serial, &name, use_it);
            }
        }
        ID_AUTOSTART => {
            std::thread::spawn(|| {
                if crate::autostart::enabled() {
                    crate::autostart::disable();
                    set_autostart_known(false);
                    crate::notify::once(
                        "autostart",
                        tr("Autostart ausgeschaltet", "Autostart turned off"),
                        tr(
                            "USBCD startet nicht mehr automatisch beim Anmelden.",
                            "USBCD no longer starts automatically at sign-in.",
                        ),
                        std::time::Duration::ZERO,
                    );
                } else {
                    match crate::autostart::enable() {
                        Ok(()) => {
                            set_autostart_known(true);
                            crate::notify::once(
                                "autostart",
                                tr("Autostart eingeschaltet", "Autostart turned on"),
                                tr(
                                    "USBCD startet beim Anmelden automatisch.",
                                    "USBCD now starts automatically at sign-in.",
                                ),
                                std::time::Duration::ZERO,
                            );
                        }
                        Err(e) => crate::notify::once(
                            "autostart",
                            tr("Autostart nicht möglich", "Autostart not possible"),
                            &format!("{e}"),
                            std::time::Duration::ZERO,
                        ),
                    }
                }
            });
        }
        _ => return false,
    }
    true
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) {
        add_icon(hwnd); // Explorer wurde neu gestartet
        return 0;
    }
    match msg {
        WM_TRAY => {
            let ev = (lp as u32) & 0xFFFF;
            if ev == WM_LBUTTONUP || ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU {
                show_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            if handle_setting(wp & 0xFFFF) {
                return 0;
            }
            match wp & 0xFFFF {
                ID_LAUNCH => crate::state::request_launch(),
                ID_UPDATE => {
                    if let Some(r) = crate::update::available() {
                        crate::update::install(&r.version);
                    }
                }
                ID_SOUND => open("ms-settings:sound"),
                ID_DISPLAY => open("ms-settings:display"),
                ID_FOLDER => open(&crate::config::data_dir().to_string_lossy()),
                ID_ABOUT => show_about(),
                ID_QUIT => {
                    set_shutdown();
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_COPYDATA => {
            // Klick in einer USBCD-Benachrichtigung (über das zweite USBCD.exe --uri …)
            let cds = &*(lp as *const COPYDATASTRUCT);
            if cds.dwData == COPYDATA_URI && cds.cbData & 1 == 0 && cds.cbData <= 2048 {
                let w =
                    std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
                let uri = String::from_utf16_lossy(w);
                std::thread::spawn(move || handle_uri(&uri));
                return 1;
            }
            0
        }
        WM_POWERBROADCAST => {
            // PC-Bildschirm an/aus (Windows meldet das auch direkt nach der Anmeldung)
            const PBT_POWERSETTINGCHANGE: usize = 0x8013;
            if wp == PBT_POWERSETTINGCHANGE && lp != 0 {
                use windows_sys::Win32::System::Power::POWERBROADCAST_SETTING;
                let s = &*(lp as *const POWERBROADCAST_SETTING);
                let g = s.PowerSetting;
                let want = windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
                if g.data1 == want.data1
                    && g.data2 == want.data2
                    && g.data3 == want.data3
                    && g.data4 == want.data4
                    && s.DataLength >= 4
                {
                    // 0 = aus, 1 = an, 2 = gedimmt (gedimmt zählt als an)
                    let state = std::ptr::read_unaligned(s.Data.as_ptr() as *const u32);
                    DISPLAY_OFF.store(state == 0, Ordering::Relaxed);
                    update_dark();
                }
            }
            1
        }
        WM_QUERYENDSESSION => {
            // Herunterfahren/Neustart/Abmelden kündigt sich an: ab jetzt keine
            // Hilfsprogramme mehr starten (ENDSESSION_CLOSEAPP: nur USBCD wird geschlossen)
            const ENDSESSION_CLOSEAPP: isize = 0x0000_0001;
            crate::session::system_ending_soon(lp & ENDSESSION_CLOSEAPP != 0);
            1
        }
        WM_ENDSESSION => {
            if wp != 0 {
                crate::session::system_ending();
            } else {
                crate::session::system_ending_cancelled();
            }
            0
        }
        WM_WTSSESSION_CHANGE => {
            const WTS_SESSION_LOCK: usize = 7;
            const WTS_SESSION_UNLOCK: usize = 8;
            match wp {
                WTS_SESSION_LOCK => LOCKED.store(true, Ordering::Relaxed),
                WTS_SESSION_UNLOCK => LOCKED.store(false, Ordering::Relaxed),
                _ => {}
            }
            update_dark();
            0
        }
        WM_TIMER => {
            update_tip(hwnd);
            0
        }
        WM_NOTE => {
            let next = PENDING.lock().ok().and_then(|mut g| {
                if g.is_empty() {
                    None
                } else {
                    Some(g.remove(0))
                }
            });
            if let Some((t, x)) = next {
                balloon(hwnd, &t, &x);
            }
            0
        }
        WM_SHOW_INFO => {
            crate::notify::show(crate::notify::Toast {
                title: tr("USBCD läuft", "USBCD is running").to_string(),
                text: format!(
                    "{}\n{}",
                    status(),
                    tr(
                        "Menü: Klick auf das USBCD-Symbol in der Taskleiste.",
                        "Menu: click the USBCD icon in the taskbar."
                    )
                ),
                silent: true,
                ..Default::default()
            });
            0
        }
        WM_SETTINGCHANGE => {
            // hell/dunkel umgestellt: Symbol im Infobereich anpassen
            if wstr_is(lp as *const u16, "ImmersiveColorSet") && load_tray_icon() {
                let mut n = nid(hwnd);
                n.uFlags = NIF_ICON;
                n.hIcon = ICON.load(Ordering::Relaxed) as HICON;
                Shell_NotifyIconW(NIM_MODIFY, &n);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DESTROY => {
            let n = nid(hwnd);
            Shell_NotifyIconW(NIM_DELETE, &n);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Gerät zulassen bzw. auf „nie verwenden“ stellen (Menü oder Benachrichtigung).
fn set_device(serial: &str, name: &str, use_it: bool) {
    let name = if name.is_empty() { serial } else { name };
    crate::config::update(|c| {
        let n = c
            .devices
            .get(serial)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| name.to_string());
        let decoder_prompt = c.devices.get(serial).and_then(|d| d.decoder_prompt);
        let stream_fallback = c
            .devices
            .get(serial)
            .and_then(|d| d.stream_fallback.clone());
        c.devices.insert(
            serial.to_string(),
            crate::config::KnownDevice {
                name: n,
                use_as_display: use_it,
                decoder_prompt,
                stream_fallback,
            },
        );
    });
    crate::state::remove_pending(serial);
    crate::notify::reset("device-choice");
    if use_it {
        crate::log!("Gerät {serial} ({name}) als Bildschirm zugelassen");
    } else {
        crate::log!("Gerät {serial} ({name}) auf „nie verwenden“ gestellt");
        crate::state::request_end_session();
        crate::notify::once(
            "device-choice",
            tr("Gerät wird nicht verwendet", "Device will not be used"),
            &format!(
                "{name}\n{}",
                tr(
                    "Änderbar jederzeit im USBCD-Menü unter „Geräte“.",
                    "You can change this anytime in the USBCD menu under “Devices”."
                )
            ),
            std::time::Duration::ZERO,
        );
    }
}

/// Aktion aus einem `usbcd:`-Link (nur mit gültigem Sitzungs-Kennwort).
fn handle_uri(uri: &str) {
    use crate::notify::{parse_uri, Action};
    match parse_uri(uri, crate::notify::nonce()) {
        None => crate::log!("Unbekannter oder veralteter USBCD-Link ignoriert"),
        Some(Action::None) => {}
        Some(Action::OpenLog) => open(&crate::config::data_dir().to_string_lossy()),
        Some(Action::ExtendDisplays) => match crate::display::extend_all() {
            Ok(()) => {
                crate::log!("Anzeige auf „Erweitern“ umgestellt (Klick in der Meldung)");
                crate::notify::reset("notextended");
                crate::state::request_launch(); // sofort neu verbinden
            }
            Err(e) => crate::notify::once(
                "failed",
                crate::i18n::tr("USBCD: Erweitern fehlgeschlagen", "USBCD: extending failed"),
                &format!(
                    "{e:#}\n{}",
                    crate::i18n::tr(
                        "Bitte Windows-Taste + P drücken und „Erweitern“ wählen.",
                        "Please press Windows key + P and choose “Extend”."
                    )
                ),
                std::time::Duration::from_secs(30),
            ),
        },
        Some(Action::UseDevice(s)) | Some(Action::NeverDevice(s))
            if !crate::state::pending_devices().iter().any(|(p, _)| p == &s)
                && !crate::config::current().devices.contains_key(&s) =>
        {
            crate::log!("USBCD-Link für unbekanntes Gerät {s} ignoriert");
        }
        Some(Action::UseDevice(s)) => {
            let name = name_of(&s);
            set_device(&s, &name, true);
        }
        Some(Action::NeverDevice(s)) => {
            let name = name_of(&s);
            set_device(&s, &name, false);
        }
        Some(Action::UpdateInstall(v)) => crate::update::install(&v),
        Some(Action::UpdateSkip(v)) => {
            crate::log!("Update {v}: wird nicht mehr per Benachrichtigung angeboten");
            crate::config::update(|c| c.update_skip = v);
        }
    }
}

fn name_of(serial: &str) -> String {
    crate::state::pending_devices()
        .into_iter()
        .find(|(s, _)| s == serial)
        .map(|(_, n)| n)
        .or_else(|| {
            crate::config::current()
                .devices
                .get(serial)
                .map(|d| d.name.clone())
        })
        .unwrap_or_default()
}

/// Zweite USBCD-Instanz (aus einer Benachrichtigung gestartet): Link an die laufende geben.
pub fn send_uri(uri: &str) {
    if uri.len() > 1000 {
        return;
    }
    let class = wide(CLASS);
    let w: Vec<u16> = uri.encode_utf16().collect();
    unsafe {
        let h = FindWindowW(class.as_ptr(), null());
        if h.is_null() {
            return; // USBCD läuft nicht mehr – alte Benachrichtigung
        }
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_URI,
            cbData: (w.len() * 2) as u32,
            lpData: w.as_ptr() as *mut _,
        };
        let mut res = 0usize;
        SendMessageTimeoutW(
            h,
            WM_COPYDATA,
            0,
            &cds as *const _ as LPARAM,
            SMTO_ABORTIFHUNG,
            3000,
            &mut res,
        );
    }
}

/// Bittet eine bereits laufende USBCD-Instanz, ihre Info-Blase zu zeigen.
pub fn show_existing() -> bool {
    let class = wide(CLASS);
    unsafe {
        let h = FindWindowW(class.as_ptr(), null());
        if h.is_null() {
            return false;
        }
        PostMessageW(h, WM_SHOW_INFO, 0, 0) != 0
    }
}

const CLASS: &str = "USBCD_Tray";

/// Beendet USBCD wie über „USBCD beenden“ (thread-sicher; z. B. vor einem Update).
pub fn request_quit() {
    let h = HWND_G.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_COMMAND, ID_QUIT, 0);
        }
    }
}

/// Zeigt in der laufenden Instanz eine Info-Blase (thread-sicher).
pub fn post_show() {
    let h = HWND_G.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_SHOW_INFO, 0, 0);
        }
    }
}

/// Blockiert, bis „Beenden“ gewählt wird.
pub fn run(show_on_start: bool) {
    unsafe {
        let hinst = GetModuleHandleW(null());
        let class = wide(CLASS);
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);

        load_tray_icon();
        // Info-Blase: App-Symbol mit Hintergrund (wie bei den Benachrichtigungen)
        let ico_path = wide(&crate::config::exe_dir().join("USBCD.ico").to_string_lossy());
        let big = LoadImageW(
            null_mut(),
            ico_path.as_ptr(),
            IMAGE_ICON,
            64,
            64,
            LR_LOADFROMFILE,
        );
        BIG_ICON.store(big as usize, Ordering::Relaxed);

        let title = wide("USBCD");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if hwnd.is_null() {
            crate::log!("Taskleisten-Symbol: Fenster konnte nicht erstellt werden");
            return;
        }
        HWND_G.store(hwnd as usize, Ordering::Relaxed);

        // Explorer meldet einen Neustart der Taskleiste mit dieser Nachricht. USBCD läuft
        // ohne Administratorrechte, daher sind keine Nachrichtenfilter-Ausnahmen nötig.
        let tb = wide("TaskbarCreated");
        TASKBAR_CREATED.store(RegisterWindowMessageW(tb.as_ptr()), Ordering::Relaxed);

        add_icon(hwnd);
        SetTimer(hwnd, TIMER_ID, 1000, None);
        // Bildschirm aus/an und Sperren/Entsperren melden lassen
        windows_sys::Win32::System::Power::RegisterPowerSettingNotification(
            hwnd as _,
            &windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE,
            0, // DEVICE_NOTIFY_WINDOW_HANDLE
        );
        windows_sys::Win32::System::RemoteDesktop::WTSRegisterSessionNotification(hwnd, 0);
        if show_on_start {
            post_show();
        }
        // Meldungen, die vor dem Erscheinen des Symbols anfielen
        let n = PENDING.lock().map(|g| g.len()).unwrap_or(0);
        for _ in 0..n {
            PostMessageW(hwnd, WM_NOTE, 0, 0);
        }

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        HWND_G.store(0, Ordering::Relaxed);
    }
}
