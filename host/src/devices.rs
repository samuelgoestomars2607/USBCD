//! Tablet-Erkennung: beobachtet die per USB angeschlossenen Android-Geräte, fragt bei
//! unbekannten erst nach, installiert oder aktualisiert die USBCD-App und startet die
//! Übertragung (session.rs) – immer nur für ein Tablet gleichzeitig.

use crate::adb::{self, Adb};
use crate::config::{self, Config};
use crate::i18n::tr;
use crate::{helper, log, notify, session, state, waiting, wallpaper};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Der virtuelle Monitor: an, solange ein Tablet ihn braucht.
struct Monitor {
    on: bool,
    /// Einstellung „Virtuellen Monitor abschalten, solange kein Tablet steckt“
    off_when_unplugged: bool,
}

impl Monitor {
    /// Ausschalten (wenn so eingestellt) und als „aus“ merken.
    fn off(&mut self) {
        if self.on && self.off_when_unplugged {
            match helper::vdd_off() {
                Ok(()) => {
                    log!("Virtueller Monitor ausgeschaltet");
                    wallpaper::restore_later();
                }
                Err(e) => log!("Monitor ausschalten: {e:#}"),
            }
        }
        self.on = false;
    }

    /// Ausschalten, auch wenn er als „aus“ gemerkt ist (nach Fehlern, Pause, Abbruch).
    fn force_off(&mut self) {
        self.on = true;
        self.off();
    }
}

/// Ergebnis für ein Gerät in einem Durchlauf.
#[derive(PartialEq, Eq)]
enum Visit {
    /// übersprungen (wartet, nicht bereit, gesperrt …) – nächstes Gerät ansehen
    Skipped,
    /// Übertragung lief – in diesem Durchlauf kein weiteres Tablet
    Connected,
}

/// Zustand je angestecktem Tablet.
struct Dev {
    /// App beim nächsten Versuch starten (direkt nach dem Anstecken oder per Menü)
    launch: bool,
    /// Nutzer hat die App geschlossen → nicht ungefragt wieder öffnen
    app_closed: bool,
    had_session: bool,
    retry_at: Instant,
    state: String,
    /// App-Installation ist fehlgeschlagen – bis zum Neuanstecken nicht erneut versuchen
    install_failed: bool,
    /// per Menü „Tablet ausschalten“ schlafen gelegt
    asleep: bool,
    /// unbekanntes Gerät: Frage „Als Bildschirm verwenden?“ schon gezeigt
    asked: bool,
}

/// Anzeigename eines Geräts, ohne dass die App installiert sein muss.
fn device_name(adb: &Adb, serial: &str) -> String {
    let clean = |s: String| -> String {
        let s: String = s.chars().filter(|c| !c.is_control()).take(48).collect();
        s.trim().to_string()
    };
    for args in [
        &["shell", "settings", "get", "global", "device_name"][..],
        &["shell", "getprop", "ro.product.model"][..],
    ] {
        if let Ok(o) = adb.on(serial, args) {
            let n = clean(o);
            if !n.is_empty() && n != "null" {
                return n;
            }
        }
    }
    serial.to_string()
}

/// „Neues Gerät erkannt“ – fragt, ob es als Bildschirm verwendet werden soll.
fn ask_new_device(serial: &str, name: &str) {
    state::add_pending(serial, name);
    state::set_status(tr(
        "Neues Gerät erkannt – im Menü unter „Geräte“ bestätigen",
        "New device detected – confirm under “Devices” in the menu",
    ));
    log!("Neues Gerät {serial} ({name}) – warte auf Zustimmung");
    notify::show(notify::Toast {
        title: tr("Neues Gerät erkannt", "New device detected").to_string(),
        text: format!(
            "{name}\n{}",
            tr(
                "Als zweiten Bildschirm verwenden? Dafür wird die USBCD-App darauf installiert.",
                "Use it as a second screen? This installs the USBCD app on it."
            )
        ),
        actions: vec![
            (
                tr("Als Bildschirm verwenden", "Use as screen").to_string(),
                format!("device?act=use&serial={serial}"),
            ),
            (
                tr("Nie für dieses Gerät", "Never for this device").to_string(),
                format!("device?act=never&serial={serial}"),
            ),
        ],
        silent: false,
        long: true,
    });
}

