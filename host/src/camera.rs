//! Webcam: die Selfie-Kamera des Tablets als „USBCD Tablet Webcam“ in Windows 11.
//!
//! - Schalter im Menü (Standard: aus). Eingeschaltet legt USBCD über usbcd_cam.dll eine
//!   virtuelle Kamera an – nur für diesen Nutzer und nur solange USBCD läuft. Sie wird nie
//!   zur Standardkamera gemacht; man wählt sie in der jeweiligen App selbst aus.
//! - Die Tablet-Kamera läuft nur, während eine App die Webcam wirklich benutzt: Dann
//!   verbindet sich der Windows-Kameradienst mit der Pipe unten, USBCD bittet das Tablet
//!   um Bilder (H.264), dekodiert sie mit FFmpeg zu NV12 1280×720 und reicht sie weiter.
//! - Ohne Bild (Tablet nicht verbunden, ausgeschaltet, Zugriff verweigert …) zeigt die
//!   Kamera ein Hinweisbild statt Schwarz.
//! - Nur die Frontkamera (Selfie) – die Rückkamera wird nie benutzt.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::i18n::tr;
use std::io::{Read, Write};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

// --- gemeinsam mit camera/src/lib.rs (usbcd_cam.dll) ---------------------------------
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;
pub const FRAME_LEN: usize = (WIDTH * HEIGHT * 3 / 2) as usize;
const PIPE_NAME: &str = r"\\.\pipe\USBCD-Kamera";
const MAGIC: &[u8; 4] = b"UCAM";
/// Nur der Windows-Kameradienst (LocalService bzw. SYSTEM) darf lesen – andere Programme
/// des Nutzers können das Kamerabild hier nicht abgreifen.
const PIPE_SDDL: &str = "D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;OW)";

fn header() -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..4].copy_from_slice(MAGIC);
    h[4..8].copy_from_slice(&WIDTH.to_le_bytes());
    h[8..12].copy_from_slice(&HEIGHT.to_le_bytes());
    h[12..16].copy_from_slice(&(FRAME_LEN as u32).to_le_bytes());
    h
}

/// Was das Tablet über seine Kamera meldet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabletCam {
    /// keine Verbindung zum Tablet
    Disconnected,
    /// verbunden, Kamera noch nicht gemeldet
    Starting,
    /// wartet auf „Zulassen“ am Tablet
    Asking,
    On {
        rotation: u32,
        width: u32,
        height: u32,
    },
    Off,
    Denied,
    NoFrontCamera,
    Error,
}

struct St {
    /// Menüschalter
    enabled: bool,
    /// virtuelle Kamera ist angelegt
    created: bool,
    /// eine App benutzt die Webcam gerade
    consumer: bool,
    tablet: TabletCam,
    latest: Option<(Arc<Vec<u8>>, Instant)>,
    seq: u64,
    decoder: Option<Decoder>,
    /// Codec-Konfiguration (SPS/PPS) für einen neu gestarteten Decoder
    codec_config: Vec<u8>,
    need_key: bool,
    placeholder: Option<(String, Arc<Vec<u8>>)>,
}

struct Shared {
    st: Mutex<St>,
    cv: Condvar,
}

fn shared() -> &'static Shared {
    static S: OnceLock<Shared> = OnceLock::new();
    S.get_or_init(|| Shared {
        st: Mutex::new(St {
            enabled: false,
            created: false,
            consumer: false,
            tablet: TabletCam::Disconnected,
            latest: None,
            seq: 0,
            decoder: None,
            codec_config: Vec::new(),
            need_key: true,
            placeholder: None,
        }),
        cv: Condvar::new(),
    })
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    let mut g = shared()
        .st
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let r = f(&mut g);
    drop(g);
    shared().cv.notify_all();
    r
}

/// Zähler für Schlüsselbild-Anfragen an das Tablet
static KEY_REQUESTS: AtomicU32 = AtomicU32::new(0);
static MANAGER: AtomicBool = AtomicBool::new(false);

pub fn camera_name() -> &'static str {
    // in beiden Sprachen gleich
    "USBCD Tablet Webcam"
}

