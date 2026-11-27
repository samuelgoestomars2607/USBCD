//! Ton: Der virtuelle Lautsprecher (VB-CABLE, in Windows „USBCD Tablet Lautsprecher“) wird
//! per WASAPI-Loopback abgegriffen und als PCM zum Tablet geschickt. Umbenennen und
//! Ausblenden brauchen Administratorrechte und laufen im Hilfsdienst (helper.rs).
//!
//! Minimal-COM ohne zusätzliche Crates: Methoden werden direkt über die vtable
//! aufgerufen (Indizes laut Windows-SDK-Headern).

use crate::config::Config;
use crate::proto::{write_frame, T_AFMT, T_PCM};
use crate::win::wide;
use anyhow::{bail, Result};
use std::ffi::c_void;
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
};

#[allow(clippy::upper_case_acronyms)]
type HRESULT = i32;
type P = *mut c_void;

const CLSID_MM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);
const IID_IMM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xA95664D2_9614_4F35_A746_DE8DB63617E6);
const IID_IAUDIO_CLIENT: GUID = GUID::from_u128(0x1CB9AD4C_DBFA_4C32_B178_C2F568A703B2);
const IID_IAUDIO_CLIENT2: GUID = GUID::from_u128(0x726778CD_F60A_4EDA_82DE_E47610CD78AA);
const IID_IAUDIO_CAPTURE_CLIENT: GUID = GUID::from_u128(0xC8ADBD64_E71E_48A0_A4DE_185C395CD317);
const IID_IAUDIO_ENDPOINT_VOLUME: GUID = GUID::from_u128(0x5CDF2C82_841E_4546_9722_0CF74078229A);
const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870AF99C_171D_4F9E_AF0D_E63DF40C2BC9);
const IID_IPOLICY_CONFIG: GUID = GUID::from_u128(0xF8679F50_850A_41CF_9C72_430F290290C8);

#[repr(C)]
struct PropertyKey {
    fmtid: GUID,
    pid: u32,
}
const PKEY_DEVICE_DEVICE_DESC: PropertyKey = PropertyKey {
    fmtid: GUID::from_u128(0xA45C254E_DF1C_4EFD_8020_67D146A850E0),
    pid: 2,
};
const PKEY_DEVICE_FRIENDLY_NAME: PropertyKey = PropertyKey {
    fmtid: GUID::from_u128(0xA45C254E_DF1C_4EFD_8020_67D146A850E0),
    pid: 14,
};

#[repr(C)]
struct PropVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    data: [usize; 2],
}

const E_RENDER: i32 = 0;
const E_CAPTURE: i32 = 1;
const DEVICE_STATE_ACTIVE: u32 = 1;
const DEVICE_STATE_DISABLED: u32 = 2;
const AUDCLNT_STREAMFLAGS_LOOPBACK: u32 = 0x0002_0000;
/// Windows meldet neue Daten per Ereignis (Loopback ab Windows 10 1703)
const AUDCLNT_STREAMFLAGS_EVENTCALLBACK: u32 = 0x0004_0000;
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;
const AUDCLNT_STREAMOPTIONS_POST_VOLUME_LOOPBACK: i32 = 0x8;

// Eigentümer eines COM-Zeigers (siehe win.rs)
use crate::win::ComPtr as Com;

fn check(hr: HRESULT, what: &str) -> Result<()> {
    if hr < 0 {
        bail!("{what} fehlgeschlagen (HRESULT 0x{:08X})", hr as u32);
    }
    Ok(())
}

unsafe fn take_wstr(p: *mut u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
    CoTaskMemFree(p as *const c_void);
    s
}

struct Enumerator(Com);

struct Endpoint {
    dev: Com,
    id: String,
    name: String,
}

impl Enumerator {
    fn new() -> Result<Self> {
        unsafe {
            let mut p: P = std::ptr::null_mut();
            check(
                CoCreateInstance(
                    &CLSID_MM_DEVICE_ENUMERATOR,
                    std::ptr::null_mut(),
                    CLSCTX_ALL,
                    &IID_IMM_DEVICE_ENUMERATOR,
                    &mut p,
                ),
                "MMDeviceEnumerator",
            )?;
            Ok(Self(Com(p)))
        }
    }

    unsafe fn device_id(dev: &Com) -> String {
        let get_id: unsafe extern "system" fn(P, *mut *mut u16) -> HRESULT = dev.f(5);
        let mut s: *mut u16 = std::ptr::null_mut();
        if get_id(dev.0, &mut s) < 0 {
            return String::new();
        }
        take_wstr(s)
    }

