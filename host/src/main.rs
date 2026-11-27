// Release-Build ohne Konsolenfenster (läuft im Hintergrund, Log: usbcd.log)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod adb;
mod audio;
mod auth;
mod autostart;
mod camera;
mod config;
mod devices;
mod display;
mod driver;
mod helper;
mod i18n;
mod input;
mod ladder;
mod log;
mod nal;
mod notify;
mod process;
mod proto;
mod session;
mod state;
mod still;
mod tray;
mod update;
mod vdd;
mod video;
mod wallpaper;
mod win;

use config::Config;
use i18n::tr;
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
    log::set_log_file(program_data().join(name));
    log::init_log();
}

/// Befehle für Setup, Deinstallation und Hilfsdienst (laufen mit Administratorrechten).
/// true = ein solcher Befehl wurde ausgeführt, das Programm endet danach.
fn admin_command(args: &[String]) -> bool {
    let first = args.first().map(String::as_str);
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
            true
        }
        Some("--uninstall-service") => {
            admin_log("setup.log");
            helper::uninstall();
            true
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
            true
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
            true
        }
        _ => false,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);

    // --- Befehle für Setup und Hilfsdienst (Administratorrechte) ---
    if admin_command(&args) {
        return;
    }

    // Abstürze in Hintergrund-Threads ins Log schreiben statt unsichtbar zu verschwinden
    std::panic::set_hook(Box::new(|info| {
        crate::log::log_line(&format!("Programmfehler: {info}"));
    }));

    // --- normales Programm (ohne Administratorrechte) ---
    process::quiet_error_boxes();
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
    log::init_log();
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
        let _ = process::command("explorer.exe")
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
    state::set_status(tr("Wird vorbereitet …", "Getting ready …"));

    // Vorbereitung und Tablet-Erkennung im Hintergrund – das Symbol erscheint sofort
    let worker_cfg = cfg.clone();
    let worker = std::thread::spawn(move || {
        prepare(&worker_cfg);
        devices::run(worker_cfg);
    });

    tray::run(args.iter().any(|a| a == "--show")); // blockiert bis „Beenden“
    state::set_shutdown();
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
    let name = win::wide(r"Local\USBCD-Instanz");
    // Handle bleibt bis Programmende offen
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    !h.is_null() && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
}

/// Einmalige Vorbereitung beim Start (dauert je nach System einige Sekunden).
fn prepare(cfg: &Config) {
    log!("adb: {} | ffmpeg: {}", cfg.adb_exe, cfg.ffmpeg_exe);
    match video::check_ffmpeg(&cfg.ffmpeg_exe) {
        Ok(true) => {}
        Ok(false) => notify::once(
            "ffmpeg",
            tr("USBCD: Bildübertragung nicht möglich", "USBCD: cannot transmit the picture"),
            tr("Die mitgelieferte FFmpeg-Version ist zu alt. Bitte das USBCD-Setup neu bauen und installieren.", "The bundled FFmpeg version is too old. Please rebuild and reinstall the USBCD setup."),
            Duration::from_secs(3600),
        ),
        Err(e) => notify::once(
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
            notify::once(
                "helper",
                tr("USBCD: Hilfsdienst fehlt", "USBCD: helper service missing"),
                &format!("{e}"),
                Duration::from_secs(3600),
            );
        }
    }
    audio::prepare_endpoint(cfg);
    tray::set_autostart_known(autostart::enabled());
    state::set_status(waiting());
}

fn waiting() -> &'static str {
    tr(
        "Wartet auf Tablet (per USB-C anschließen)",
        "Waiting for tablet (connect via USB-C)",
    )
}
