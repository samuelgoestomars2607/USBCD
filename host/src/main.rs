// Release-Build ohne Konsolenfenster (läuft im Hintergrund, Log: usbcd.log)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod adb;
mod audio;
mod auth;
mod autostart;
mod camera;
mod config;
mod display;
mod driver;
mod helper;
mod i18n;
mod input;
mod ladder;
mod nal;
mod notify;
mod proto;
mod session;
mod still;
mod tray;
mod update;
mod util;
mod vdd;
mod video;
mod wallpaper;

use adb::Adb;
use config::Config;
use i18n::tr;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Log-Ordner für Dienst und Setup-Befehle (laufen mit Administratorrechten und
/// schreiben deshalb nie ins Nutzerprofil). Das Setup schränkt die Rechte darauf ein.
fn program_data() -> std::path::PathBuf {
    let base = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
    let d = std::path::PathBuf::from(base).join("USBCD");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn admin_log(name: &str) {
    util::set_log_file(program_data().join(name));
    util::init_log();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);

    // --- Befehle für Setup und Hilfsdienst (Administratorrechte) ---
    match first {
        Some("--service") => {
            admin_log("helper.log");
            std::process::exit(helper::run_service());
        }
        Some("--install-service") => {
            admin_log("setup.log");
            if let Err(e) = helper::install() {
                log!("Hilfsdienst: {e:#}");
                std::process::exit(1);
            }
            return;
        }
        Some("--uninstall-service") => {
            admin_log("setup.log");
            helper::uninstall();
            return;
        }
        Some("--restore-audio") => {
            // Deinstallation, VB-CABLE bleibt: Namen und Sichtbarkeit zurücksetzen
            admin_log("setup.log");
            unsafe {
                windows_sys::Win32::System::Com::CoInitializeEx(
                    std::ptr::null(),
                    windows_sys::Win32::System::Com::COINIT_MULTITHREADED as u32,
                )
            };
            if let Err(e) = audio::restore_system() {
                log!("Audio zurücksetzen: {e:#}");
            }
            return;
        }
        Some("--install-driver") if args.len() >= 3 => {
            admin_log("setup.log");
            match driver::install(&args[1], &args[2]) {
                Ok(reboot) => {
                    log!(
                        "Treiber {} installiert{}",
                        args[2],
                        if reboot { " (Neustart nötig)" } else { "" }
                    );
                    std::process::exit(if reboot { 3010 } else { 0 });
                }
                Err(e) => {
                    log!("Treiberinstallation: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        // Selbsttest des Build-Skripts: prüft Setup und Signatur wie beim Update (0 = gültig)
        Some("--verify-update") if args.len() >= 3 => {
            std::process::exit(update::verify_cli(&args[1], &args[2]));
        }
        Some("--remove-device") if args.len() >= 2 => {
            admin_log("setup.log");
            let n = driver::remove(&args[1]);
            log!("{n} Gerät(e) {} entfernt", args[1]);
            return;
        }
        _ => {}
    }

    // Abstürze in Hintergrund-Threads ins Log schreiben statt unsichtbar zu verschwinden
    std::panic::set_hook(Box::new(|info| {
        crate::util::log_line(&format!("Programmfehler: {info}"));
    }));

    // --- normales Programm (ohne Administratorrechte) ---
    util::quiet_error_boxes();
    // Echte Pixel-Koordinaten (wichtig für Touch/Stift und Monitorlage bei Skalierung)
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    // Klick in einer USBCD-Benachrichtigung: an die laufende Instanz weitergeben
    if first == Some("--uri") {
        if let Some(u) = args.get(1) {
            tray::send_uri(u);
        }
        return;
    }
    util::init_log();
    let cfg = Config::load();
    config::set_current(cfg.clone());

    if args.iter().any(|a| a == "--list-audio") {
        match audio::list_devices() {
            Ok(v) => v.iter().for_each(|n| println!("{n}")),
            Err(e) => eprintln!("{e:#}"),
        }
        return;
    }
    if first == Some("--autostart") {
        match args.get(1).map(String::as_str) {
            Some("off") => autostart::disable(),
            _ => {
                if let Err(e) = autostart::enable() {
                    log!("{e:#}");
                    std::process::exit(1);
                }
            }
        }
        return;
    }
    if first == Some("--open-folder") {
        // Startmenü „Einstellungen und Log“
        let _ = util::command("explorer.exe")
            .arg(config::data_dir())
            .spawn();
        return;
    }
    if args.iter().any(|a| a == "--init-config") {
        return; // Config::load() hat die Datei bereits angelegt
    }
    if args.iter().any(|a| a == "--find-display") {
        println!("{:?}", display::find_virtual_display(&cfg.display_match));
        return;
    }

    // nur eine Instanz je Anmeldung; ein zweiter Start (z. B. über das Startmenü)
    // bittet die laufende Instanz, ihre Info-Blase zu zeigen
    if !single_instance() {
        for _ in 0..20 {
            if tray::show_existing() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        return;
    }

    log!("USBCD {} gestartet", env!("CARGO_PKG_VERSION"));
    notify::init();
    camera::init();
    update::cleanup();
    update::start(args.iter().any(|a| a == "--update-test"));
    util::set_status(tr("Wird vorbereitet …", "Getting ready …"));

    // Vorbereitung und Tablet-Erkennung im Hintergrund – das Symbol erscheint sofort
    let worker_cfg = cfg.clone();
    let worker = std::thread::spawn(move || {
        prepare(&worker_cfg);
        device_loop(worker_cfg);
    });

    tray::run(args.iter().any(|a| a == "--show")); // blockiert bis „Beenden“
    util::set_shutdown();
    log!("USBCD wird beendet …");
    let t = Instant::now();
    while !worker.is_finished() && t.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(100));
    }
    log!("USBCD beendet");
}

fn single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = util::wide(r"Local\USBCD-Instanz");
    // Handle bleibt bis Programmende offen
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    !h.is_null() && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
}

/// Einmalige Vorbereitung beim Start (dauert je nach System einige Sekunden).
fn prepare(cfg: &Config) {
    log!("adb: {} | ffmpeg: {}", cfg.adb_exe, cfg.ffmpeg_exe);
    match video::check_ffmpeg(&cfg.ffmpeg_exe) {
        Ok(true) => {}
        Ok(false) if cfg.capture_method != "ddagrab" => util::notify(
            "ffmpeg",
            tr("USBCD: Bildübertragung nicht möglich", "USBCD: cannot transmit the picture"),
            tr("Die mitgelieferte FFmpeg-Version ist zu alt. Bitte das USBCD-Setup neu bauen und installieren.", "The bundled FFmpeg version is too old. Please rebuild and reinstall the USBCD setup."),
            Duration::from_secs(3600),
        ),
        Ok(false) => {}
        Err(e) => util::notify(
            "ffmpeg",
            tr("USBCD: Bildübertragung nicht möglich", "USBCD: cannot transmit the picture"),
            &format!("{} ({e})", tr("FFmpeg fehlt. Bitte das USBCD-Setup erneut ausführen.", "FFmpeg is missing. Please run the USBCD setup again.")),
            Duration::from_secs(3600),
        ),
    }
    match helper::call("ping") {
        Ok(v) => {
            log!("Hilfsdienst {v} erreichbar");
            if cfg.disable_display_when_unplugged {
                if let Err(e) = helper::vdd_off() {
                    log!("Monitor ausschalten: {e:#}");
                }
            }
            wallpaper::remember();
        }
        Err(e) => {
            log!("Hilfsdienst: {e:#}");
            util::notify(
                "helper",
                tr("USBCD: Hilfsdienst fehlt", "USBCD: helper service missing"),
                &format!("{e}"),
                Duration::from_secs(3600),
            );
        }
    }
    audio::prepare_endpoint(cfg);
    tray::set_autostart_known(autostart::enabled());
    util::set_status(waiting());
}

fn waiting() -> &'static str {
    tr(
        "Wartet auf Tablet (per USB-C anschließen)",
        "Waiting for tablet (connect via USB-C)",
    )
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
    util::add_pending(serial, name);
    util::set_status(tr(
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

fn device_loop(cfg: Config) {
    let adb = Adb {
        path: cfg.adb_exe.clone(),
    };
    let _ = adb.run(&["start-server"]);
    let mut devs: HashMap<String, Dev> = HashMap::new();
    let mut adb_error_logged = false;
    let mut display_on = false;
    let display_off = |on: &mut bool| {
        if *on && cfg.disable_display_when_unplugged {
            match helper::vdd_off() {
                Ok(()) => {
                    log!("Virtueller Monitor ausgeschaltet");
                    wallpaper::restore_later();
                }
                Err(e) => log!("Monitor ausschalten: {e:#}"),
            }
        }
        *on = false;
    };

    // Geräteliste: adb meldet Änderungen selbst (track-devices); `adb devices` nur,
    // falls die Beobachtung gerade nicht läuft. Die Schleife läuft trotzdem jede Sekunde
    // (Wiederholungen, Pause, Zustimmung).
    let tracker = adb::Tracker::start(&adb);
    let mut seen = 0u64;
    while !util::shutdown() {
        let tracked = tracker.wait(&mut seen, Duration::from_secs(1));
        if util::shutdown() {
            break;
        }
        if util::windows_ending() {
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
                    util::notify(
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
            display_off(&mut display_on);
            util::remove_pending(&s);
            // „Tablet ausschalten“ gilt nur bis zum Abziehen
            util::set_paused(false);
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
                util::reset_notify(k);
            }
            util::set_status(waiting());
        }

        let launch_req = util::take_launch_request();
        if launch_req && list.is_empty() {
            util::notify(
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
            // Emulatoren und WLAN-Debugging gehören nicht zu USBCD
            if d.serial.starts_with("emulator-") || !notify::valid_serial(&d.serial) {
                continue;
            }
            let st = devs.entry(d.serial.clone()).or_insert_with(|| Dev {
                launch: true, // frisch angesteckt → App einmal automatisch starten
                app_closed: false,
                had_session: false,
                retry_at: Instant::now(),
                state: String::new(),
                install_failed: false,
                asleep: false,
                asked: false,
            });
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
                    display_off(&mut display_on);
                    util::set_status(waiting());
                }
                continue;
            }

            if d.state != "device" {
                if st.state != d.state {
                    st.state = d.state.clone();
                    if d.state == "unauthorized" {
                        log!("{}: USB-Debugging noch nicht erlaubt", d.serial);
                        util::set_status(tr(
                            "Auf dem Tablet USB-Debugging erlauben",
                            "Allow USB debugging on the tablet",
                        ));
                        // bekanntes Tablet: deutlich hinweisen; fremdes Gerät: nur leise
                        util::notify(
                            "unauthorized",
                            if known.is_some() {
                                tr(
                                    "Tablet erkannt – bitte erlauben",
                                    "Tablet detected – please allow",
                                )
                            } else {
                                tr("Android-Gerät erkannt", "Android device detected")
                            },
                            if known.is_some() {
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
                continue;
            }
            st.state = d.state.clone();

            // Unbekanntes Gerät: nichts installieren oder starten, erst fragen
            if known.is_none() {
                let name = device_name(&adb, &d.serial);
                if installed_app_version(&adb, &d.serial, &cfg.android_package).is_some() {
                    // wurde schon mit USBCD benutzt → gilt als zugelassen
                    config::update(|c| {
                        c.devices.insert(
                            d.serial.clone(),
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
                        d.serial
                    );
                } else {
                    if !st.asked || launch_req {
                        st.asked = true;
                        ask_new_device(&d.serial, &name);
                    }
                    continue;
                }
            }
            util::remove_pending(&d.serial);

            // Menü „Tablet ausschalten“: Monitor weg, Display aus, warten auf „wieder ein“
            if util::paused() {
                if !st.asleep {
                    st.asleep = true;
                    display_on = true;
                    display_off(&mut display_on);
                    let _ = adb.on(&d.serial, &["shell", "input", "keyevent", "KEYCODE_SLEEP"]);
                    log!("Tablet ausgeschaltet (Menü)");
                    util::set_status(tr(
                        "Tablet ausgeschaltet – Menü → „Tablet wieder einschalten“",
                        "Tablet switched off – menu → “Switch tablet back on”",
                    ));
                }
                continue;
            }
            if st.asleep {
                st.asleep = false;
                let _ = adb.on(&d.serial, &["shell", "input", "keyevent", "KEYCODE_WAKEUP"]);
                log!("Tablet wieder eingeschaltet");
                st.launch = true; // App nach vorn holen und verbinden
                st.retry_at = Instant::now();
            }
            if Instant::now() < st.retry_at {
                continue;
            }

            // App installieren bzw. aktualisieren, wenn das Setup eine neuere mitbringt
            let mut installed = installed_app_version(&adb, &d.serial, &cfg.android_package);
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
                        d.serial
                    );
                    util::set_status(tr(
                        "USBCD-App wird auf dem Tablet installiert …",
                        "Installing the USBCD app on the tablet …",
                    ));
                    util::notify(
                        "installing",
                        title,
                        tr(
                            "Einen Moment – die App wird auf das Tablet übertragen.",
                            "One moment – the app is being transferred to the tablet.",
                        ),
                        Duration::from_secs(60),
                    );
                    let apk_s = apk.to_string_lossy().to_string();
                    let mut res = adb.on(&d.serial, &["install", "-r", &apk_s]);
                    if let Err(e) = &res {
                        let msg = format!("{e:#}");
                        if installed.is_some()
                            && (msg.contains("UPDATE_INCOMPATIBLE")
                                || msg.contains("INCONSISTENT_CERTIFICATES"))
                        {
                            // anders signierte Fassung (z. B. aus Android Studio): ersetzen
                            log!("Vorhandene App anders signiert – wird ersetzt");
                            let _ = adb.on(&d.serial, &["uninstall", &cfg.android_package]);
                            res = adb.on(&d.serial, &["install", &apk_s]);
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
                            let hint = if msg.contains("UPDATE_INCOMPATIBLE")
                                || msg.contains("SIGNATURES")
                            {
                                tr("Die vorhandene USBCD-App passt nicht zu dieser Version. Bitte sie auf dem Tablet deinstallieren und das Tablet neu anstecken.", "The installed USBCD app does not match this version. Please uninstall it on the tablet and reconnect the tablet.")
                            } else if msg.contains("USER_RESTRICTED")
                                || msg.contains("INSTALL_FAILED_ABORTED")
                            {
                                tr("Das Tablet erlaubt keine Installation über USB. Bitte auf dem Tablet bestätigen bzw. „Über USB installieren“ / Samsung „Auto Blocker“ prüfen.", "The tablet does not allow installing over USB. Please confirm on the tablet or check “Install via USB” / Samsung “Auto Blocker”.")
                            } else {
                                tr("Die App konnte nicht installiert werden. Details: Menü → „Einstellungen und Log öffnen“.", "The app could not be installed. Details: menu → “Open settings and log”.")
                            };
                            util::notify(
                                "installfail",
                                tr("App-Installation fehlgeschlagen", "App installation failed"),
                                hint,
                                Duration::from_secs(600),
                            );
                        }
                    }
                }
            }
            if installed.is_none() {
                log!("{}: USBCD-App nicht installiert", d.serial);
                util::set_status(tr(
                    "Tablet angeschlossen – USBCD-App fehlt",
                    "Tablet connected – USBCD app missing",
                ));
                if !st.install_failed {
                    util::notify(
                        "noapp",
                        tr("USBCD-App fehlt auf dem Tablet", "USBCD app missing on the tablet"),
                        tr("Bitte die USBCD-App auf dem Tablet installieren (USBCD-Setup erneut ausführen oder Android Studio → Run).", "Please install the USBCD app on the tablet (run the USBCD setup again or Android Studio → Run)."),
                        Duration::from_secs(3600),
                    );
                }
                st.retry_at = Instant::now() + Duration::from_secs(30);
                continue;
            }

            let launch = st.launch;
            if launch {
                log!("Tablet {} angeschlossen – starte App", d.serial);
                util::set_status(tr("Tablet wird verbunden …", "Connecting tablet …"));
            }
            let outcome = session::run(&config::current(), &adb, &d.serial, launch);
            st.launch = false;
            match outcome {
                session::Outcome::AppNotRunning => {
                    // App wurde geschlossen: nicht wieder aufdrängen, nur still nachsehen
                    display_off(&mut display_on);
                    if !st.app_closed {
                        st.app_closed = true;
                        log!("Tablet {}: USBCD-App ist geschlossen", d.serial);
                        util::set_status(tr(
                            "Tablet angeschlossen – Anzeige beendet",
                            "Tablet connected – display stopped",
                        ));
                        if st.had_session {
                            util::notify(
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
                    display_on = true; // bleibt an, bis klar ist, ob das Tablet noch da ist
                    util::set_status(waiting());
                    st.retry_at = Instant::now() + Duration::from_secs(2);
                }
                session::Outcome::NotExtended => {
                    display_on = true;
                    display_off(&mut display_on);
                    st.launch = launch;
                    util::set_status(tr(
                        "Tablet ist noch kein eigener Bildschirm",
                        "Tablet is not a separate screen yet",
                    ));
                    util::notify_with(
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
                    display_on = true;
                    display_off(&mut display_on);
                    st.launch = false; // App zeigt den Hinweis – nicht neu starten
                    util::set_status(tr(
                        "Tablet kann das Bild nicht anzeigen",
                        "Tablet cannot display the picture",
                    ));
                    util::notify(
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
                    display_on = true;
                    display_off(&mut display_on);
                    st.launch = launch; // beim nächsten Versuch App ggf. erneut starten
                    log!("Tablet {}: {e:#}", d.serial);
                    util::set_status(tr("Fehler – Details im Log", "Error – see log for details"));
                    util::notify(
                        "failed",
                        tr("USBCD: Übertragung fehlgeschlagen", "USBCD: transmission failed"),
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
            break; // ein Tablet gleichzeitig
        }
    }
    display_off(&mut display_on);
}