/// Beim Start: Webcam einrichten, falls im Menü eingeschaltet.
pub fn init() {
    if crate::config::current().webcam {
        set_enabled(true, false);
    }
}

/// Menüschalter „Tablet-Kamera als Webcam“.
pub fn set_enabled(on: bool, announce: bool) {
    with(|s| s.enabled = on);
    if !MANAGER.swap(true, Ordering::SeqCst) {
        std::thread::Builder::new()
            .name("webcam".into())
            .spawn(manager)
            .ok();
    }
    if announce {
        ANNOUNCE.store(true, Ordering::SeqCst);
    }
    if on {
        RETRY.store(true, Ordering::SeqCst); // nach einem Fehler erneut versuchen
    }
    if !on {
        poke_pipe();
    }
}

static ANNOUNCE: AtomicBool = AtomicBool::new(false);
static RETRY: AtomicBool = AtomicBool::new(false);

/// Steuernachricht für das Tablet (wird gesendet, wenn sie sich ändert).
pub fn control_json() -> String {
    let (enabled, on) = with(|s| (s.enabled && s.created, s.enabled && s.created && s.consumer));
    serde_json::json!({
        "enabled": enabled,
        "on": on,
        "width": WIDTH,
        "height": HEIGHT,
        "fps": FPS,
        "key": KEY_REQUESTS.load(Ordering::Relaxed),
    })
    .to_string()
}

fn request_key() {
    KEY_REQUESTS.fetch_add(1, Ordering::Relaxed);
}

/// Verbindung zum Tablet steht.
pub fn session_started() {
    with(|s| {
        s.tablet = TabletCam::Starting;
        s.decoder = None;
        s.latest = None;
    });
}

/// Verbindung zum Tablet beendet.
pub fn session_ended() {
    with(|s| {
        s.tablet = TabletCam::Disconnected;
        s.decoder = None;
        s.latest = None;
        s.codec_config.clear();
    });
}

/// T_CAM_STATE vom Tablet: {"state": "on"|"off"|"asking"|"denied"|"none"|"error", …}
pub fn on_tablet_state(payload: &[u8]) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return;
    };
    let num = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
    let state = match v.get("state").and_then(|x| x.as_str()).unwrap_or("") {
        "on" => {
            let (w, h, r) = (num("width"), num("height"), num("rotation"));
            if !(16..=4096).contains(&w) || !(16..=4096).contains(&h) || r % 90 != 0 || r >= 360 {
                crate::log!("Webcam: ungültige Angaben vom Tablet: {w}x{h}, {r}°");
                TabletCam::Error
            } else {
                TabletCam::On {
                    rotation: r,
                    width: w,
                    height: h,
                }
            }
        }
        "off" => TabletCam::Off,
        "asking" => TabletCam::Asking,
        "denied" => TabletCam::Denied,
        "none" => TabletCam::NoFrontCamera,
        _ => TabletCam::Error,
    };
    crate::log!("Webcam: Tablet meldet {state:?}");
    if state == TabletCam::Denied {
        crate::util::notify(
            "camdenied",
            tr("Kamerazugriff am Tablet verweigert", "Camera access denied on the tablet"),
            tr(
                "Damit die Webcam ein Bild zeigt: am Tablet Einstellungen → Apps → USBCD → Berechtigungen → Kamera erlauben.",
                "For the webcam to show a picture: on the tablet, open Settings → Apps → USBCD → Permissions and allow Camera.",
            ),
            Duration::from_secs(3600),
        );
    }
    let start = with(|s| {
        let changed = s.tablet != state;
        s.tablet = state.clone();
        // Bilder nur annehmen, wenn die Webcam eingeschaltet ist und eine App sie benutzt
        let wanted = s.enabled && s.created && s.consumer;
        match &state {
            TabletCam::On { .. } if !wanted => {
                s.decoder = None;
                false
            }
            TabletCam::On { .. } if changed || s.decoder.is_none() => true,
            TabletCam::On { .. } => false,
            _ => {
                s.decoder = None;
                s.latest = None;
                false
            }
        }
    });
    if let (true, TabletCam::On { rotation, .. }) = (start, &state) {
        start_decoder(*rotation);
    }
}

