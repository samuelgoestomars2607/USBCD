//! Ablauf für ein angestecktes Tablet: App starten → Display-Infos empfangen →
//! virtuellen Monitor mit Tablet-Auflösung/-Bildrate einrichten → Ton umleiten →
//! streamen, bis das Kabel gezogen wird → alles zurückbauen.

use crate::adb::Adb;
use crate::audio::AudioSession;
use crate::auth::{self, AuthError};
use crate::config::Config;
use crate::display;
use crate::i18n::tr;
use crate::nal::Codec;
use crate::proto::{read_frame_max, write_frame, Hello, T_CONFIG, T_HELLO};
use crate::video::{self, Encoder, StreamEnd, StreamParams};
use anyhow::{bail, Context, Result};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Die USBCD-App auf dem Tablet ist nicht geöffnet (z. B. vom Nutzer geschlossen).
#[derive(Debug)]
pub struct AppNotRunning;
impl std::fmt::Display for AppNotRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("USBCD-App ist auf dem Tablet nicht geöffnet")
    }
}
impl std::error::Error for AppNotRunning {}

/// Verbindung abgerissen (Kabel gezogen, App geschlossen oder kurzer USB-Aussetzer).
#[derive(Debug)]
pub struct Disconnected;
impl std::fmt::Display for Disconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Verbindung zum Tablet getrennt")
    }
}
impl std::error::Error for Disconnected {}

/// Windows zeigt den Tablet-Monitor nicht als eigenen Bildschirm an (z. B. gespiegelt)
/// und er ließ sich auch nicht gezielt anhängen.
#[derive(Debug)]
pub struct NotExtended;
impl std::fmt::Display for NotExtended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tablet-Monitor ist nicht Teil des Desktops (nicht „Erweitern“)")
    }
}
impl std::error::Error for NotExtended {}

/// Im Menü „Tablet ausschalten“ gewählt.
#[derive(Debug)]
pub struct Paused;
impl std::fmt::Display for Paused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tablet ausgeschaltet")
    }
}
impl std::error::Error for Paused {}

/// Die App läuft, kennt aber das Geheimnis dieser USBCD-Sitzung nicht (z. B. von
/// Hand geöffnet, bevor USBCD lief) → mit Geheimnis neu starten.
#[derive(Debug)]
struct NeedsToken;
impl std::fmt::Display for NeedsToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("App kennt das Sitzungsgeheimnis nicht")
    }
}
impl std::error::Error for NeedsToken {}

pub enum Outcome {
    AppNotRunning,
    Disconnected,
    Paused,
    /// Tablet-Monitor nicht im Desktop (gespiegelt) – Nutzer kann „Erweitern“ wählen
    NotExtended,
    Failed(anyhow::Error),
}

struct Cleanup<'a> {
    cfg: &'a Config,
    adb: &'a Adb,
    serial: String,
    display_on: bool,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        // Den virtuellen Monitor schaltet die Hauptschleife aus – erst wenn das
        // Tablet wirklich weg ist, nicht bei kurzen USB-Aussetzern (sonst springen Fenster).
        let _ = self.display_on;
        let _ = self.adb.on(
            &self.serial,
            &[
                "forward",
                "--remove",
                &format!("tcp:{}", self.cfg.video_port),
            ],
        );
        let _ = self.adb.on(
            &self.serial,
            &[
                "forward",
                "--remove",
                &format!("tcp:{}", self.cfg.audio_port),
            ],
        );
    }
}