    unsafe fn friendly_name(dev: &Com) -> String {
        let open: unsafe extern "system" fn(P, u32, *mut P) -> HRESULT = dev.f(4);
        let mut store: P = std::ptr::null_mut();
        if open(dev.0, 0, &mut store) < 0 {
            return String::new();
        }
        let store = Com(store);
        let get_value: unsafe extern "system" fn(
            P,
            *const PropertyKey,
            *mut PropVariant,
        ) -> HRESULT = store.f(5);
        let mut pv: PropVariant = std::mem::zeroed();
        if get_value(store.0, &PKEY_DEVICE_FRIENDLY_NAME, &mut pv) < 0 || pv.vt != 31 {
            return String::new();
        }
        take_wstr(pv.data[0] as *mut u16)
    }

    /// Benennt den Endpunkt um (wie "Umbenennen" in den Windows-Soundeinstellungen).
    /// Braucht Administratorrechte; schlägt still fehl, wenn nicht möglich.
    unsafe fn rename(dev: &Com, name: &str) -> bool {
        let open: unsafe extern "system" fn(P, u32, *mut P) -> HRESULT = dev.f(4);
        let mut store: P = std::ptr::null_mut();
        if open(dev.0, 2 /* STGM_READWRITE */, &mut store) < 0 {
            return false;
        }
        let store = Com(store);
        let set_value: unsafe extern "system" fn(
            P,
            *const PropertyKey,
            *const PropVariant,
        ) -> HRESULT = store.f(6);
        let commit: unsafe extern "system" fn(P) -> HRESULT = store.f(7);
        let w = wide(name);
        let mut pv: PropVariant = std::mem::zeroed();
        pv.vt = 31; // VT_LPWSTR
        pv.data[0] = w.as_ptr() as usize;
        set_value(store.0, &PKEY_DEVICE_DEVICE_DESC, &pv) >= 0 && commit(store.0) >= 0
    }

    /// Alle aktiven Wiedergabegeräte.
    fn render_endpoints(&self) -> Result<Vec<Endpoint>> {
        self.endpoints(E_RENDER, DEVICE_STATE_ACTIVE)
    }

    fn endpoints(&self, flow: i32, states: u32) -> Result<Vec<Endpoint>> {
        unsafe {
            let enum_eps: unsafe extern "system" fn(P, i32, u32, *mut P) -> HRESULT = self.0.f(3);
            let mut coll: P = std::ptr::null_mut();
            check(
                enum_eps(self.0 .0, flow, states, &mut coll),
                "EnumAudioEndpoints",
            )?;
            let coll = Com(coll);
            let get_count: unsafe extern "system" fn(P, *mut u32) -> HRESULT = coll.f(3);
            let item: unsafe extern "system" fn(P, u32, *mut P) -> HRESULT = coll.f(4);
            let mut n = 0u32;
            check(get_count(coll.0, &mut n), "GetCount")?;
            let mut v = Vec::new();
            for i in 0..n {
                let mut d: P = std::ptr::null_mut();
                if item(coll.0, i, &mut d) < 0 {
                    continue;
                }
                let dev = Com(d);
                let id = Self::device_id(&dev);
                let name = Self::friendly_name(&dev);
                v.push(Endpoint { dev, id, name });
            }
            Ok(v)
        }
    }

    fn default_render_id(&self, role: i32) -> Option<String> {
        unsafe {
            let get_default: unsafe extern "system" fn(P, i32, i32, *mut P) -> HRESULT =
                self.0.f(4);
            let mut d: P = std::ptr::null_mut();
            if get_default(self.0 .0, E_RENDER, role, &mut d) < 0 {
                return None;
            }
            let dev = Com(d);
            Some(Self::device_id(&dev))
        }
    }
}

/// Setzt das Standard-Wiedergabegerät (undokumentiertes, aber seit Windows 7
/// stabiles IPolicyConfig – so macht es auch die Windows-Soundsteuerung).
fn set_default_endpoint(id: &str, role: i32) -> Result<()> {
    // IPolicyConfig hat keinen Proxy/Stub – sicherheitshalber in eigenem STA-Thread aufrufen
    let id = id.to_string();
    std::thread::spawn(move || unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let r = set_default_endpoint_sta(&id, role);
        CoUninitialize();
        r
    })
    .join()
    .unwrap_or_else(|_| bail!("PolicyConfig-Thread abgestürzt"))
}