/// T_CAM_DATA vom Tablet: [Flags u8][H.264 Annex-B]; Flags 1 = Schlüsselbild, 2 = Konfiguration.
pub fn on_tablet_data(payload: &[u8]) {
    let Some((&flags, data)) = payload.split_first() else {
        return;
    };
    // ein Bild der Selfie-Kamera (720p) ist weit kleiner – Übergroßes verwerfen
    if data.len() > 1 << 20 {
        return;
    }
    let key = flags & 1 != 0;
    let config = flags & 2 != 0;
    let mut want_key = false;
    with(|s| {
        if config {
            s.codec_config = data.to_vec();
        }
        let Some(dec) = s.decoder.as_ref() else {
            return;
        };
        if s.need_key && !(key || config) {
            return; // erst ab dem nächsten Schlüsselbild dekodierbar
        }
        if key {
            s.need_key = false;
        }
        match dec.tx.try_send(data.to_vec()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                // FFmpeg kommt nicht hinterher: Lücke → bis zum nächsten Schlüsselbild warten
                s.need_key = true;
                want_key = true;
            }
            Err(TrySendError::Disconnected(_)) => {
                s.decoder = None;
            }
        }
    });
    if want_key {
        request_key();
    }
}

// --- Decoder (FFmpeg) --------------------------------------------------------------

struct Decoder {
    child: Child,
    tx: SyncSender<Vec<u8>>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static GENERATION: AtomicU32 = AtomicU32::new(0);

/// Filter: Bild aufrecht drehen und auf 1280×720 einpassen (schwarze Ränder statt Verzerren).
fn filter(rotation: u32) -> String {
    let rot = match rotation {
        90 => "transpose=clock,",
        180 => "hflip,vflip,",
        270 => "transpose=cclock,",
        _ => "",
    };
    format!(
        "{rot}scale=w={WIDTH}:h={HEIGHT}:force_original_aspect_ratio=decrease:force_divisible_by=2,\
         pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=nv12"
    )
}

fn decoder_args(rotation: u32) -> Vec<String> {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-threads",
        "1",
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-fflags",
        "nobuffer",
        "-flags",
        "low_delay",
        "-f",
        "h264",
        "-i",
        "pipe:0",
        "-vf",
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .chain([filter(rotation)])
    .chain(
        [
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "nv12",
            "-f",
            "rawvideo",
            "pipe:1",
        ]
        .iter()
        .map(std::string::ToString::to_string),
    )
    .collect()
}

fn start_decoder(rotation: u32) {
    if crate::util::windows_ending() {
        return;
    }
    let exe = crate::config::current().ffmpeg_exe;
    let child = crate::util::command(&exe)
        .args(decoder_args(rotation))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            crate::log!("Webcam: FFmpeg startet nicht: {e}");
            return;
        }
    };
    crate::util::bind_to_us(&child);
    let gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx, rx) = sync_channel::<Vec<u8>>(30);
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            for chunk in rx {
                if stdin.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
    }
    if let Some(mut out) = child.stdout.take() {
        std::thread::spawn(move || loop {
            let mut buf = vec![0u8; FRAME_LEN];
            if out.read_exact(&mut buf).is_err() {
                break;
            }
            with(|s| {
                if GENERATION.load(Ordering::SeqCst) == gen {
                    s.latest = Some((Arc::new(buf), Instant::now()));
                    s.seq += 1;
                }
            });
        });
    }
    if let Some(err) = child.stderr.take() {
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(err)
                .lines()
                .map_while(Result::ok)
                .take(50)
            {
                crate::log!("Webcam-FFmpeg: {line}");
            }
        });
    }
    crate::log!("Webcam: Bild vom Tablet wird dekodiert (Drehung {rotation}°)");
    with(|s| {
        // gespeicherte SPS/PPS zuerst, dann ab dem nächsten Schlüsselbild
        if !s.codec_config.is_empty() {
            let _ = tx.try_send(s.codec_config.clone());
        }
        s.need_key = true;
        // letztes Bild bleibt kurz stehen (z. B. beim Drehen), bis das neue kommt
        s.decoder = Some(Decoder { child, tx });
    });
    request_key();
}

// --- virtuelle Kamera (usbcd_cam.dll) --------------------------------------------------

