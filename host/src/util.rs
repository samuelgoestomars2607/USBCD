use std::io::Write;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static LOGFILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
static LOG_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const LOG_MAX: u64 = 5 << 20;

static LOG_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Anderer Log-Ort (Hilfsdienst und Setup-Befehle schreiben nach ProgramData).
pub fn set_log_file(p: std::path::PathBuf) {
    let _ = LOG_PATH.set(p);
}

fn log_path() -> std::path::PathBuf {
    LOG_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| crate::config::data_dir().join("usbcd.log"))
}

fn open_log() -> Option<std::fs::File> {
    let path = log_path();
    // zu groß → als usbcd.old.log beiseitelegen und neu beginnen
    if std::fs::metadata(&path)
        .map(|m| m.len() > LOG_MAX)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(&path, path.with_extension("old.log"));
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    let len = f
        .as_ref()
        .and_then(|f| f.metadata().ok())
        .map(|m| m.len())
        .unwrap_or(0);
    LOG_BYTES.store(len, std::sync::atomic::Ordering::Relaxed);
    f
}

pub fn init_log() {
    START.get_or_init(Instant::now);
    let _ = LOGFILE.set(Mutex::new(open_log()));
}

pub fn log_line(msg: &str) {
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let line = format!("[{t:9.2}s] {msg}\n");
    eprint!("{line}");
    if let Some(m) = LOGFILE.get() {
        if let Ok(mut g) = m.lock() {
            if LOG_BYTES.load(std::sync::atomic::Ordering::Relaxed) > LOG_MAX {
                *g = None; // Datei schließen, damit sie umbenannt werden kann
                *g = open_log();
            }
            if let Some(f) = g.as_mut() {
                if f.write_all(line.as_bytes()).is_ok() {
                    LOG_BYTES.fetch_add(line.len() as u64, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }
}

#[macro_export]
macro_rules! log {
    ($($a:tt)*) => { $crate::util::log_line(&format!($($a)*)) };
}

/// Kindprozess ohne aufpoppendes Konsolenfenster.
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Hilfsprozesse (ffmpeg, adb-Beobachter) an USBCD binden: Endet USBCD – auch durch
/// Absturz oder „Task beenden“ –, beendet Windows sie mit. Der adb-Server selbst wird
/// nicht angebunden (andere Programme wie Android Studio nutzen ihn mit).
pub fn bind_to_us(child: &std::process::Child) {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let job = *JOB.get_or_init(|| {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            h as usize // bleibt bis zum Prozessende offen
        });
        if job != 0 {
            AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
        }
    }
    #[cfg(not(windows))]
    let _ = child;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STATUS: Mutex<String> = Mutex::new(String::new());

pub fn shutdown() -> bool {
    SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn set_shutdown() {
    SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn set_status(s: &str) {
    if let Ok(mut g) = STATUS.lock() {
        *g = s.to_string();
    }
}

pub fn status() -> String {
    STATUS.lock().map(|g| g.clone()).unwrap_or_default()
}

static LAUNCH_REQUEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Menü „Auf dem Tablet anzeigen“: App beim nächsten Durchlauf starten.
pub fn request_launch() {
    LAUNCH_REQUEST.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn take_launch_request() -> bool {
    LAUNCH_REQUEST.swap(false, std::sync::atomic::Ordering::SeqCst)
}

static LAST_NOTE: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// Windows-Benachrichtigung zeigen; dieselbe Meldung (key) höchstens einmal je `cooldown`.
pub fn notify(key: &str, title: &str, text: &str, cooldown: std::time::Duration) {
    notify_with(key, title, text, cooldown, Vec::new());
}

/// Wie `notify`, mit eigenen Knöpfen (Beschriftung, `usbcd:`-Aktion) vor „Log öffnen“.
pub fn notify_with(
    key: &str,
    title: &str,
    text: &str,
    cooldown: std::time::Duration,
    mut actions: Vec<(String, String)>,
) {
    if let Ok(mut g) = LAST_NOTE.lock() {
        if let Some((_, t)) = g.iter().find(|(k, _)| k == key) {
            if t.elapsed() < cooldown {
                return;
            }
        }
        g.retain(|(k, _)| k != key);
        g.push((key.to_string(), Instant::now()));
    }
    crate::log!("Hinweis: {title} – {}", text.replace('\n', " "));
    // Fehler mit Ton und „Log öffnen“, Statusmeldungen leise
    let problem = matches!(
        key,
        "failed" | "installfail" | "ffmpeg" | "helper" | "security" | "camera" | "notextended"
    );
    if problem {
        actions.push((
            crate::i18n::tr("Log öffnen", "Open log").to_string(),
            "log".to_string(),
        ));
    }
    crate::notify::show(crate::notify::Toast {
        title: title.to_string(),
        text: text.to_string(),
        actions,
        silent: !problem,
        long: problem,
    });
}

/// Sperre einer Meldung aufheben (z. B. nach dem Abziehen), damit sie beim
/// nächsten Mal wieder erscheint.
pub fn reset_notify(key: &str) {
    if let Ok(mut g) = LAST_NOTE.lock() {
        g.retain(|(k, _)| k != key);
    }
}

static SETTINGS_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RECONNECT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tablet-Einstellungen (Helligkeit) beim nächsten Bild an das Tablet senden.
pub fn mark_settings_dirty() {
    SETTINGS_DIRTY.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn take_settings_dirty() -> bool {
    SETTINGS_DIRTY.swap(false, std::sync::atomic::Ordering::SeqCst)
}

/// Laufende Übertragung mit neuen Einstellungen (Qualität, Ton) neu aufbauen.
pub fn request_reconnect() {
    RECONNECT.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn take_reconnect() -> bool {
    RECONNECT.swap(false, std::sync::atomic::Ordering::SeqCst)
}

// --- Zustand „PC-Bildschirm aus / gesperrt“ und „Tablet ausgeschaltet“ ---

static PC_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Vom Taskleisten-Fenster: PC-Bildschirm aus oder Sitzung gesperrt.
pub fn set_pc_dark(on: bool) {
    PC_DARK.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn pc_dark() -> bool {
    PC_DARK.load(std::sync::atomic::Ordering::Relaxed)
}

/// „Tablet ausschalten“ im Menü.
pub fn set_paused(on: bool) {
    PAUSED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn paused() -> bool {
    PAUSED.load(std::sync::atomic::Ordering::Relaxed)
}

// --- Geräte, die auf eine Entscheidung warten („Neues Gerät erkannt“) ---

static PENDING_DEVICES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub fn add_pending(serial: &str, name: &str) {
    if let Ok(mut g) = PENDING_DEVICES.lock() {
        if !g.iter().any(|(s, _)| s == serial) {
            g.push((serial.to_string(), name.to_string()));
        }
    }
}

pub fn remove_pending(serial: &str) {
    if let Ok(mut g) = PENDING_DEVICES.lock() {
        g.retain(|(s, _)| s != serial);
    }
}

pub fn pending_devices() -> Vec<(String, String)> {
    PENDING_DEVICES
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

static END_SESSION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Laufende Übertragung beenden (z. B. Gerät auf „nie verwenden“ gestellt).
pub fn request_end_session() {
    END_SESSION.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn take_end_session() -> bool {
    END_SESSION.swap(false, std::sync::atomic::Ordering::SeqCst)
}