fn connect_hello(cfg: &Config, launched: bool) -> Result<(TcpStream, Hello)> {
    // Nach dem Starten der App bis zu 20 s warten, sonst nur kurz nachsehen
    let deadline = Instant::now() + Duration::from_millis(if launched { 20_000 } else { 2_500 });
    let mut rejected = 0;
    loop {
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", cfg.video_port)) {
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            match auth::handshake(&mut s, auth::token()) {
                Ok(()) => {
                    if let Ok((T_HELLO, payload)) = read_frame_max(&mut s, 64 << 10) {
                        let mut hello: Hello =
                            serde_json::from_slice(&payload).context("HELLO ungültig")?;
                        if let Err(e) = hello.validate() {
                            bail!(
                                "{}: {e}",
                                tr(
                                    "Ungültige Display-Daten vom Tablet",
                                    "Invalid display data from the tablet"
                                )
                            );
                        }
                        let _ = s.set_read_timeout(None);
                        let _ = s.set_nodelay(true);
                        return Ok((s, hello));
                    }
                }
                Err(AuthError::NoApp) => {}
                Err(AuthError::Outdated) => bail!(
                    "{}",
                    tr(
                        "Die USBCD-App auf dem Tablet ist veraltet – bitte das Tablet neu anstecken, dann wird sie aktualisiert",
                        "The USBCD app on the tablet is outdated – please reconnect the tablet so it gets updated"
                    )
                ),
                Err(AuthError::Rejected) => {
                    if !launched {
                        return Err(NeedsToken.into());
                    }
                    // direkt nach dem Start kann die App das neue Geheimnis noch nicht haben
                    rejected += 1;
                    if rejected >= 8 {
                        bail!(
                            "{}",
                            tr(
                                "Die USBCD-App auf dem Tablet lehnt die Anmeldung ab. Bitte die App auf dem Tablet schließen und das Tablet neu anstecken.",
                                "The USBCD app on the tablet rejects the sign-in. Please close the app on the tablet and reconnect the tablet."
                            )
                        );
                    }
                }
                Err(AuthError::Impostor) => bail!(
                    "{}",
                    tr(
                        "Sicherheitswarnung: Eine andere App auf dem Tablet gibt sich als USBCD aus. Es wurde nichts übertragen. Bitte unbekannte Apps auf dem Tablet prüfen.",
                        "Security warning: another app on the tablet pretends to be USBCD. Nothing was transmitted. Please check the tablet for unknown apps."
                    )
                ),
            }
        }
        if crate::util::shutdown() {
            return Err(Disconnected.into());
        }
        if Instant::now() > deadline {
            if !launched {
                return Err(AppNotRunning.into());
            }
            bail!(
                "{}",
                tr(
                    "Die USBCD-App auf dem Tablet antwortet nicht – ist das Tablet entsperrt?",
                    "The USBCD app on the tablet does not respond – is the tablet unlocked?"
                )
            );
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// Startet die App und übergibt ihr das Geheimnis dieser USBCD-Sitzung.
fn start_app(cfg: &Config, adb: &Adb, serial: &str) -> Result<()> {
    // ".Connect" darf nur die adb-Shell öffnen – nur darüber nimmt die App das Geheimnis an
    let activity = format!("{}/.Connect", cfg.android_package);
    let token = auth::token_hex();
    let out = adb.on(
        serial,
        &[
            "shell",
            "am",
            "start",
            "-n",
            &activity,
            "--es",
            "usbcd_token",
            &token,
        ],
    )?;
    if out.contains("Error") || out.contains("Exception") {
        bail!("App-Start: {}", out.trim());
    }
    Ok(())
}

/// Wählt Codec und Bildrate anhand der Decoder-Fähigkeiten des Tablets.
pub fn choose(cfg: &Config, hello: &Hello, enc: Encoder) -> (Codec, u32) {
    let mut target = hello.refresh.round().max(24.0) as u32;
    if cfg.max_fps > 0 {
        target = target.min(cfg.max_fps);
    }
    let order: Vec<Codec> = match (cfg.codec.as_str(), enc) {
        ("hevc", _) => vec![Codec::Hevc],
        ("avc", _) | (_, Encoder::Software) => vec![Codec::Avc],
        _ => vec![Codec::Hevc, Codec::Avc],
    };
    let caps = |c: Codec| hello.codecs.get(c.name()).cloned().unwrap_or_default();
    // 1. Codec, der die volle Bildrate schafft
    for &c in &order {
        let k = caps(c);
        if k.supported && k.max_fps + 1.0 >= target as f32 {
            return (c, target);
        }
    }
    // 2. sonst der mit der höchsten möglichen Bildrate
    let best = order
        .iter()
        .copied()
        .filter(|&c| caps(c).supported)
        .max_by(|a, b| caps(*a).max_fps.total_cmp(&caps(*b).max_fps));
    match best {
        Some(c) => (c, (caps(c).max_fps.floor() as u32).clamp(24, target)),
        None => (order[0], target),
    }
}

/// `launch`: App auf dem Tablet starten (nur direkt nach dem Anstecken bzw. auf
/// Wunsch über das Menü) – sonst nur verbinden, falls sie ohnehin geöffnet ist.
pub fn run(cfg: &Config, adb: &Adb, serial: &str, launch: bool) -> Outcome {
    match run_inner(cfg, adb, serial, launch) {
        Ok(()) => Outcome::Disconnected,
        Err(e) if e.downcast_ref::<AppNotRunning>().is_some() => Outcome::AppNotRunning,
        Err(e) if e.downcast_ref::<Disconnected>().is_some() => Outcome::Disconnected,
        Err(e) if e.downcast_ref::<Paused>().is_some() => Outcome::Paused,
        Err(e) if e.downcast_ref::<NotExtended>().is_some() => Outcome::NotExtended,
        Err(e) => Outcome::Failed(e),
    }
}

fn run_inner(cfg: &Config, adb: &Adb, serial: &str, launch: bool) -> Result<()> {
    if crate::util::paused() {
        return Err(Paused.into());
    }
    let mut cleanup = Cleanup {
        cfg,
        adb,
        serial: serial.to_string(),
        display_on: false,
    };

    for port in [cfg.video_port, cfg.audio_port] {
        adb.on(
            serial,
            &["forward", &format!("tcp:{port}"), &format!("tcp:{port}")],
        )?;
    }
    if launch {
        start_app(cfg, adb, serial)?;
    }

    let (mut sock, hello) = match connect_hello(cfg, launch) {
        Err(e) if e.downcast_ref::<NeedsToken>().is_some() => {
            crate::log!("Tablet-App kennt das Sitzungsgeheimnis nicht – wird damit neu gestartet");
            start_app(cfg, adb, serial)?;
            connect_hello(cfg, true)?
        }
        r => r?,
    };
    let name = if hello.name.trim().is_empty() {
        hello.model.clone()
    } else {
        hello.name.clone()
    };
    crate::log!(
        "Tablet: {} – {}x{} @ {:.1} Hz, {} dpi, Decoder: {:?}",
        hello.model,
        hello.width,
        hello.height,
        hello.refresh,
        hello.density_dpi,
        hello.codecs
    );
    let (w, h) = (hello.width, hello.height);
    let hz = hello.refresh.round() as u32;

    // --- virtueller Monitor ---
    // Hintergrundbilder merken, bevor sich die Anordnung ändert
    crate::wallpaper::remember();
    if crate::helper::vdd_on(w, h, hz)? {
        crate::log!("Virtueller Monitor auf {w}x{h}@{hz} eingestellt, Treiber neu geladen");
    }
    cleanup.display_on = true;

    let deadline = Instant::now() + Duration::from_secs(15);
    let vd = loop {
        if let Some(d) = display::find_virtual_display(&cfg.display_match) {
            break d;
        }
        if Instant::now() > deadline {
            bail!(
                "{}",
                crate::i18n::tr(
                    "Der virtuelle Monitor erscheint nicht – display_match in usbcd.toml prüfen",
                    "The virtual monitor does not appear – check display_match in usbcd.toml"
                )
            );
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    crate::log!(
        "Virtueller Monitor: {} ({})",
        vd.device_name,
        vd.description
    );
    // Windows erst selbst anhängen lassen – dabei stellt es die gemerkte Anordnung her
    let mut vd = display::wait_attached(&vd, Duration::from_secs(4));
    if !vd.attached {
        // Gespiegelt oder „Nur PC-Bildschirm“ (häufig bei Laptops): nur den Tablet-Monitor
        // als erweiterten Bildschirm anhängen, die übrigen Bildschirme bleiben unverändert
        match display::attach_extended(&vd) {
            Ok(()) => {
                vd = display::wait_attached(&vd, Duration::from_secs(4));
                crate::log!(
                    "Tablet-Monitor war nicht Teil des Desktops – als erweiterter Bildschirm angehängt: {}",
                    vd.attached
                );
            }
            Err(e) => crate::log!("Tablet-Monitor anhängen: {e:#}"),
        }
    }
    // Eine in Windows gewählte kleinere Auflösung im Seitenverhältnis des Tablets bleibt
    // (Windows merkt sie sich), sonst die des Tablets
    let (tw, th, thz) = match display::mode_of(&vd.device_name) {
        Some(m) if vd.attached && crate::vdd::offered_modes(w, h, hz).contains(&m) => m,
        _ => (w, h, hz),
    };
    if let Err(e) = display::set_mode(&vd, tw, th, thz, cfg.display_position) {
        crate::log!(
            "Modus setzen: {e:#} – aktueller Modus: {:?}",
            display::mode_of(&vd.device_name)
        );
    }

    let deadline = Instant::now() + Duration::from_secs(8);
    let hmon = loop {
        if let Some(hm) = display::hmonitor_for(&vd.device_name) {
            break hm;
        }
        if Instant::now() > deadline {
            crate::log!("{} ist nicht Teil des Desktops", vd.device_name);
            return Err(NotExtended.into());
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    if let Some((mw, mh, mhz)) = display::mode_of(&vd.device_name) {
        crate::log!("Windows-Modus des Tablet-Monitors: {mw}x{mh} @ {mhz} Hz");
    }
    // in der Windows-Auflösung übertragen; ist sie kleiner, rechnet das Tablet hoch
    let (sw, sh) = stream_size(&vd.device_name, w, h, hz);
    if (sw, sh) != (w, h) {
        crate::log!("Übertragung in {sw}x{sh}, das Tablet rechnet auf {w}x{h} hoch");
    }

    // Windows verliert bei neuen Monitoren gern das Hintergrundbild → wiederherstellen
    crate::wallpaper::restore_later();
    // Position merken, wenn der Nutzer den Monitor verschiebt
    let _layout = LayoutWatcher::start(vd.clone());
    // Touch/Stift vom Tablet annehmen, Laufzeit messen
    let _input = InputReader::start(&sock, vd.clone())?;
    // Tablet-Display mit dem PC-Bildschirm aus- und einschalten
    let _power = PowerWatcher::start(adb, serial);
    let _active = ActiveDevice::set(adb, serial);

    // --- Ton ---
    let mut audio = if cfg.audio_enabled {
        Some(AudioSession::start(cfg))
    } else {
        None
    };

    // --- Video ---
    // Encoder des Grafikchips, der den Tablet-Monitor zeichnet (Laptops mit zwei Chips);
    // unbekannt → wie bisher der Chip des Hauptbildschirms
    let render = display::render_gpu(&vd.device_name);
    let fallback_encoder = Encoder::pick(cfg, display::primary_gpu_vendor());
    let encoder = match render.as_ref().map(|g| display::GpuVendor::from_pci(g.vendor_id)) {
        Some(v) if v != display::GpuVendor::Unknown => Encoder::pick(cfg, v),
        _ => fallback_encoder,
    };
    let capture_gpu = render
        .as_ref()
        .map(|g| encoder.vendor_id().unwrap_or(g.vendor_id));
    if let Some(g) = &render {
        crate::log!(
            "Grafikchip des Tablet-Monitors: {} (0x{:04x}) → Aufnahme und {encoder:?} dort{}",
            g.name,
            g.vendor_id,
            if encoder != fallback_encoder {
                format!(" (statt {fallback_encoder:?} des Hauptbildschirms)")
            } else {
                String::new()
            }
        );
    }
    let (codec, fps) = choose(cfg, &hello, encoder);
    let bitrate = video::bitrate_for(cfg, sw, sh, fps, codec);
    crate::util::set_status(&format!(
        "{}: {name} · {sw}×{sh} · {fps} Hz",
        crate::i18n::tr("Verbunden", "Connected")
    ));
    crate::util::notify(
        "connected",
        crate::i18n::tr("Tablet verbunden", "Tablet connected"),
        &format!(
            "{name} · {sw}×{sh} · {fps} Hz\n{} „{}“.",
            crate::i18n::tr(
                "Ton übers Tablet: Soundeinstellungen →",
                "Sound on the tablet: Sound settings →"
            ),
            cfg.audio_device_rename
        ),
        Duration::from_secs(120),
    );
    crate::log!(
        "Stream: {codec:?} über {encoder:?}, {sw}x{sh} @ {fps} fps, {} Mbit/s",
        bitrate / 1_000_000
    );

    let mut params = StreamParams {
        hmonitor: hmon,
        width: sw,
        height: sh,
        fps,
        codec,
        encoder,
        bitrate,
        capture_gpu,
        dup_frames: !sparse_allowed(&cfg_serial_prompt(cfg, serial)),
        // erstes Verbinden dieses Tablets: erst den Dekoder prüfen
        probe: cfg.sparse_frames
            && cfg_serial_prompt(cfg, serial).is_none()
            && video::has_dup_frames(),
        native: is_native(&vd.device_name, sw, sh),
        pixel_exact: is_native(&vd.device_name, sw, sh) && (sw, sh) == (w, h),
    };
    video::set_sparse(!params.dup_frames);
    video::set_probing(params.probe);
    if params.probe {
        crate::log!("Erstes Verbinden: Dekoder des Tablets wird geprüft");
        // ohne Antwort (z. B. ältere App) nach 6 s normal weiter
        let generation = PROBE_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(6));
            if PROBE_GEN.load(std::sync::atomic::Ordering::SeqCst) == generation && video::is_probing() {
                crate::log!("Dekoder-Prüfung ohne Ergebnis – gleichmäßiger Bildstrom");
                video::set_probing(false);
                crate::util::request_reconnect();
            }
        });
    }
    if !params.dup_frames {
        crate::log!("Bilder nur bei Änderungen (Dekoder des Tablets gibt jedes Bild sofort aus)");
    }
    let mut restarts = 0u32;
    let mut cur = cfg.clone();
    loop {
        // erstes Verbinden dieses Tablets: Dekoder wird geprüft, danach ggf. umgestellt –
        // das Tablet zeigt so lange den Verbindungsbildschirm (kein sichtbarer Ruckler)
        let check = params.probe;
        let conf = serde_json::json!({
            "codec": codec.name(),
            "width": params.width,
            "height": params.height,
            "fps": fps,
            "check": check,
        })
        .to_string();
        if write_frame(&mut sock, T_CONFIG, conf.as_bytes()).is_err()
            || video::send_settings(&mut sock).is_err()
        {
            return Err(Disconnected.into());
        }
        crate::util::take_reconnect(); // alte Anfragen verwerfen – diese Übertragung ist frisch
        let started = Instant::now();
        match video::stream(&cur, &params, &mut sock)? {
            StreamEnd::EncoderExited => {
                if started.elapsed() > Duration::from_secs(20) {
                    restarts = 0;
                }
                restarts += 1;
                if restarts > 5 {
                    bail!(
                        "{}",
                        crate::i18n::tr(
                            "Die Bildübertragung (FFmpeg) bricht ständig ab – encoder = \"software\" in usbcd.toml testen",
                            "The picture transmission (FFmpeg) keeps stopping – try encoder = \"software\" in usbcd.toml"
                        )
                    );
                }
                crate::log!("ffmpeg beendet – Neustart ({restarts}/5)");
                // Aufnahme auf dem Grafikchip des Tablet-Monitors klappt nicht → zurück zur
                // bisherigen Wahl (Windows-Standard, Encoder des Hauptbildschirms)
                if restarts >= 2 && params.capture_gpu.is_some() {
                    params.capture_gpu = None;
                    if fallback_encoder != Encoder::Software {
                        params.encoder = fallback_encoder;
                    }
                    crate::log!(
                        "Aufnahme auf dem Grafikchip des Tablet-Monitors ausgeschaltet – weiter mit {:?}",
                        params.encoder
                    );
                }
                // Einmaliger Absturz bei unverändertem Monitor (z. B. Intel „Invalid
                // FrameType“): sofort neu starten. Sonst kurz warten – Windows kann den
                // Monitor gerade neu anlegen (Anordnung geändert, Grafiktreiber neu gestartet).
                let same_monitor = display::hmonitor_for(&vd.device_name) == Some(params.hmonitor);
                std::thread::sleep(Duration::from_millis(if restarts == 1 && same_monitor {
                    50
                } else {
                    800
                }));
                // dann gilt ggf. ein neues Monitor-Handle
                if let Some(hm) = display::hmonitor_for(&vd.device_name) {
                    if hm != params.hmonitor {
                        crate::log!("Tablet-Monitor neu erkannt");
                        params.hmonitor = hm;
                    }
                }
                resize(&mut params, &cur, &vd.device_name, (w, h, hz));
            }
            StreamEnd::Restart => {
                // neue Einstellungen übernehmen, ohne Verbindung und Monitor anzufassen
                cur = crate::config::current();
                if params.probe && !video::is_probing() {
                    params.probe = false; // Prüfung beendet → normale Bildrate
                }
                let dup = !sparse_allowed(&cfg_serial_prompt(&cur, serial));
                if dup != params.dup_frames {
                    crate::log!(
                        "{}",
                        if dup {
                            "Wieder gleichmäßiger Bildstrom (Dekoder hält Bilder fest)"
                        } else {
                            "Bilder nur bei Änderungen (Dekoder des Tablets gibt jedes Bild sofort aus)"
                        }
                    );
                    params.dup_frames = dup;
                    video::set_sparse(!dup);
                }
                resize(&mut params, &cur, &vd.device_name, (w, h, hz));
                params.bitrate = video::bitrate_for(&cur, params.width, params.height, fps, codec);
                if cur.audio_enabled != audio.is_some() {
                    audio = None; // alte Ton-Übertragung zuerst beenden
                    if cur.audio_enabled {
                        audio = Some(AudioSession::start(&cur));
                    }
                }
                crate::log!(
                    "Neue Einstellungen: {} Mbit/s, Ton {}",
                    params.bitrate / 1_000_000,
                    if cur.audio_enabled { "an" } else { "aus" }
                );
            }
        }
    }
}

/// Prüfergebnis des Tablet-Dekoders für dieses Gerät (None = noch nicht geprüft).
fn cfg_serial_prompt(cfg: &Config, serial: &str) -> Option<bool> {
    cfg.devices.get(serial).and_then(|d| d.decoder_prompt)
}

/// „Nur bei Änderungen“ nur, wenn eingeschaltet und der Dekoder nachweislich jedes Bild
/// sofort ausgibt – sonst bliebe das letzte Bild hängen, bis das nächste kommt.
fn sparse_allowed(prompt: &Option<bool>) -> bool {
    crate::config::current().sparse_frames && *prompt == Some(true)
}

/// Generation der Dekoder-Prüfung (Zeitgrenze gilt nur für die eigene Verbindung)
static PROBE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Tablet meldet, ob sein Dekoder jedes Bild sofort ausgibt.
///
/// Entschieden wird einmal, bei der Prüfung beim ersten Verbinden (5 Bilder/s, eindeutig
/// messbar). Danach zählt nur noch „hält Bilder fest“ als Sicherung: hing im Betrieb ein
/// Bild, geht es zurück zum gleichmäßigen Bildstrom – nie von selbst wieder zurück.
fn on_decoder_report(p: &[u8]) {
    let Ok(j) = serde_json::from_slice::<serde_json::Value>(p) else { return };
    let Some(prompt) = j.get("prompt").and_then(|v| v.as_bool()) else { return };
    let Some(serial) = ACTIVE.lock().ok().and_then(|g| g.as_ref().map(|(_, s)| s.clone())) else {
        return;
    };
    let store = |prompt: bool| {
        crate::config::update(|c| {
            if let Some(d) = c.devices.get_mut(&serial) {
                d.decoder_prompt = Some(prompt);
            }
        });
    };
    if video::is_probing() {
        crate::log!(
            "Dekoder des Tablets geprüft: {}",
            if prompt { "gibt jedes Bild sofort aus" } else { "hält Bilder fest" }
        );
        store(prompt);
        video::set_probing(false);
        crate::util::request_reconnect(); // Prüfung beenden, passende Übertragungsart
    } else if !prompt && video::is_sparse() {
        crate::log!("Ein Bild hing im Dekoder – zurück zum gleichmäßigen Bildstrom");
        store(false);
        crate::util::request_reconnect();
    }
}

/// Tablet wartet auf ein Schlüsselbild. Bei „nur bei Änderungen“ käme bei stillem Bild
/// keins → Encoder neu starten (beginnt mit einem Schlüsselbild); höchstens 1× je Sekunde.
fn on_key_request() {
    static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
    if !video::is_sparse() {
        return; // gleichmäßiger Bildstrom: Schlüsselbild kommt ohnehin spätestens nach 2 s
    }
    let Ok(mut last) = LAST.lock() else { return };
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last = Some(Instant::now());
    crate::log!("Tablet braucht ein Schlüsselbild – Encoder neu gestartet");
    crate::util::request_reconnect();
}

/// Übertragungsgröße: die in Windows gewählte Auflösung, wenn sie zu den angebotenen
/// (Seitenverhältnis des Tablets) gehört – das Tablet rechnet sie dann hoch –, sonst die
/// des Tablets (ffmpeg skaliert).
fn stream_size(device: &str, w: u32, h: u32, hz: u32) -> (u32, u32) {
    match display::mode_of(device) {
        Some((mw, mh, _))
            if crate::vdd::offered_modes(w, h, hz)
                .iter()
                .any(|&(ow, oh, _)| (ow, oh) == (mw, mh)) =>
        {
            (mw, mh)
        }
        _ => (w, h),
    }
}

/// Nach einem Auflösungswechsel in Windows: Übertragungsgröße (und automatische
/// Bitrate) anpassen; das Tablet bekommt die neue Größe mit der nächsten CONFIG.
fn resize(params: &mut StreamParams, cfg: &Config, device: &str, tablet: (u32, u32, u32)) {
    let (sw, sh) = stream_size(device, tablet.0, tablet.1, tablet.2);
    params.native = is_native(device, sw, sh);
    params.pixel_exact = params.native && (sw, sh) == (tablet.0, tablet.1);
    if (sw, sh) == (params.width, params.height) {
        return;
    }
    crate::log!(
        "Auflösung des Tablet-Monitors: {sw}x{sh} (vorher {}x{})",
        params.width,
        params.height
    );
    params.width = sw;
    params.height = sh;
    params.bitrate = video::bitrate_for(cfg, sw, sh, params.fps, params.codec);
}

/// Hat der Tablet-Monitor genau die Übertragungsgröße? (dann muss nichts skaliert werden)
fn is_native(device: &str, w: u32, h: u32) -> bool {
    matches!(display::mode_of(device), Some((mw, mh, _)) if (mw, mh) == (w, h))
}

/// Liest, was das Tablet auf der Video-Verbindung zurückschickt: Touch/Stift, PONG,
/// Lautstärketasten und die Bilder der Selfie-Kamera (Webcam).
struct InputReader {
    sock: TcpStream,
}

impl InputReader {
    fn start(sock: &TcpStream, vd: display::VirtualDisplay) -> Result<Self> {
        let mut rd = sock.try_clone().context("Video-Verbindung")?;
        let keep = sock.try_clone().context("Video-Verbindung")?;
        crate::camera::session_started();
        std::thread::spawn(move || {
            let mut injector: Option<crate::input::Injector> = None;
            // angemeldet: Kamerabilder (Schlüsselbilder) dürfen größer sein
            while let Ok((t, p)) = crate::proto::read_frame_max(&mut rd, 2 << 20) {
                match t {
                    crate::proto::T_INPUT => {
                        let c = crate::config::current();
                        if !c.tablet_input {
                            injector = None; // ausgeschaltet: offene Berührungen beenden
                            continue;
                        }
                        if let Some(mut ev) = crate::input::parse(&p) {
                            if let crate::input::Event::Pen(pen) = &mut ev {
                                pen.pressure =
                                    crate::input::pressure_curve(pen.pressure, &c.pen_pressure);
                            }
                            let inj = injector
                                .get_or_insert_with(|| crate::input::Injector::new(vd.clone()));
                            inj.set_button_mode(crate::input::ButtonMode::from_config(&c.pen_button));
                            inj.handle(ev);
                        }
                    }
                    crate::proto::T_PONG => video::on_pong(&p),
                    crate::proto::T_CAM_STATE => crate::camera::on_tablet_state(&p),
                    crate::proto::T_CAM_DATA => crate::camera::on_tablet_data(&p),
                    crate::proto::T_VOLUME_KEY => crate::audio::volume_key(&p),
                    crate::proto::T_DECODER => on_decoder_report(&p),
                    crate::proto::T_KEY_REQUEST => on_key_request(),
                    _ => {}
                }
            }
            crate::camera::session_ended();
        });
        Ok(InputReader { sock: keep })
    }
}

impl Drop for InputReader {
    fn drop(&mut self) {
        // Verbindung wirklich schließen: Unter Windows hält sonst die Kopie im Lese-Thread
        // sie offen, und das Tablet wartet ewig auf der alten Verbindung. Both schickt
        // ein FIN – das Tablet schließt, dadurch endet auch der Lese-Thread.
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

/// Schaltet das Tablet-Display aus, wenn der PC-Bildschirm ausgeht oder der PC
/// gesperrt wird, und wieder ein, wenn er zurückkommt. Bild und Ton laufen weiter.
/// Gerade verbundenes Tablet (adb-Pfad, Seriennummer) – fürs Herunterfahren.
static ACTIVE: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

struct ActiveDevice;

impl ActiveDevice {
    fn set(adb: &Adb, serial: &str) -> Self {
        if let Ok(mut g) = ACTIVE.lock() {
            *g = Some((adb.path.clone(), serial.to_string()));
        }
        ActiveDevice
    }
}

impl Drop for ActiveDevice {
    fn drop(&mut self) {
        if let Ok(mut g) = ACTIVE.lock() {
            *g = None;
        }
    }
}

/// Windows fährt herunter, startet neu oder meldet ab: Tablet nicht mit dem letzten Bild
/// stehen lassen. Mit „Tablet mit dem PC-Bildschirm aus- und einschalten“ geht das
/// Tablet-Display sofort aus (wie beim Sperren), sonst zeigt die App den Wartebildschirm.
/// Wartet höchstens 2,5 s – Windows gibt Programmen hier nur wenige Sekunden.
pub fn system_ending() {
    let active = ACTIVE.lock().ok().and_then(|g| g.clone());
    crate::util::request_end_session();
    let Some((path, serial)) = active else {
        return;
    };
    if !crate::config::current().follow_pc_power {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = Adb { path }.on(&serial, &["shell", "input", "keyevent", "KEYCODE_SLEEP"]);
        let _ = tx.send(r.is_ok());
    });
    match rx.recv_timeout(Duration::from_millis(2500)) {
        Ok(true) => crate::log!("Windows wird beendet – Tablet-Display aus"),
        _ => crate::log!("Windows wird beendet – Tablet-Display ließ sich nicht rechtzeitig ausschalten"),
    }
}

struct PowerWatcher {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PowerWatcher {
    fn start(adb: &Adb, serial: &str) -> Self {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let adb = Adb {
            path: adb.path.clone(),
        };
        let serial = serial.to_string();
        std::thread::spawn(move || {
            let mut dark = false;
            while !flag.load(Ordering::Relaxed) && !crate::util::shutdown() {
                let want = crate::util::pc_dark() && crate::config::current().follow_pc_power;
                if want != dark {
                    dark = want;
                    let key = if dark {
                        "KEYCODE_SLEEP"
                    } else {
                        "KEYCODE_WAKEUP"
                    };
                    match adb.on(&serial, &["shell", "input", "keyevent", key]) {
                        Ok(_) => crate::log!(
                            "Tablet-Display {}",
                            if dark {
                                "aus (PC-Bildschirm aus/gesperrt)"
                            } else {
                                "wieder an"
                            }
                        ),
                        Err(e) => crate::log!("Tablet-Display schalten: {e:#}"),
                    }
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            // Verbindung endet, während das Tablet dunkel ist: nicht dunkel lassen
            if dark && !crate::util::shutdown() && !crate::util::paused() {
                let _ = adb.on(&serial, &["shell", "input", "keyevent", "KEYCODE_WAKEUP"]);
            }
        });
        PowerWatcher { stop }
    }
}

impl Drop for PowerWatcher {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Merkt sich während der Verbindung, wo der Tablet-Monitor in Windows liegt, damit
/// er beim nächsten Anstecken (auch nach einem Neustart) wieder dort erscheint.
struct LayoutWatcher {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LayoutWatcher {
    fn start(vd: display::VirtualDisplay) -> Self {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            // erst warten, bis Windows und USBCD mit dem Anordnen fertig sind
            let mut last = None;
            let mut size: Option<(u32, u32)> = None;
            let mut waited = 0;
            while !flag.load(Ordering::Relaxed) && !crate::util::shutdown() {
                std::thread::sleep(Duration::from_millis(500));
                waited += 1;
                if waited < 10 || waited % 4 != 0 {
                    continue;
                }
                let Some(p) = display::current_placement(&vd) else {
                    last = None;
                    continue;
                };
                // Auflösung in Windows geändert → Übertragung auf die neue Größe umstellen
                if last == Some(p) {
                    let now = (p.width, p.height);
                    if size.is_some_and(|s| s != now) {
                        crate::util::request_reconnect();
                    }
                    size = Some(now);
                }
                // erst übernehmen, wenn zwei Abfragen hintereinander gleich sind
                if last == Some(p) && crate::config::current().display_position != Some(p) {
                    if flag.load(Ordering::Relaxed) {
                        break;
                    }
                    crate::config::update(|c| c.display_position = Some(p));
                    crate::log!(
                        "Position des Tablet-Monitors gemerkt: {},{} ({}x{})",
                        p.x,
                        p.y,
                        p.width,
                        p.height
                    );
                }
                last = Some(p);
            }
        });
        LayoutWatcher { stop }
    }
}

impl Drop for LayoutWatcher {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::CodecCaps;

    fn hello(hevc: f32, avc: f32) -> Hello {
        let mut h = Hello {
            width: 2560,
            height: 1600,
            refresh: 120.0,
            ..Default::default()
        };
        h.codecs.insert(
            "hevc".into(),
            CodecCaps {
                supported: hevc > 0.0,
                max_fps: hevc,
                ..Default::default()
            },
        );
        h.codecs.insert(
            "avc".into(),
            CodecCaps {
                supported: avc > 0.0,
                max_fps: avc,
                ..Default::default()
            },
        );
        h
    }

    #[test]
    fn prefers_hevc_at_full_rate() {
        let c = Config::default();
        assert_eq!(
            choose(&c, &hello(120.0, 60.0), Encoder::Nvenc),
            (Codec::Hevc, 120)
        );
        assert_eq!(
            choose(&c, &hello(60.0, 120.0), Encoder::Nvenc),
            (Codec::Avc, 120)
        );
        assert_eq!(
            choose(&c, &hello(90.0, 60.0), Encoder::Nvenc),
            (Codec::Hevc, 90)
        );
        assert_eq!(
            choose(&c, &hello(120.0, 60.0), Encoder::Software),
            (Codec::Avc, 60)
        );
    }
}