#[cfg(windows)]
mod dll {
    use std::ffi::c_void;
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        LOAD_LIBRARY_SEARCH_SYSTEM32,
    };

    type StartFn = unsafe extern "system" fn(*const u16, *mut *mut c_void) -> i32;
    type StopFn = unsafe extern "system" fn(*mut c_void);

    pub struct Cam {
        handle: *mut c_void,
        stop: StopFn,
    }
    unsafe impl Send for Cam {}

    impl Drop for Cam {
        fn drop(&mut self) {
            unsafe { (self.stop)(self.handle) };
        }
    }

    pub fn start(name: &str) -> Result<Cam, String> {
        let path = crate::config::exe_dir().join("usbcd_cam.dll");
        if !path.exists() {
            return Err(format!("{} fehlt", path.display()));
        }
        let p = crate::util::wide(&path.to_string_lossy());
        unsafe {
            let m = LoadLibraryExW(
                p.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            );
            if m.is_null() {
                let e = windows_sys::Win32::Foundation::GetLastError();
                // 127: MFCreateVirtualCamera fehlt (Windows 10)
                return Err(if e == 127 {
                    crate::i18n::tr(
                        "Diese Windows-Version kennt keine virtuellen Kameras (ab Windows 11)",
                        "This Windows version has no virtual cameras (Windows 11 or newer)",
                    )
                    .to_string()
                } else {
                    format!("usbcd_cam.dll lädt nicht (Fehler {e})")
                });
            }
            let s = GetProcAddress(m, c"UsbcdCamStart".as_ptr().cast());
            let t = GetProcAddress(m, c"UsbcdCamStop".as_ptr().cast());
            let (Some(s), Some(t)) = (s, t) else {
                return Err("usbcd_cam.dll ist veraltet".into());
            };
            let start: StartFn =
                std::mem::transmute::<unsafe extern "system" fn() -> isize, StartFn>(s);
            let stop: StopFn =
                std::mem::transmute::<unsafe extern "system" fn() -> isize, StopFn>(t);
            let n = crate::util::wide(name);
            let mut h: *mut c_void = std::ptr::null_mut();
            let hr = start(n.as_ptr(), &mut h);
            if hr < 0 || h.is_null() {
                return Err(format!("{hr:#010x}", hr = hr as u32));
            }
            Ok(Cam { handle: h, stop })
        }
    }
}

#[cfg(not(windows))]
mod dll {
    pub struct Cam;
    pub fn start(_name: &str) -> Result<Cam, String> {
        Err("nur unter Windows".into())
    }
}

/// Legt die Kamera an bzw. entfernt sie, wenn sich der Menüschalter ändert.
fn manager() {
    let mut cam: Option<dll::Cam> = None;
    let mut pipe_started = false;
    let mut failed_for = false;
    loop {
        let enabled = {
            let g = shared()
                .st
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let have = cam.is_some();
            let (g, _) = shared()
                .cv
                .wait_timeout_while(g, Duration::from_secs(1), |s| {
                    let todo = s.enabled != have && !(s.enabled && failed_for);
                    !todo && !RETRY.load(Ordering::SeqCst)
                })
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            g.enabled
        };
        if crate::util::shutdown() {
            break;
        }
        if !enabled || RETRY.swap(false, Ordering::SeqCst) {
            failed_for = false;
        }
        if enabled && cam.is_none() && !failed_for {
            match dll::start(camera_name()) {
                Ok(c) => {
                    crate::log!("Webcam „{}“ angelegt", camera_name());
                    cam = Some(c);
                    with(|s| s.created = true);
                    if !pipe_started {
                        pipe_started = true;
                        std::thread::Builder::new()
                            .name("webcam-pipe".into())
                            .spawn(pipe_server)
                            .ok();
                    }
                    if ANNOUNCE.swap(false, Ordering::SeqCst) {
                        crate::util::notify(
                            "camon",
                            tr("Webcam eingeschaltet", "Webcam turned on"),
                            &format!(
                                "{} „{}“. {}",
                                tr(
                                    "In Teams, Discord, Zoom oder im Browser als Kamera wählen:",
                                    "Choose it as the camera in Teams, Discord, Zoom or the browser:"
                                ),
                                camera_name(),
                                tr(
                                    "Die Selfie-Kamera des Tablets läuft nur, während eine App sie benutzt.",
                                    "The tablet's selfie camera only runs while an app uses it."
                                )
                            ),
                            Duration::ZERO,
                        );
                    }
                }
                Err(e) => {
                    failed_for = true;
                    ANNOUNCE.store(false, Ordering::SeqCst);
                    crate::util::notify(
                        "camera",
                        tr("Webcam nicht verfügbar", "Webcam not available"),
                        &format!(
                            "{}: {e}",
                            tr(
                                "Die USBCD-Kamera konnte nicht angelegt werden",
                                "The USBCD camera could not be created"
                            )
                        ),
                        Duration::from_secs(600),
                    );
                }
            }
        } else if !enabled && cam.is_some() {
            cam = None; // Drop entfernt die Kamera aus Windows
            with(|s| {
                s.created = false;
                s.consumer = false;
            });
            crate::log!("Webcam entfernt");
        }
    }
}