fn set_default_endpoint_sta(id: &str, role: i32) -> Result<()> {
    unsafe {
        let mut p: P = std::ptr::null_mut();
        check(
            CoCreateInstance(
                &CLSID_POLICY_CONFIG_CLIENT,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IID_IPOLICY_CONFIG,
                &mut p,
            ),
            "PolicyConfig",
        )?;
        let pc = Com(p);
        let set: unsafe extern "system" fn(P, *const u16, i32) -> HRESULT = pc.f(13);
        let w = wide(id);
        check(set(pc.0, w.as_ptr(), role), "SetDefaultEndpoint")
    }
}

/// Gerät ein-/ausblenden (entspricht „Aktivieren/Deaktivieren“ in der Windows-
/// Soundsteuerung, jederzeit umkehrbar). Braucht Administratorrechte.
fn set_visibility(id: &str, visible: bool) -> bool {
    let id = id.to_string();
    std::thread::spawn(move || unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let mut p: P = std::ptr::null_mut();
        let mut ok = false;
        if CoCreateInstance(
            &CLSID_POLICY_CONFIG_CLIENT,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IPOLICY_CONFIG,
            &mut p,
        ) >= 0
        {
            let pc = Com(p);
            let set_vis: unsafe extern "system" fn(P, *const u16, i32) -> HRESULT = pc.f(14);
            let w = wide(&id);
            ok = set_vis(pc.0, w.as_ptr(), visible as i32) >= 0;
        }
        CoUninitialize();
        ok
    })
    .join()
    .unwrap_or(false)
}

fn is_cable(name: &str) -> bool {
    name.to_lowercase().contains("vb-audio virtual cable")
}

/// Im Hilfsdienst: Lautsprecher benennen, „CABLE In 16ch“ ausblenden (dieselbe Leitung
/// mit 16 Kanälen, verwirrt nur in der Liste) und – falls gewünscht – die Aufnahmeseite
/// „CABLE Output“, damit Programme den Tablet-Ton nicht als „Mikrofon“ aufnehmen.
pub fn prepare_system(name: &str, hide_output: bool) -> Result<String> {
    let en = Enumerator::new()?;
    let mut report = Vec::new();
    for e in en.render_endpoints()? {
        let n = e.name.to_lowercase();
        if is_cable(&n) && n.contains("16ch") || n.contains("cable in 16") {
            let ok = set_visibility(&e.id, false);
            report.push(format!(
                "{} {}",
                e.name,
                if ok {
                    "ausgeblendet"
                } else {
                    "nicht ausblendbar"
                }
            ));
        }
    }
    if hide_output {
        for e in en.endpoints(E_CAPTURE, DEVICE_STATE_ACTIVE)? {
            if is_cable(&e.name) {
                let ok = set_visibility(&e.id, false);
                report.push(format!(
                    "{} {}",
                    e.name,
                    if ok {
                        "ausgeblendet"
                    } else {
                        "nicht ausblendbar"
                    }
                ));
            }
        }
    } else {
        for e in en.endpoints(E_CAPTURE, DEVICE_STATE_DISABLED)? {
            if is_cable(&e.name) && !e.name.to_lowercase().contains("16ch") {
                let ok = set_visibility(&e.id, true);
                report.push(format!(
                    "{} {}",
                    e.name,
                    if ok {
                        "eingeblendet"
                    } else {
                        "nicht einblendbar"
                    }
                ));
            }
        }
    }
    let name = name.trim();
    if !name.is_empty() {
        let patterns = Config::default().audio_device_match;
        if let Ok(ep) = find_endpoint(&patterns, &en) {
            if !ep.name.starts_with(name) {
                let ok = unsafe { Enumerator::rename(&ep.dev, name) };
                report.push(format!(
                    "\"{}\" → \"{name}\" {}",
                    ep.name,
                    if ok { "umbenannt" } else { "nicht umbenennbar" }
                ));
            }
        }
    }
    for r in &report {
        crate::log!("Audio: {r}");
    }
    Ok(report.join("; "))
}

