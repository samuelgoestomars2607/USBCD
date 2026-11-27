//! Log-Datei `usbcd.log` (im Datenordner, beim Dienst und den Setup-Befehlen unter
//! ProgramData). Wird sie größer als 5 MB, wandert sie nach `usbcd.old.log`.

use std::io::Write;
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

/// Zeile ins Log schreiben: `log!("Text {x}")`.
#[macro_export]
macro_rules! log {
    ($($a:tt)*) => { $crate::log::log_line(&format!($($a)*)) };
}