// --- Pipe zum Windows-Kameradienst ----------------------------------------------------

#[cfg(windows)]
fn poke_pipe() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_GENERIC_READ, OPEN_EXISTING};
    let name = crate::util::wide(PIPE_NAME);
    unsafe {
        let h = CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_READ,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            CloseHandle(h);
        }
    }
}

#[cfg(not(windows))]
fn poke_pipe() {}

/// Welches Bild gerade an die Webcam geht.
fn current_frame(s: &mut St) -> Arc<Vec<u8>> {
    if let (TabletCam::On { .. }, Some((f, t))) = (&s.tablet, &s.latest) {
        if t.elapsed() < Duration::from_secs(1) {
            return f.clone();
        }
    }
    let (status, detail) = placeholder_text(&s.tablet);
    let key = format!("{status}\n{detail}");
    if let Some((k, f)) = &s.placeholder {
        if *k == key {
            return f.clone();
        }
    }
    let f = Arc::new(render_placeholder(status, detail));
    s.placeholder = Some((key, f.clone()));
    f
}

/// (Statuszeile, Hinweis darunter) – wie der Wartebildschirm auf dem Tablet.
fn placeholder_text(t: &TabletCam) -> (&'static str, &'static str) {
    if crate::util::paused() {
        return (
            tr("Tablet ausgeschaltet", "Tablet switched off"),
            tr(
                "Wieder einschalten: USBCD-Menü › Tablet wieder einschalten",
                "Switch on again: USBCD menu › Switch tablet back on",
            ),
        );
    }
    match t {
        TabletCam::Disconnected => (
            tr("Tablet nicht verbunden", "Tablet not connected"),
            tr(
                "Tablet per USB-C mit dem PC verbinden",
                "Connect the tablet to the PC via USB-C",
            ),
        ),
        TabletCam::Starting | TabletCam::Off | TabletCam::On { .. } => {
            (tr("Kamera wird gestartet …", "Starting camera …"), "")
        }
        TabletCam::Asking => (
            tr(
                "Bitte am Tablet den Kamerazugriff erlauben",
                "Please allow camera access on the tablet",
            ),
            "",
        ),
        TabletCam::Denied => (
            tr(
                "Kamerazugriff am Tablet nicht erlaubt",
                "Camera access not allowed on the tablet",
            ),
            tr(
                "Tablet: Einstellungen › Apps › USBCD › Berechtigungen › Kamera",
                "Tablet: Settings › Apps › USBCD › Permissions › Camera",
            ),
        ),
        TabletCam::NoFrontCamera => (
            tr(
                "Dieses Tablet hat keine Frontkamera",
                "This tablet has no front camera",
            ),
            "",
        ),
        TabletCam::Error => (
            tr(
                "Tablet-Kamera nicht verfügbar",
                "Tablet camera not available",
            ),
            tr(
                "Vielleicht benutzt eine andere App auf dem Tablet die Kamera",
                "Another app on the tablet may be using the camera",
            ),
        ),
    }
}