/// Beim Deinstallieren (VB-CABLE bleibt): alles zurück auf den Auslieferungszustand –
/// Name „CABLE Input“, „CABLE In 16ch“ und „CABLE Output“ wieder eingeblendet.
pub fn restore_system() -> Result<String> {
    let en = Enumerator::new()?;
    let mut report = Vec::new();
    for flow in [E_RENDER, E_CAPTURE] {
        for e in en.endpoints(flow, DEVICE_STATE_DISABLED)? {
            if is_cable(&e.name) {
                let ok = set_visibility(&e.id, true);
                report.push(format!(
                    "{} {}",
                    e.name,
                    if ok {
                        "eingeblendet"
                    } else {
                        "nicht einblendbar"
                    }
                ));
            }
        }
    }
    for e in en.render_endpoints()? {
        if is_cable(&e.name) && !e.name.starts_with("CABLE") {
            let ok = unsafe { Enumerator::rename(&e.dev, "CABLE Input") };
            report.push(format!(
                "\"{}\" → \"CABLE Input\" {}",
                e.name,
                if ok { "umbenannt" } else { "nicht umbenennbar" }
            ));
        }
    }
    for r in &report {
        crate::log!("Audio: {r}");
    }
    Ok(report.join("; "))
}

pub struct AudioSession {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl AudioSession {
    pub fn start(cfg: &Config) -> AudioSession {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let cfg = cfg.clone();
        let join = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
                if let Err(e) = audio_thread(&cfg, &s2) {
                    crate::log!("Audio: {e:#}");
                }
                unsafe { CoUninitialize() };
            })
            .ok();
        AudioSession { stop, join }
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct Restore(Vec<(i32, String)>);
impl Drop for Restore {
    fn drop(&mut self) {
        for (role, id) in &self.0 {
            if let Err(e) = set_default_endpoint(id, *role) {
                crate::log!("Audio: Standardgerät nicht zurückgesetzt: {e:#}");
            }
        }
        if !self.0.is_empty() {
            crate::log!("Audio: vorheriges Standard-Wiedergabegerät wiederhergestellt");
        }
    }
}

/// Sucht den virtuellen Lautsprecher.
fn find_endpoint(patterns: &[String], en: &Enumerator) -> Result<Endpoint> {
    let eps = en.render_endpoints()?;

    // Muster in Reihenfolge prüfen; Mehrkanal-Varianten ("CABLE In 16 Ch") meiden
    let pats: Vec<String> = patterns.iter().map(|s| s.to_lowercase()).collect();
    let mut eps = eps;
    eps.sort_by_key(|e| e.name.to_lowercase().contains("16 ch"));
    let pos = pats.iter().filter(|p| !p.is_empty()).find_map(|p| {
        eps.iter()
            .position(|e| e.name.to_lowercase().contains(p.as_str()))
    });
    let Some(ep) = pos.map(|i| eps.swap_remove(i)) else {
        bail!(
            "{} ({patterns:?})",
            crate::i18n::tr(
                "kein Tablet-Lautsprecher gefunden – VB-CABLE fehlt, USBCD-Setup erneut ausführen",
                "no tablet speaker found – VB-CABLE missing, run the USBCD setup again"
            )
        );
    };
    Ok(ep)
}

/// Beim Programmstart: Lautsprecher in „USBCD Tablet Lautsprecher“ umbenennen (über den
/// Hilfsdienst), damit man ihn in Windows leicht findet. Der Standard-Ausgang bleibt
/// unverändert.
pub fn prepare_endpoint(cfg: &Config) {
    if !cfg.audio_enabled {
        return;
    }
    if let Err(e) = crate::helper::speaker_prepare(cfg.hide_cable_output, &cfg.audio_device_rename)
    {
        crate::log!("Audio vorbereiten: {e:#}");
    }
    let cfg = cfg.clone();
    let _ = std::thread::spawn(move || unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32);
        match Enumerator::new().and_then(|en| find_endpoint(&cfg.audio_device_match, &en)) {
            Ok(ep) => crate::log!("Audio: Tablet-Lautsprecher = \"{}\"", ep.name),
            Err(e) => crate::log!("Audio: {e:#}"),
        }
        CoUninitialize();
    })
    .join();
}