/// Vom Setup mitgelieferte App: (Pfad zur APK, versionCode).
fn bundled_app() -> Option<(std::path::PathBuf, u32)> {
    let dir = config::exe_dir();
    let apk = dir.join("USBCD.apk");
    let ver = std::fs::read_to_string(dir.join("USBCD.apk.version")).ok()?;
    let ver = ver.trim().parse().ok()?;
    apk.exists().then_some((apk, ver))
}

/// versionCode der installierten App, None = nicht installiert.
fn installed_app_version(adb: &Adb, serial: &str, pkg: &str) -> Option<u32> {
    let out = adb.on(serial, &["shell", "dumpsys", "package", pkg]).ok()?;
    let i = out.find("versionCode=")? + "versionCode=".len();
    let digits: String = out[i..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl Dev {
    /// frisch angesteckt → App einmal automatisch starten
    fn new() -> Self {
        Dev {
            launch: true,
            app_closed: false,
            had_session: false,
            retry_at: Instant::now(),
            state: String::new(),
            install_failed: false,
            asleep: false,
            asked: false,
        }
    }
}

/// Läuft im Hintergrund bis zum Beenden: Geräte beobachten und verbinden.
pub fn run(cfg: Config) {
    let adb = Adb {
        path: cfg.adb_exe.clone(),
    };
    let _ = adb.run(&["start-server"]);
    let mut devs: HashMap<String, Dev> = HashMap::new();
    let mut adb_error_logged = false;
    let mut monitor = Monitor {
        on: false,
        off_when_unplugged: cfg.disable_display_when_unplugged,
    };

    // Geräteliste: adb meldet Änderungen selbst (track-devices); `adb devices` nur,
    // falls die Beobachtung gerade nicht läuft. Die Schleife läuft trotzdem jede Sekunde
    // (Wiederholungen, Pause, Zustimmung).
    let tracker = adb::Tracker::start(&adb);
    let mut seen = 0u64;
    while !state::shutdown() {
        let tracked = tracker.wait(&mut seen, Duration::from_secs(1));
        if state::shutdown() {
            break;
        }
        if state::windows_ending() {
            // Herunterfahren: nichts Neues mehr starten, keine Meldungen
            continue;
        }
        let list = match tracked.map(Ok).unwrap_or_else(|| adb.devices()) {
            Ok(d) => {
                adb_error_logged = false;
                d
            }
            Err(e) => {
                if !adb_error_logged {
                    log!("{e:#}");
                    adb_error_logged = true;
                }
                continue;
            }
        };
        forget_unplugged(&mut devs, &list, &mut monitor);

        let launch_req = state::take_launch_request();
        if launch_req && list.is_empty() {
            notify::once(
                "nolaunch",
                tr("Kein Tablet gefunden", "No tablet found"),
                tr(
                    "Bitte das Tablet per USB-C anschließen.",
                    "Please connect the tablet via USB-C.",
                ),
                Duration::from_secs(5),
            );
        }

        for d in &list {
            if visit(&cfg, &adb, &mut devs, &mut monitor, d, launch_req) == Visit::Connected {
                break; // ein Tablet gleichzeitig
            }
        }
    }
    monitor.off();
}

/// Abgezogene Tablets vergessen, Monitor aus, Meldungen wieder erlauben.
fn forget_unplugged(devs: &mut HashMap<String, Dev>, list: &[adb::Device], monitor: &mut Monitor) {
    // abgezogene Tablets
    let gone: Vec<String> = devs
        .keys()
        .filter(|s| !list.iter().any(|d| &d.serial == *s))
        .cloned()
        .collect();
    for s in gone {
        if let Some(d) = devs.remove(&s) {
            if d.had_session {
                log!("Tablet {s} abgezogen");
                notify::once(
                    "disconnected",
                    tr("Tablet getrennt", "Tablet disconnected"),
                    tr(
                        "Der Tablet-Monitor wurde entfernt.",
                        "The tablet monitor has been removed.",
                    ),
                    Duration::from_secs(30),
                );
            }
        }
        monitor.off();
        state::remove_pending(&s);
        // „Tablet ausschalten“ gilt nur bis zum Abziehen
        state::set_paused(false);
        for k in [
            "connected",
            "unauthorized",
            "noapp",
            "appclosed",
            "failed",
            "installfail",
            "notextended",
            "unsupported",
        ] {
            notify::reset(k);
        }
        state::set_status(waiting());
    }
}

/// Ein Gerät ansehen: zulassen, App bereitstellen, Übertragung starten.
fn visit(
    cfg: &Config,
    adb: &Adb,
    devs: &mut HashMap<String, Dev>,
    monitor: &mut Monitor,
    d: &adb::Device,
    launch_req: bool,
) -> Visit {
    // Emulatoren und WLAN-Debugging gehören nicht zu USBCD
    if d.serial.starts_with("emulator-") || !notify::valid_serial(&d.serial) {
        return Visit::Skipped;
    }
    let st = devs.entry(d.serial.clone()).or_insert_with(Dev::new);
    if launch_req {
        st.launch = true;
        st.app_closed = false;
        st.retry_at = Instant::now();
    }

    let known = config::current().devices.get(&d.serial).cloned();
    // „nie verwenden“: USBCD lässt das Gerät vollständig in Ruhe
    if known.as_ref().is_some_and(|k| !k.use_as_display) {
        if st.state != "ignored" {
            st.state = "ignored".into();
            log!(
                "{}: auf „nie verwenden“ gestellt – wird ignoriert",
                d.serial
            );
            monitor.off();
            state::set_status(waiting());
        }
        return Visit::Skipped;
    }

    if d.state != "device" {
        report_state(d, st, known.is_some());
        return Visit::Skipped;
    }
    st.state = d.state.clone();

    // Unbekanntes Gerät: nichts installieren oder starten, erst fragen
    if known.is_none() && !admit_unknown(adb, cfg, &d.serial, st, launch_req) {
        return Visit::Skipped;
    }
    state::remove_pending(&d.serial);

    // Menü „Tablet ausschalten“: Monitor weg, Display aus, warten auf „wieder ein“
    if state::paused() {
        if !st.asleep {
            switch_off(adb, &d.serial, st, monitor);
        }
        return Visit::Skipped;
    }
    if st.asleep {
        st.asleep = false;
        let _ = adb.on(&d.serial, &["shell", "input", "keyevent", "KEYCODE_WAKEUP"]);
        log!("Tablet wieder eingeschaltet");
        st.launch = true; // App nach vorn holen und verbinden
        st.retry_at = Instant::now();
    }
    if Instant::now() < st.retry_at {
        return Visit::Skipped;
    }

    // App installieren bzw. aktualisieren, wenn das Setup eine neuere mitbringt
    if ensure_app(adb, cfg, &d.serial, st).is_none() {
        app_missing(&d.serial, st);
        return Visit::Skipped;
    }

    let launch = st.launch;
    if launch {
        log!("Tablet {} angeschlossen – starte App", d.serial);
        state::set_status(tr("Tablet wird verbunden …", "Connecting tablet …"));
    }
    let outcome = session::run(&config::current(), adb, &d.serial, launch);
    st.launch = false;
    after_session(outcome, &d.serial, st, monitor, launch);
    Visit::Connected
}

/// Zustand „nicht bereit“ (z. B. USB-Debugging noch nicht erlaubt) einmal melden.
fn report_state(d: &adb::Device, st: &mut Dev, known: bool) {
    if st.state != d.state {
        st.state = d.state.clone();
        if d.state == "unauthorized" {
            log!("{}: USB-Debugging noch nicht erlaubt", d.serial);
            state::set_status(tr(
                "Auf dem Tablet USB-Debugging erlauben",
                "Allow USB debugging on the tablet",
            ));
            // bekanntes Tablet: deutlich hinweisen; fremdes Gerät: nur leise
            notify::once(
                "unauthorized",
                if known {
                    tr(
                        "Tablet erkannt – bitte erlauben",
                        "Tablet detected – please allow",
                    )
                } else {
                    tr("Android-Gerät erkannt", "Android device detected")
                },
                if known {
                    tr("Auf dem Tablet „USB-Debugging zulassen?“ mit „Immer von diesem Computer zulassen“ bestätigen.", "On the tablet, confirm “Allow USB debugging?” with “Always allow from this computer”.")
                } else {
                    tr("Um es als Bildschirm zu nutzen: auf dem Gerät „USB-Debugging zulassen?“ bestätigen. Sonst einfach ignorieren.", "To use it as a screen: confirm “Allow USB debugging?” on the device. Otherwise just ignore this.")
                },
                Duration::from_secs(600),
            );
        } else {
            log!("{}: Zustand {}", d.serial, d.state);
        }
    }
}

/// Unbekanntes Gerät: zulassen, wenn die USBCD-App schon darauf ist, sonst nachfragen.
/// true = weiter wie mit einem bekannten Gerät.
fn admit_unknown(adb: &Adb, cfg: &Config, serial: &str, st: &mut Dev, launch_req: bool) -> bool {
    let name = device_name(adb, serial);
    if installed_app_version(adb, serial, &cfg.android_package).is_some() {
        // wurde schon mit USBCD benutzt → gilt als zugelassen
        config::update(|c| {
            c.devices.insert(
                serial.to_string(),
                config::KnownDevice {
                    name: name.clone(),
                    use_as_display: true,
                    decoder_prompt: None,
                    stream_fallback: None,
                },
            );
        });
        log!(
            "{}: USBCD-App vorhanden – „{name}“ als Bildschirm zugelassen",
            serial
        );
    } else {
        if !st.asked || launch_req {
            st.asked = true;
            ask_new_device(serial, &name);
        }
        return false;
    }
    true
}

/// „Tablet ausschalten“ im Menü: Monitor aus, Display des Tablets aus.
fn switch_off(adb: &Adb, serial: &str, st: &mut Dev, monitor: &mut Monitor) {
    st.asleep = true;
    monitor.force_off();
    let _ = adb.on(serial, &["shell", "input", "keyevent", "KEYCODE_SLEEP"]);
    log!("Tablet ausgeschaltet (Menü)");
    state::set_status(tr(
        "Tablet ausgeschaltet – Menü → „Tablet wieder einschalten“",
        "Tablet switched off – menu → “Switch tablet back on”",
    ));
}

/// Installiert oder aktualisiert die App, wenn das Setup eine neuere mitbringt.
/// Liefert die installierte Version (None = keine App auf dem Tablet).
fn ensure_app(adb: &Adb, cfg: &Config, serial: &str, st: &mut Dev) -> Option<u32> {
    let mut installed = installed_app_version(adb, serial, &cfg.android_package);
    if let Some((apk, bundled)) = bundled_app() {
        if !matches!(installed, Some(v) if v >= bundled) && !st.install_failed {
            let what = if installed.is_some() {
                "aktualisiert"
            } else {
                "installiert"
            };
            let title = if installed.is_some() {
                tr("USBCD-App wird aktualisiert", "Updating the USBCD app")
            } else {
                tr("USBCD-App wird installiert", "Installing the USBCD app")
            };
            log!(
                "Tablet {}: USBCD-App wird {what} (Version {bundled})",
                serial
            );
            state::set_status(tr(
                "USBCD-App wird auf dem Tablet installiert …",
                "Installing the USBCD app on the tablet …",
            ));
            notify::once(
                "installing",
                title,
                tr(
                    "Einen Moment – die App wird auf das Tablet übertragen.",
                    "One moment – the app is being transferred to the tablet.",
                ),
                Duration::from_secs(60),
            );
            let apk_s = apk.to_string_lossy().to_string();
            let mut res = adb.on(serial, &["install", "-r", &apk_s]);
            if let Err(e) = &res {
                let msg = format!("{e:#}");
                if installed.is_some()
                    && (msg.contains("UPDATE_INCOMPATIBLE")
                        || msg.contains("INCONSISTENT_CERTIFICATES"))
                {
                    // anders signierte Fassung (z. B. aus Android Studio): ersetzen
                    log!("Vorhandene App anders signiert – wird ersetzt");
                    let _ = adb.on(serial, &["uninstall", &cfg.android_package]);
                    res = adb.on(serial, &["install", &apk_s]);
                }
            }
            match res {
                Ok(_) => {
                    log!("USBCD-App {what}");
                    installed = Some(bundled);
                }
                Err(e) => {
                    st.install_failed = true;
                    let msg = format!("{e:#}");
                    log!("App-Installation fehlgeschlagen: {msg}");
                    let hint = if msg.contains("UPDATE_INCOMPATIBLE") || msg.contains("SIGNATURES")
                    {
                        tr("Die vorhandene USBCD-App passt nicht zu dieser Version. Bitte sie auf dem Tablet deinstallieren und das Tablet neu anstecken.", "The installed USBCD app does not match this version. Please uninstall it on the tablet and reconnect the tablet.")
                    } else if msg.contains("USER_RESTRICTED")
                        || msg.contains("INSTALL_FAILED_ABORTED")
                    {
                        tr("Das Tablet erlaubt keine Installation über USB. Bitte auf dem Tablet bestätigen bzw. „Über USB installieren“ / Samsung „Auto Blocker“ prüfen.", "The tablet does not allow installing over USB. Please confirm on the tablet or check “Install via USB” / Samsung “Auto Blocker”.")
                    } else {
                        tr("Die App konnte nicht installiert werden. Details: Menü → „Einstellungen und Log öffnen“.", "The app could not be installed. Details: menu → “Open settings and log”.")
                    };
                    notify::once(
                        "installfail",
                        tr("App-Installation fehlgeschlagen", "App installation failed"),
                        hint,
                        Duration::from_secs(600),
                    );
                }
            }
        }
    }
    installed
}

/// Keine App auf dem Tablet: melden und später erneut versuchen.
fn app_missing(serial: &str, st: &mut Dev) {
    log!("{}: USBCD-App nicht installiert", serial);
    state::set_status(tr(
        "Tablet angeschlossen – USBCD-App fehlt",
        "Tablet connected – USBCD app missing",
    ));
    if !st.install_failed {
        notify::once(
            "noapp",
            tr("USBCD-App fehlt auf dem Tablet", "USBCD app missing on the tablet"),
            tr("Bitte die USBCD-App auf dem Tablet installieren (USBCD-Setup erneut ausführen oder Android Studio → Run).", "Please install the USBCD app on the tablet (run the USBCD setup again or Android Studio → Run)."),
            Duration::from_secs(3600),
        );
    }
    st.retry_at = Instant::now() + Duration::from_secs(30);
}

/// Ergebnis einer Übertragung auswerten: Status, Meldung, nächster Versuch.
fn after_session(
    outcome: session::Outcome,
    serial: &str,
    st: &mut Dev,
    monitor: &mut Monitor,
    launch: bool,
) {
    match outcome {
        session::Outcome::AppNotRunning => {
            // App wurde geschlossen: nicht wieder aufdrängen, nur still nachsehen
            monitor.off();
            if !st.app_closed {
                st.app_closed = true;
                log!("Tablet {}: USBCD-App ist geschlossen", serial);
                state::set_status(tr(
                    "Tablet angeschlossen – Anzeige beendet",
                    "Tablet connected – display stopped",
                ));
                if st.had_session {
                    notify::once(
                        "appclosed",
                        tr("Tablet-Anzeige beendet", "Tablet display stopped"),
                        tr("Zum Fortsetzen: USBCD-Symbol → „Auf dem Tablet anzeigen“ oder die USBCD-App auf dem Tablet öffnen.", "To continue: USBCD icon → “Show on tablet”, or open the USBCD app on the tablet."),
                        Duration::from_secs(600),
                    );
                }
            }
            st.retry_at = Instant::now() + Duration::from_secs(3);
        }
        session::Outcome::Paused => {
            // erledigt der nächste Durchlauf (Monitor aus, Display aus)
            st.retry_at = Instant::now();
        }
        session::Outcome::Disconnected => {
            // abgezogen, App geschlossen oder kurzer Aussetzer – der nächste
            // Durchlauf (ohne App-Neustart) zeigt, was es war
            st.had_session = true;
            st.app_closed = false;
            monitor.on = true; // bleibt an, bis klar ist, ob das Tablet noch da ist
            state::set_status(waiting());
            st.retry_at = Instant::now() + Duration::from_secs(2);
        }
        session::Outcome::NotExtended => {
            monitor.force_off();
            st.launch = launch;
            state::set_status(tr(
                "Tablet ist noch kein eigener Bildschirm",
                "Tablet is not a separate screen yet",
            ));
            notify::once_with(
                "notextended",
                tr("Tablet ist noch kein eigener Bildschirm", "Tablet is not a separate screen yet"),
                tr(
                    "Windows spiegelt die Anzeige oder nutzt nur den PC-Bildschirm. „Bildschirme erweitern“ behebt das – wie Windows-Taste + P → Erweitern.",
                    "Windows is mirroring the display or only using the PC screen. “Extend displays” fixes this – like Windows key + P → Extend.",
                ),
                Duration::from_secs(300),
                vec![(
                    tr("Bildschirme erweitern", "Extend displays").to_string(),
                    "extend".to_string(),
                )],
            );
            st.retry_at = Instant::now() + Duration::from_secs(15);
        }
        session::Outcome::Unsupported => {
            monitor.force_off();
            st.launch = false; // App zeigt den Hinweis – nicht neu starten
            state::set_status(tr(
                "Tablet kann das Bild nicht anzeigen",
                "Tablet cannot display the picture",
            ));
            notify::once(
                "unsupported",
                tr("Tablet kann das Bild nicht anzeigen", "Tablet cannot display the picture"),
                tr(
                    "Kein Decoder dieses Tablets kann das Bild vom PC dekodieren, auch nicht in kleinerer Auflösung. Details: Menü → „Einstellungen und Log öffnen“.",
                    "None of this tablet's decoders can decode the picture from the PC, not even at a lower resolution. Details: menu → “Open settings and log”.",
                ),
                Duration::from_secs(300),
            );
            // erst nach erneutem Anstecken wieder versuchen
            st.retry_at = Instant::now() + Duration::from_secs(24 * 3600);
        }
        session::Outcome::Failed(e) => {
            monitor.force_off();
            st.launch = launch; // beim nächsten Versuch App ggf. erneut starten
            log!("Tablet {}: {e:#}", serial);
            state::set_status(tr("Fehler – Details im Log", "Error – see log for details"));
            notify::once(
                "failed",
                tr(
                    "USBCD: Übertragung fehlgeschlagen",
                    "USBCD: transmission failed",
                ),
                &format!(
                    "{e}\n{}",
                    tr(
                        "Neuer Versuch in 15 s. Details: Menü → „Einstellungen und Log öffnen“.",
                        "Retrying in 15 s. Details: menu → “Open settings and log”."
                    )
                ),
                Duration::from_secs(300),
            );
            st.retry_at = Instant::now() + Duration::from_secs(15);
        }
    }
}