#[cfg(windows)]
fn pipe_server() {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_PIPE_CONNECTED,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{
        WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_OUTBOUND,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    let sddl = crate::util::wide(PIPE_SDDL);
    let mut sd: *mut c_void = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        crate::log!("Webcam: Sicherheitsbeschreibung ungültig");
        return;
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let name = crate::util::wide(PIPE_NAME);
    let head = header();
    let mut warned = false;
    while !crate::util::shutdown() {
        let enabled = with(|s| s.enabled && s.created);
        if !enabled {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                (FRAME_LEN + 4096) as u32,
                0,
                0,
                &sa,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            let e = unsafe { GetLastError() };
            if !warned {
                warned = true;
                crate::log!("Webcam: Pipe belegt oder nicht anlegbar (Fehler {e})");
                if e == ERROR_ACCESS_DENIED {
                    crate::util::notify(
                        "security",
                        tr("Sicherheitswarnung", "Security warning"),
                        tr(
                            "Ein anderes Programm belegt den Anschluss der USBCD-Webcam. Die Webcam bleibt aus. Bitte unbekannte Programme prüfen.",
                            "Another program occupies the USBCD webcam connection. The webcam stays off. Please check for unknown programs.",
                        ),
                        Duration::from_secs(3600),
                    );
                }
            }
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        warned = false;
        let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if connected && with(|s| s.enabled && s.created) && !crate::util::shutdown() {
            crate::log!("Webcam: eine App benutzt die Kamera");
            // Tablet-Kamera läuft evtl. noch (App hat kurz neu verbunden) → gleich dekodieren
            let running = with(|s| {
                s.consumer = true;
                match (&s.tablet, s.decoder.is_none()) {
                    (TabletCam::On { rotation, .. }, true) => Some(*rotation),
                    _ => None,
                }
            });
            if let Some(r) = running {
                start_decoder(r);
            }
            let mut sent = u64::MAX;
            loop {
                let frame = {
                    let g = shared()
                        .st
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let (mut g, _) = shared()
                        .cv
                        .wait_timeout_while(g, Duration::from_millis(200), |s| {
                            s.seq == sent && s.enabled
                        })
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !g.enabled || crate::util::shutdown() {
                        break;
                    }
                    sent = g.seq;
                    current_frame(&mut g)
                };
                let mut n = 0u32;
                let ok = unsafe {
                    WriteFile(pipe, head.as_ptr(), head.len() as u32, &mut n, null_mut()) != 0
                        && WriteFile(pipe, frame.as_ptr(), frame.len() as u32, &mut n, null_mut())
                            != 0
                };
                if !ok {
                    break;
                }
            }
            with(|s| {
                s.consumer = false;
                s.decoder = None; // FFmpeg beenden, keine Kamerabilder mehr annehmen
                s.latest = None;
            });
            crate::log!("Webcam: keine App benutzt die Kamera mehr");
        }
        unsafe {
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
        }
    }
    unsafe { LocalFree(sd as _) };
}

#[cfg(not(windows))]
fn pipe_server() {}

// --- Hinweisbild ----------------------------------------------------------------------
//
// Gleiches Design wie der Wartebildschirm der Tablet-App: heller Verlauf (#F1F1F1 ↔ #FFFFFF),
// Logo, „USBCD“ in Space Grotesk, Status in #1F1F1F, Hinweis in #6B6B6B.
// Hinweis: Discord, Teams usw. zeigen das eigene Bild gespiegelt (wie einen Spiegel) –
// beim Gegenüber ist die Schrift richtig herum.

const LOGO_SIZE: usize = 128;
/// USBCD-Logo (installer/USBCD-logo.png), 128×128 BGRA mit Transparenz
static LOGO: &[u8] = include_bytes!("../assets/logo-128.bgra");
/// Space Grotesk (SIL OFL 1.1) als feste Schnitte, umbenannt in „USBCD Grotesk …“
static FONT_BOLD: &[u8] = include_bytes!("../assets/USBCDGrotesk-Bold.ttf");
static FONT_MEDIUM: &[u8] = include_bytes!("../assets/USBCDGrotesk-Medium.ttf");
static FONT_REGULAR: &[u8] = include_bytes!("../assets/USBCDGrotesk-Regular.ttf");

/// Abstände (Pixel bei 1280×720)
const GAP_LOGO: i32 = 34;
const TITLE_PX: i32 = 56;
const GAP_TITLE: i32 = 22;
const STATUS_PX: i32 = 30;
const GAP_STATUS: i32 = 14;
const DETAIL_PX: i32 = 22;
const DETAIL_LINE: i32 = 30;

/// Oberkante des Inhaltsblocks (senkrecht zentriert).
fn layout_top(detail_lines: usize) -> i32 {
    let mut total = LOGO_SIZE as i32 + GAP_LOGO + TITLE_PX + GAP_TITLE + STATUS_PX;
    if detail_lines > 0 {
        total += GAP_STATUS + detail_lines as i32 * DETAIL_LINE;
    }
    (HEIGHT as i32 - total) / 2
}

/// Hintergrund und Logo als BGRA.
fn background(top: i32) -> Vec<u8> {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut v = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            // diagonal: hell – weiß – hell
            let t = x as f32 / w as f32 * 0.6 + y as f32 / h as f32 * 0.4;
            let k = 1.0 - (t - 0.5).abs() * 2.0;
            let c = (241.0 + 14.0 * k).round() as u8;
            let p = (y * w + x) * 4;
            v[p..p + 4].copy_from_slice(&[c, c, c, 255]);
        }
    }
    let x0 = (w - LOGO_SIZE) / 2;
    let y0 = top.max(0) as usize;
    for ly in 0..LOGO_SIZE {
        for lx in 0..LOGO_SIZE {
            let s = (ly * LOGO_SIZE + lx) * 4;
            let Some(src) = LOGO.get(s..s + 4) else {
                continue;
            };
            let a = src[3] as u32;
            if a == 0 || y0 + ly >= h {
                continue;
            }
            let d = ((y0 + ly) * w + x0 + lx) * 4;
            for i in 0..3 {
                let dst = v[d + i] as u32;
                v[d + i] = ((src[i] as u32 * a + dst * (255 - a) + 127) / 255) as u8;
            }
        }
    }
    v
}