fn audio_thread(cfg: &Config, stop: &AtomicBool) -> Result<()> {
    let en = Enumerator::new()?;
    let ep = find_endpoint(&cfg.audio_device_match, &en)?;

    // Standardgerät umstellen (Konsole + Multimedia), alten Zustand merken
    let mut restore = Restore(Vec::new());
    if cfg.auto_switch_audio {
        for role in [0, 1] {
            if let Some(old) = en.default_render_id(role) {
                if old != ep.id && set_default_endpoint(&ep.id, role).is_ok() {
                    restore.0.push((role, old));
                }
            }
        }
    }

    while !stop.load(Ordering::SeqCst) {
        if let Ok(mut sock) = TcpStream::connect(("127.0.0.1", cfg.audio_port)) {
            let _ = sock.set_nodelay(true);
            let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));
            match crate::auth::handshake(&mut sock, crate::auth::token()) {
                Ok(()) => {
                    let _ = sock.set_read_timeout(None);
                    if let Err(e) = capture_loop(cfg, &ep.dev, sock, stop) {
                        crate::log!("Audio-Verbindung beendet: {e:#}");
                    }
                }
                Err(crate::auth::AuthError::NoApp) => {}
                Err(e) => {
                    crate::log!("Audio: Anmeldung am Tablet fehlgeschlagen ({e:?})");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    drop(restore);
    Ok(())
}

unsafe fn rd<T: Copy>(base: *const u8, off: usize) -> T {
    std::ptr::read_unaligned(base.add(off) as *const T)
}

/// Ton-Thread bei Windows als Audio-Aufgabe anmelden (MMCSS): Er bekommt dann Vorrang,
/// wenn die CPU voll ist – der Ton knackt nicht. Wird beim Verlassen zurückgenommen.
struct MmcssAudio(*mut c_void);

impl MmcssAudio {
    fn register() -> MmcssAudio {
        use windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
        let mut idx = 0u32;
        let h = unsafe { AvSetMmThreadCharacteristicsW(wide("Audio").as_ptr(), &mut idx) };
        if h.is_null() {
            crate::log!(
                "Audio: Windows-Audio-Vorrang (MMCSS) nicht verfügbar – läuft normal weiter"
            );
        }
        MmcssAudio(h)
    }
}

impl Drop for MmcssAudio {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics(self.0)
            };
        }
    }
}

// Ereignis für „neue Ton-Daten“ (wird beim Verlassen geschlossen, win.rs)
use crate::win::Handle as EventHandle;

