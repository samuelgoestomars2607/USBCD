//! Gemeinsamer Zustand des Programms (Menü, Tablet-Erkennung und Übertragung laufen in
//! verschiedenen Threads): Beenden, Statuszeile, Anforderungen aus dem Menü, Bildschirm
//! aus/gesperrt, wartende Geräte, Herunterfahren.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);
static STATUS: Mutex<String> = Mutex::new(String::new());

pub fn shutdown() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

pub fn set_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

pub fn set_status(s: &str) {
    if let Ok(mut g) = STATUS.lock() {
        *g = s.to_string();
    }
}

pub fn status() -> String {
    STATUS.lock().map(|g| g.clone()).unwrap_or_default()
}

static LAUNCH_REQUEST: AtomicBool = AtomicBool::new(false);

/// Menü „Auf dem Tablet anzeigen“: App beim nächsten Durchlauf starten.
pub fn request_launch() {
    LAUNCH_REQUEST.store(true, Ordering::SeqCst);
}

pub fn take_launch_request() -> bool {
    LAUNCH_REQUEST.swap(false, Ordering::SeqCst)
}

static SETTINGS_DIRTY: AtomicBool = AtomicBool::new(false);
static RECONNECT: AtomicBool = AtomicBool::new(false);

/// Tablet-Einstellungen (Helligkeit) beim nächsten Bild an das Tablet senden.
pub fn mark_settings_dirty() {
    SETTINGS_DIRTY.store(true, Ordering::SeqCst);
}

pub fn take_settings_dirty() -> bool {
    SETTINGS_DIRTY.swap(false, Ordering::SeqCst)
}

/// Laufende Übertragung mit neuen Einstellungen (Qualität, Ton) neu aufbauen.
pub fn request_reconnect() {
    RECONNECT.store(true, Ordering::SeqCst);
}

pub fn take_reconnect() -> bool {
    RECONNECT.swap(false, Ordering::SeqCst)
}

// --- Zustand „PC-Bildschirm aus / gesperrt“ und „Tablet ausgeschaltet“ ---

static PC_DARK: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);

/// Vom Taskleisten-Fenster: PC-Bildschirm aus oder Sitzung gesperrt.
pub fn set_pc_dark(on: bool) {
    PC_DARK.store(on, Ordering::Relaxed);
}

pub fn pc_dark() -> bool {
    PC_DARK.load(Ordering::Relaxed)
}

/// „Tablet ausschalten“ im Menü.
pub fn set_paused(on: bool) {
    PAUSED.store(on, Ordering::Relaxed);
}

pub fn paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
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

static END_SESSION: AtomicBool = AtomicBool::new(false);
static WINDOWS_ENDING: AtomicBool = AtomicBool::new(false);

/// Windows fährt herunter, startet neu oder meldet ab (ab der Rückfrage an die Programme;
/// bei Abbruch wieder aus).
pub fn windows_ending() -> bool {
    WINDOWS_ENDING.load(Ordering::SeqCst)
}

pub fn set_windows_ending(on: bool) {
    WINDOWS_ENDING.store(on, Ordering::SeqCst);
}

/// Laufende Übertragung beenden (z. B. Gerät auf „nie verwenden“ gestellt).
pub fn request_end_session() {
    END_SESSION.store(true, Ordering::SeqCst);
}

pub fn take_end_session() -> bool {
    END_SESSION.swap(false, Ordering::SeqCst)
}