/// Hinweisbild als NV12.
fn render_placeholder(status: &str, detail: &str) -> Vec<u8> {
    let lines: Vec<&str> = detail.lines().filter(|l| !l.is_empty()).collect();
    let top = layout_top(lines.len());
    let mut bgra = background(top);
    draw_text(&mut bgra, top, status, &lines);
    bgra_to_nv12(&bgra, WIDTH as usize, HEIGHT as usize)
}

#[cfg(windows)]
fn load_fonts() {
    use windows_sys::Win32::Graphics::Gdi::AddFontMemResourceEx;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for f in [FONT_BOLD, FONT_MEDIUM, FONT_REGULAR] {
            let n = 0u32;
            // nur für USBCD sichtbar, wird nicht in Windows installiert
            let h = unsafe {
                AddFontMemResourceEx(f.as_ptr().cast(), f.len() as u32, std::ptr::null(), &n)
            };
            if h.is_null() {
                crate::log!("Webcam: Schrift nicht ladbar – nutze Segoe UI");
            }
        }
    });
}

/// Schreibt Titel, Status und Hinweis in das Bild (GDI, geglättet).
#[cfg(windows)]
fn draw_text(bgra: &mut [u8], top: i32, status: &str, lines: &[&str]) {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
        GdiFlush, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
        DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, FF_SWISS, HFONT,
        OUT_TT_PRECIS, TRANSPARENT,
    };
    load_fonts();
    unsafe {
        let dc = CreateCompatibleDC(null_mut());
        if dc.is_null() {
            return;
        }
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = WIDTH as i32;
        bmi.bmiHeader.biHeight = -(HEIGHT as i32); // von oben nach unten
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut std::ffi::c_void = null_mut();
        let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bmp.is_null() || bits.is_null() {
            DeleteDC(dc);
            return;
        }
        let len = bgra.len().min((WIDTH * HEIGHT * 4) as usize);
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, len);
        let old = SelectObject(dc, bmp);
        SetBkMode(dc, TRANSPARENT as i32);

        let font = |face: &str, px: i32| {
            let f = crate::util::wide(face);
            CreateFontW(
                -px,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_TT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                ANTIALIASED_QUALITY as u32,
                (DEFAULT_PITCH | FF_SWISS) as u32,
                f.as_ptr(),
            )
        };
        let line = |txt: &str, f: HFONT, color: u32, y: i32, h: i32| {
            let prev = SelectObject(dc, f);
            SetTextColor(dc, color);
            let mut w = crate::util::wide(txt);
            let mut r = RECT {
                left: 40,
                top: y,
                right: WIDTH as i32 - 40,
                bottom: y + h + h / 2,
            };
            DrawTextW(
                dc,
                w.as_mut_ptr(),
                -1,
                &mut r,
                DT_CENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            SelectObject(dc, prev);
        };
        let bold = font("USBCD Grotesk Bold", TITLE_PX);
        let medium = font("USBCD Grotesk Medium", STATUS_PX);
        let regular = font("USBCD Grotesk Regular", DETAIL_PX);
        const PRIMARY: u32 = 0x001f_1f1f; // COLORREF (BGR)
        const SECONDARY: u32 = 0x006b_6b6b;
        let mut y = top + LOGO_SIZE as i32 + GAP_LOGO;
        line("USBCD", bold, PRIMARY, y, TITLE_PX);
        y += TITLE_PX + GAP_TITLE;
        line(status, medium, PRIMARY, y, STATUS_PX);
        y += STATUS_PX + GAP_STATUS;
        for l in lines {
            line(l, regular, SECONDARY, y, DETAIL_PX);
            y += DETAIL_LINE;
        }
        GdiFlush();
        std::ptr::copy_nonoverlapping(bits as *const u8, bgra.as_mut_ptr(), len);
        SelectObject(dc, old);
        for f in [bold, medium, regular] {
            DeleteObject(f);
        }
        DeleteObject(bmp);
        DeleteDC(dc);
    }
}