fn capture_loop(cfg: &Config, dev: &Com, mut sock: TcpStream, stop: &AtomicBool) -> Result<()> {
    let _mmcss = MmcssAudio::register();
    crate::process::high_qos_current_thread();
    unsafe {
        let activate: unsafe extern "system" fn(
            P,
            *const GUID,
            u32,
            *const c_void,
            *mut P,
        ) -> HRESULT = dev.f(3);

        // IAudioClient2 bevorzugt: damit lässt sich "nach Lautstärke" abgreifen,
        // dann wirkt der Windows-Lautstärkeregler automatisch.
        let mut p: P = std::ptr::null_mut();
        let mut post_volume = false;
        let client = if activate(
            dev.0,
            &IID_IAUDIO_CLIENT2,
            CLSCTX_ALL,
            std::ptr::null(),
            &mut p,
        ) >= 0
        {
            let c = Com(p);
            if cfg.volume_mode == "auto" {
                #[repr(C)]
                struct Props {
                    cb: u32,
                    offload: i32,
                    category: i32,
                    options: i32,
                }
                let props = Props {
                    cb: 16,
                    offload: 0,
                    category: 0,
                    options: AUDCLNT_STREAMOPTIONS_POST_VOLUME_LOOPBACK,
                };
                let set_props: unsafe extern "system" fn(P, *const Props) -> HRESULT = c.f(16);
                post_volume = set_props(c.0, &props) >= 0;
            }
            c
        } else {
            check(
                activate(
                    dev.0,
                    &IID_IAUDIO_CLIENT,
                    CLSCTX_ALL,
                    std::ptr::null(),
                    &mut p,
                ),
                "IAudioClient",
            )?;
            Com(p)
        };

        let manual_volume = match cfg.volume_mode.as_str() {
            "off" => false,
            "manual" => true,
            _ => !post_volume,
        };
        let volume = if manual_volume {
            let mut v: P = std::ptr::null_mut();
            if activate(
                dev.0,
                &IID_IAUDIO_ENDPOINT_VOLUME,
                CLSCTX_ALL,
                std::ptr::null(),
                &mut v,
            ) >= 0
            {
                Some(Com(v))
            } else {
                None
            }
        } else {
            None
        };

        let get_mix: unsafe extern "system" fn(P, *mut *mut u8) -> HRESULT = client.f(8);
        let mut fmt: *mut u8 = std::ptr::null_mut();
        check(get_mix(client.0, &mut fmt), "GetMixFormat")?;
        let tag: u16 = rd(fmt, 0);
        let channels: u16 = rd(fmt, 2);
        let rate: u32 = rd(fmt, 4);
        let bits: u16 = rd(fmt, 14);
        let is_float = match tag {
            3 => true,
            0xFFFE => rd::<u32>(fmt, 24) == 3,
            _ => false,
        };
        let init: unsafe extern "system" fn(
            P,
            i32,
            u32,
            i64,
            i64,
            *const u8,
            *const GUID,
        ) -> HRESULT = client.f(3);
        // Bevorzugt ereignisgesteuert: Windows weckt den Thread, sobald Daten da sind, statt
        // 500-mal pro Sekunde nachzufragen (spart CPU-Aufwachen und Akku). Lehnt Windows das
        // ab, wie bisher mit regelmäßigem Nachfragen.
        let event = EventHandle(windows_sys::Win32::System::Threading::CreateEventW(
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
        ));
        let evented = !event.0.is_null()
            && init(
                client.0,
                0,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                200_000,
                0,
                fmt,
                std::ptr::null(),
            ) >= 0;
        let hr = if evented {
            0
        } else {
            init(
                client.0,
                0,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                200_000,
                0,
                fmt,
                std::ptr::null(),
            )
        };
        CoTaskMemFree(fmt as *const c_void);
        check(hr, "IAudioClient::Initialize (Loopback)")?;
        if evented {
            let set_event: unsafe extern "system" fn(P, *mut c_void) -> HRESULT = client.f(13);
            check(set_event(client.0, event.0), "IAudioClient::SetEventHandle")?;
        }

        let get_service: unsafe extern "system" fn(P, *const GUID, *mut P) -> HRESULT =
            client.f(14);
        let mut cp: P = std::ptr::null_mut();
        check(
            get_service(client.0, &IID_IAUDIO_CAPTURE_CLIENT, &mut cp),
            "IAudioCaptureClient",
        )?;
        let cap = Com(cp);
        let get_buffer: unsafe extern "system" fn(
            P,
            *mut *mut u8,
            *mut u32,
            *mut u32,
            *mut u64,
            *mut u64,
        ) -> HRESULT = cap.f(3);
        let release_buffer: unsafe extern "system" fn(P, u32) -> HRESULT = cap.f(4);
        let next_size: unsafe extern "system" fn(P, *mut u32) -> HRESULT = cap.f(5);

        let out_ch: u16 = channels.clamp(1, 2);
        crate::log!(
            "Audio: {rate} Hz, {channels} Kanäle, {} → Tablet {out_ch} Kanäle; {}; Lautstärke {}",
            if is_float { "float" } else { "int" },
            if evented {
                "ereignisgesteuert"
            } else {
                "regelmäßig abgefragt"
            },
            if post_volume {
                "von Windows angewendet"
            } else if manual_volume {
                "gespiegelt"
            } else {
                "unverändert"
            }
        );
        let afmt = serde_json::json!({ "rate": rate, "channels": out_ch }).to_string();
        write_frame(&mut sock, T_AFMT, afmt.as_bytes())?;

        let start: unsafe extern "system" fn(P) -> HRESULT = client.f(10);
        let stop_fn: unsafe extern "system" fn(P) -> HRESULT = client.f(11);
        check(start(client.0), "IAudioClient::Start")?;

        let mut gain = 1.0f32;
        // None = noch nie abgefragt (Instant minus Dauer kann kurz nach dem Windows-Start scheitern)
        let mut last_vol: Option<Instant> = None;
        let batch_frames = (rate / 200) as usize; // ~5 ms
        let mut pcm: Vec<u8> = Vec::with_capacity(batch_frames * 4 * 2);
        let bytes_per_sample = (bits / 8) as usize;
        let frame_bytes = bytes_per_sample * channels as usize;

        let result = loop {
            if stop.load(Ordering::SeqCst) {
                break Ok(());
            }
            if let Some(v) = &volume {
                if last_vol.is_none_or(|t| t.elapsed() > Duration::from_millis(100)) {
                    last_vol = Some(Instant::now());
                    let get_db: unsafe extern "system" fn(P, *mut f32) -> HRESULT = v.f(8);
                    let get_mute: unsafe extern "system" fn(P, *mut i32) -> HRESULT = v.f(15);
                    let (mut db, mut mute) = (0f32, 0i32);
                    get_db(v.0, &mut db);
                    get_mute(v.0, &mut mute);
                    gain = if mute != 0 {
                        0.0
                    } else {
                        10f32.powf(db / 20.0)
                    };
                }
            }
            let mut sent_err = None;
            loop {
                let mut n = 0u32;
                if next_size(cap.0, &mut n) < 0 || n == 0 {
                    break;
                }
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                if get_buffer(
                    cap.0,
                    &mut data,
                    &mut frames,
                    &mut flags,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) < 0
                {
                    break;
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null();
                for f in 0..frames as usize {
                    for c in 0..out_ch as usize {
                        let s = if silent {
                            0.0
                        } else {
                            let off = f * frame_bytes + c * bytes_per_sample;
                            match (is_float, bits) {
                                (true, 32) => rd::<f32>(data, off),
                                (false, 16) => rd::<i16>(data, off) as f32 / 32768.0,
                                (false, 32) => rd::<i32>(data, off) as f32 / 2147483648.0,
                                (false, 24) => {
                                    let b = [
                                        0,
                                        rd::<u8>(data, off),
                                        rd::<u8>(data, off + 1),
                                        rd::<u8>(data, off + 2),
                                    ];
                                    i32::from_le_bytes(b) as f32 / 2147483648.0
                                }
                                _ => 0.0,
                            }
                        };
                        let v = (s * gain).clamp(-1.0, 1.0);
                        pcm.extend_from_slice(&((v * 32767.0) as i16).to_le_bytes());
                    }
                }
                release_buffer(cap.0, frames);
                if pcm.len() >= batch_frames * out_ch as usize * 2 {
                    if let Err(e) = write_frame(&mut sock, T_PCM, &pcm) {
                        sent_err = Some(e);
                        break;
                    }
                    pcm.clear();
                }
            }
            if let Some(e) = sent_err {
                break Err(e.into());
            }
            let _ = sock.flush();
            if evented {
                // Zeitgrenze: Bei Stille liefert Windows im Loopback keine Daten – Beenden und
                // Lautstärke werden trotzdem regelmäßig geprüft
                windows_sys::Win32::System::Threading::WaitForSingleObject(event.0, 20);
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        stop_fn(client.0);
        result
    }
}

// --- Lautstärketasten des Tablets -------------------------------------------------------
//
// Die Tasten am Tablet regeln den „USBCD Tablet Lautsprecher“ in Windows – derselbe Regler
// wie in den Windows-Soundeinstellungen. Da der Ton nach der Windows-Lautstärke abgegriffen
// wird, gibt es so nur noch einen Lautstärkeregler statt zwei hintereinander.

pub const KEY_UP: u8 = 1;
pub const KEY_DOWN: u8 = 2;
pub const KEY_MUTE: u8 = 3;

/// Warteschlange zum Lautstärke-Thread (eigener COM-Thread, blockiert nie den Lese-Thread)
static VOLUME_TX: std::sync::Mutex<Option<std::sync::mpsc::Sender<u8>>> =
    std::sync::Mutex::new(None);
/// Neueste Lautstärke für die Anzeige auf dem Tablet (JSON), wird im Video-Thread gesendet
static VOLUME_REPORT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Lautstärketaste vom Tablet (Nachricht VOLUME_KEY).
pub fn volume_key(p: &[u8]) {
    let [key] = p else { return };
    if !matches!(*key, KEY_UP | KEY_DOWN | KEY_MUTE) {
        return;
    }
    let c = crate::config::current();
    if !(c.tablet_volume_keys && c.audio_enabled && c.volume_mode != "off") {
        return;
    }
    let Ok(mut g) = VOLUME_TX.lock() else { return };
    if let Some(tx) = g.as_ref() {
        if tx.send(*key).is_ok() {
            return;
        }
    }
    let (tx, rx) = std::sync::mpsc::channel::<u8>();
    let spawned = std::thread::Builder::new()
        .name("volume".into())
        .spawn(move || volume_thread(rx));
    if spawned.is_ok() && tx.send(*key).is_ok() {
        *g = Some(tx);
    }
}

/// Für den Video-Thread: Lautstärke, die das Tablet anzeigen soll (einmalig).
pub fn take_volume_report() -> Option<String> {
    VOLUME_REPORT.lock().ok()?.take()
}

/// Neue Lautstärke (0.0–1.0) und Stummschaltung nach einem Tastendruck.
/// Schritte von 2 % wie bei den Lautstärketasten einer Windows-Tastatur.
fn volume_step(level: f32, muted: bool, key: u8) -> (f32, bool) {
    let steps = (level.clamp(0.0, 1.0) * 50.0).round();
    match key {
        KEY_UP => (((steps + 1.0) / 50.0).min(1.0), false),
        KEY_DOWN => (((steps - 1.0) / 50.0).max(0.0), muted),
        _ => (steps / 50.0, !muted),
    }
}

fn volume_thread(rx: std::sync::mpsc::Receiver<u8>) {
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    let mut ev: Option<Com> = None;
    while let Ok(key) = rx.recv() {
        // Gerät kann sich ändern (VB-CABLE neu installiert): bei Fehler einmal neu suchen
        let mut done = false;
        for _ in 0..2 {
            if ev.is_none() {
                ev = match endpoint_volume() {
                    Ok(v) => Some(v),
                    Err(e) => {
                        crate::log!("Lautstärketaste: {e:#}");
                        break;
                    }
                };
            }
            if let Some(v) = ev.as_ref() {
                match unsafe { apply_key(v, key) } {
                    Ok((level, muted)) => {
                        let j = serde_json::json!({
                            "level": (level * 100.0).round() as u32,
                            "muted": muted,
                        });
                        if let Ok(mut r) = VOLUME_REPORT.lock() {
                            *r = Some(j.to_string());
                        }
                        done = true;
                        break;
                    }
                    Err(_) => ev = None,
                }
            }
        }
        if !done {
            crate::log!("Lautstärketaste: Tablet-Lautsprecher nicht erreichbar");
        }
    }
    drop(ev);
    unsafe { CoUninitialize() };
}

/// IAudioEndpointVolume des Tablet-Lautsprechers.
fn endpoint_volume() -> Result<Com> {
    let cfg = crate::config::current();
    let en = Enumerator::new()?;
    let ep = find_endpoint(&cfg.audio_device_match, &en)?;
    unsafe {
        let activate: unsafe extern "system" fn(
            P,
            *const GUID,
            u32,
            *const c_void,
            *mut P,
        ) -> HRESULT = ep.dev.f(3);
        let mut v: P = std::ptr::null_mut();
        check(
            activate(
                ep.dev.0,
                &IID_IAUDIO_ENDPOINT_VOLUME,
                CLSCTX_ALL,
                std::ptr::null(),
                &mut v,
            ),
            "IAudioEndpointVolume",
        )?;
        Ok(Com(v))
    }
}

/// Wendet eine Taste an; liefert die neue Lautstärke und Stummschaltung.
unsafe fn apply_key(v: &Com, key: u8) -> Result<(f32, bool)> {
    // vtable-Indizes laut endpointvolume.h
    let set_level: unsafe extern "system" fn(P, f32, *const GUID) -> HRESULT = v.f(7);
    let get_level: unsafe extern "system" fn(P, *mut f32) -> HRESULT = v.f(9);
    let set_mute: unsafe extern "system" fn(P, i32, *const GUID) -> HRESULT = v.f(14);
    let get_mute: unsafe extern "system" fn(P, *mut i32) -> HRESULT = v.f(15);
    let mut level = 0f32;
    let mut mute = 0i32;
    check(get_level(v.0, &mut level), "GetMasterVolumeLevelScalar")?;
    check(get_mute(v.0, &mut mute), "GetMute")?;
    let muted = mute != 0;
    let (new_level, new_muted) = volume_step(level, muted, key);
    if (new_level - level).abs() > f32::EPSILON {
        check(
            set_level(v.0, new_level, std::ptr::null()),
            "SetMasterVolumeLevelScalar",
        )?;
    }
    if new_muted != muted {
        check(set_mute(v.0, new_muted as i32, std::ptr::null()), "SetMute")?;
    }
    Ok((new_level, new_muted))
}

/// Zum Testen: alle Wiedergabegeräte auflisten (`usbcd --list-audio`).
pub fn list_devices() -> Result<Vec<String>> {
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    let en = Enumerator::new()?;
    Ok(en.render_endpoints()?.into_iter().map(|e| e.name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_steps() {
        // 2-%-Schritte, gerundet, an den Grenzen festgehalten
        assert_eq!(volume_step(0.47, false, KEY_UP), (0.5, false));
        assert_eq!(volume_step(0.50, false, KEY_DOWN), (0.48, false));
        assert_eq!(volume_step(1.0, false, KEY_UP), (1.0, false));
        assert_eq!(volume_step(0.0, false, KEY_DOWN), (0.0, false));
        // lauter hebt die Stummschaltung auf (wie bei Windows), leiser nicht
        assert_eq!(volume_step(0.3, true, KEY_UP), (0.32, false));
        assert_eq!(volume_step(0.3, true, KEY_DOWN), (0.28, true));
        // Stumm schaltet um und lässt die Lautstärke stehen
        assert_eq!(volume_step(0.3, false, KEY_MUTE), (0.3, true));
        assert_eq!(volume_step(0.3, true, KEY_MUTE), (0.3, false));
    }
}