#[cfg(not(windows))]
fn draw_text(_bgra: &mut [u8], _top: i32, _status: &str, _lines: &[&str]) {}

/// BGRA → NV12 (BT.601, begrenzter Bereich wie bei Webcams üblich).
pub fn bgra_to_nv12(bgra: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 3 / 2];
    let (y_plane, uv) = out.split_at_mut(w * h);
    for (i, px) in bgra.chunks_exact(4).take(w * h).enumerate() {
        let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
        y_plane[i] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16).clamp(0, 255) as u8;
    }
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut r, mut g, mut b) = (0i32, 0i32, 0i32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = ((y + dy).min(h - 1) * w + (x + dx).min(w - 1)) * 4;
                b += bgra[p] as i32;
                g += bgra[p + 1] as i32;
                r += bgra[p + 2] as i32;
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            let i = (y / 2) * w + x;
            uv[i] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            uv[i + 1] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_colors() {
        // Weiß → Y 235, Grau neutral (U/V 128)
        let white = vec![255u8; 4 * 4 * 4];
        let n = bgra_to_nv12(&white, 4, 4);
        assert_eq!(n.len(), 24);
        assert_eq!(n[0], 235);
        assert_eq!(n[16], 128);
        assert_eq!(n[17], 128);
        // Schwarz → Y 16
        let black = vec![0u8; 4 * 4 * 4];
        assert_eq!(bgra_to_nv12(&black, 4, 4)[0], 16);
    }

    #[test]
    fn placeholder_size() {
        assert_eq!(render_placeholder("a", "b").len(), FRAME_LEN);
        assert_eq!(LOGO.len(), LOGO_SIZE * LOGO_SIZE * 4);
        // Inhalt passt mit zwei Hinweiszeilen ins Bild
        assert!(layout_top(2) > 40);
    }

    #[test]
    fn filters() {
        assert!(filter(0).starts_with("scale="));
        assert!(filter(90).starts_with("transpose=clock,"));
        assert!(filter(180).starts_with("hflip,vflip,"));
        assert!(filter(270).starts_with("transpose=cclock,"));
        let a = decoder_args(0);
        assert_eq!(a.last().map(String::as_str), Some("pipe:1"));
    }

    #[test]
    fn header_layout() {
        let h = header();
        assert_eq!(&h[..4], b"UCAM");
        assert_eq!(
            u32::from_le_bytes([h[12], h[13], h[14], h[15]]) as usize,
            FRAME_LEN
        );
    }
}
