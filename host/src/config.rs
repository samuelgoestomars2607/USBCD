use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Pfad zu adb.exe ("adb" = aus PATH bzw. Android-SDK automatisch suchen)
    pub adb_path: String,
    /// Pfad zu ffmpeg.exe (FFmpeg 8.0 oder neuer, wegen gfxcapture)
    pub ffmpeg_path: String,
    /// tatsächlich verwendete Programme (ermittelt, nicht gespeichert)
    #[serde(skip)]
    pub adb_exe: String,
    #[serde(skip)]
    pub ffmpeg_exe: String,
    /// Paketname der Tablet-App
    pub android_package: String,
    pub video_port: u16,
    pub audio_port: u16,

    /// "auto" | "nvenc" | "amf" | "qsv" | "software"
    pub encoder: String,
    /// "auto" | "hevc" | "avc"
    pub codec: String,
    /// 0 = automatisch aus Auflösung × Bildrate (Menü „Bildqualität“)
    pub bitrate_mbps: u32,
    /// Bildqualität „Maximal“: gleichbleibende Qualität statt gleichbleibender Datenrate
    /// (NVIDIA), sonst die höchste Datenrate – für starke PCs; Vorrang vor bitrate_mbps
    pub quality_max: bool,
    /// Vorrang für die Bildübertragung: ffmpeg bekommt beim Grafikchip die Stufe „hoch“ und
    /// bei der CPU „höher als normal“ (wie bei Game-Streaming-Servern)
    pub stream_priority: bool,
    /// Standbilder verlustfrei: steht das Bild still, schickt USBCD ein pixelgenaues Bild
    /// hinterher (exakte Farben, scharfe farbige Schrift) – für starke PCs
    pub lossless_stills: bool,
    /// Helligkeit des Tablets in Prozent (Menü „Tablet-Helligkeit“)
    pub tablet_brightness: u32,
    /// 0 = keine Begrenzung (Tablet-Maximum)
    pub max_fps: u32,
    pub capture_cursor: bool,
    /// "gfxcapture" (Standard, FFmpeg ≥ 8) oder "ddagrab"
    pub capture_method: String,
    /// nur für ddagrab: DXGI-Ausgangsindex des virtuellen Monitors
    pub ddagrab_output_idx: u32,

    /// Textstücke, an denen der virtuelle Monitor erkannt wird
    pub display_match: Vec<String>,
    /// Virtuellen Monitor abschalten, solange kein Tablet steckt
    pub disable_display_when_unplugged: bool,

    pub audio_enabled: bool,
    /// Textstücke im Namen des virtuellen Lautsprechers
    pub audio_device_match: Vec<String>,
    /// Beim Anstecken automatisch als Standard-Wiedergabegerät setzen (Standard: nein –
    /// den Tablet-Lautsprecher wählt man selbst in den Windows-Soundeinstellungen)
    pub auto_switch_audio: bool,
    /// Lautsprecher in Windows so benennen (leer = nicht umbenennen)
    pub audio_device_rename: String,
    /// "auto" | "manual" | "off" – wie die Windows-Lautstärke angewendet wird
    pub volume_mode: String,
    /// Aufnahmeseite von VB-CABLE („CABLE Output“) ausblenden, damit Programme den
    /// Tablet-Ton nicht als Mikrofon aufnehmen können
    pub hide_cable_output: bool,
    /// Zuletzt genutzte Position des Tablet-Monitors (wird automatisch gemerkt,
    /// wenn du ihn in den Windows-Anzeigeeinstellungen verschiebst)
    pub display_position: Option<crate::display::Placement>,
    /// Touch und S Pen am Tablet steuern Windows
    pub tablet_input: bool,
    /// Druckkurve des S Pen: "soft" (weich – wenig Kraft nötig) | "linear" | "hard" (hart)
    pub pen_pressure: String,
    /// Info-Anzeige (Bilder/s, Datenrate, Laufzeiten) auf dem Tablet
    pub show_stats: bool,
    /// Tablet-Display aus, wenn der PC-Bildschirm ausgeht oder der PC gesperrt wird
    pub follow_pc_power: bool,
    /// Selfie-Kamera des Tablets als Webcam „USBCD Tablet Webcam“ anbieten (nie als
    /// Standardkamera; die Tablet-Kamera läuft nur, während eine App sie benutzt)
    pub webcam: bool,
    /// „Nicht stören“ auf dem Tablet, solange es als Monitor läuft (braucht einmal die
    /// Erlaubnis auf dem Tablet; danach wird der vorherige Zustand wiederhergestellt)
    pub tablet_dnd: bool,
    /// Lautstärketasten des Tablets regeln den Tablet-Lautsprecher in Windows
    pub tablet_volume_keys: bool,
    /// Bilder nur senden, wenn sich auf dem Tablet-Monitor etwas ändert (spart Grafikkarte,
    /// USB und Tablet-Akku). Nur bei Tablets, deren Dekoder nachweislich jedes Bild sofort
    /// ausgibt – sonst wie bisher ein gleichmäßiger Bildstrom.
    pub sparse_frames: bool,
    /// Täglich bei GitHub nach einer neuen USBCD-Version sehen (Menü „Automatisch nach
    /// Updates suchen“). Installiert wird nur nach Klick und nur mit gültiger Signatur.
    pub update_check: bool,
    /// Diese Version nicht mehr per Benachrichtigung anbieten („Diese Version überspringen“);
    /// im Menü bleibt das Update trotzdem wählbar.
    pub update_skip: String,
    /// Bekannte Geräte (Seriennummer → Entscheidung). Unbekannte Geräte werden nie
    /// ungefragt verändert – USBCD fragt erst nach.
    pub devices: std::collections::BTreeMap<String, KnownDevice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownDevice {
    /// Anzeigename, z. B. „Tab S6 Lite von Samuel“
    pub name: String,
    /// true = als Bildschirm verwenden, false = nie verwenden
    #[serde(rename = "use")]
    pub use_as_display: bool,
    /// Prüfergebnis der App beim ersten Verbinden: gibt der Dekoder jedes Bild sofort aus?
    /// (None = noch nicht geprüft; Zeile löschen = beim nächsten Verbinden neu prüfen)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoder_prompt: Option<bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            adb_path: "adb".into(),
            ffmpeg_path: "ffmpeg".into(),
            adb_exe: String::new(),
            ffmpeg_exe: String::new(),
            android_package: "de.samuel.usbcd".into(),
            video_port: 27183,
            audio_port: 27184,
            encoder: "auto".into(),
            codec: "auto".into(),
            bitrate_mbps: 0,
            quality_max: false,
            stream_priority: true,
            lossless_stills: false,
            tablet_brightness: 100,
            max_fps: 0,
            capture_cursor: true,
            capture_method: "gfxcapture".into(),
            ddagrab_output_idx: 1,
            display_match: vec![
                "Virtual Display Driver".into(),
                "MttVDD".into(),
                "MTT1337".into(),
            ],
            disable_display_when_unplugged: true,
            audio_enabled: true,
            audio_device_match: vec![
                "USBCD Tablet".into(),
                "Tablet-Lautsprecher".into(),
                "Tablet Speakers".into(),
                "VB-Audio Virtual Cable".into(),
                "CABLE Input".into(),
                "Virtual Audio Driver".into(),
            ],
            auto_switch_audio: false,
            audio_device_rename: crate::i18n::tr("USBCD Tablet Lautsprecher", "USBCD Tablet Speakers")
                .into(),
            volume_mode: "auto".into(),
            hide_cable_output: true,
            display_position: None,
            tablet_input: true,
            pen_pressure: "linear".into(),
            show_stats: false,
            follow_pc_power: true,
            webcam: false,
            tablet_dnd: false,
            tablet_volume_keys: true,
            sparse_frames: true,
            update_check: true,
            update_skip: String::new(),
            devices: Default::default(),
        }
    }
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Ordner für Konfiguration und Log: %LOCALAPPDATA%\USBCD
/// (die exe selbst liegt schreibgeschützt unter Programme).
pub fn data_dir() -> PathBuf {
    let d = std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("USBCD"))
        .unwrap_or_else(exe_dir);
    let _ = std::fs::create_dir_all(&d);
    d
}

impl Config {
    /// Lädt `usbcd.toml` aus dem Datenordner; legt sie mit Standardwerten an, falls sie fehlt.
    pub fn load() -> Config {
        let path = data_dir().join("usbcd.toml");
        let mut original: Option<String> = None;
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(c) => {
                    original = Some(s);
                    c
                }
                Err(e) => {
                    crate::log!("Konfiguration fehlerhaft ({e}) – nutze Standardwerte");
                    Config::default()
                }
            },
            Err(_) => {
                let c = Config::default();
                if let Ok(s) = toml::to_string_pretty(&c) {
                    let _ = std::fs::write(&path, s);
                }
                c
            }
        };
        cfg.migrate();
        // Datei auf den aktuellen Stand bringen (veraltete Einträge entfernen,
        // neue mit Standardwert ergänzen) – nur wenn sie fehlerfrei gelesen wurde
        if let (Some(old), Ok(new)) = (original, toml::to_string_pretty(&cfg)) {
            if old.replace("\r\n", "\n") != new {
                let _ = std::fs::write(&path, new);
            }
        }
        cfg.resolve_tools();
        cfg
    }

    /// Alte, zu breite Standardwerte ersetzen ("Virtual Display" passte auch auf Parsec).
    fn migrate(&mut self) {
        for m in self.display_match.iter_mut() {
            if m == "Virtual Display" {
                *m = "Virtual Display Driver".into();
            }
        }
        self.display_match.retain(|m| m != "IddSample");
        // Lautsprecher heißt jetzt „USBCD Tablet Lautsprecher“ / „USBCD Tablet Speakers“
        // (alte Standardnamen ersetzen, eigene Namen bleiben)
        if matches!(
            self.audio_device_rename.as_str(),
            "Tablet-Lautsprecher" | "Tablet Speakers"
        ) {
            self.audio_device_rename = Config::default().audio_device_rename;
        }
        if !self.audio_device_match.iter().any(|m| m == "USBCD Tablet") {
            self.audio_device_match.insert(0, "USBCD Tablet".into());
        }
        // Der Paketname landet in adb-Befehlen: nur gültige Android-Paketnamen zulassen
        if !valid_package(&self.android_package) {
            crate::log!(
                "android_package {:?} ungültig – nutze Standard",
                self.android_package
            );
            self.android_package = Config::default().android_package;
        }
    }

    /// Sucht adb/ffmpeg an üblichen Orten, wenn nur der Programmname angegeben ist.
    fn resolve_tools(&mut self) {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let here = exe_dir();
        self.adb_exe = self.adb_path.clone();
        self.ffmpeg_exe = self.ffmpeg_path.clone();
        if self.adb_path == "adb" {
            // adb aus Android Studio bevorzugen: zwei verschiedene adb-Versionen
            // würden sich gegenseitig den Server beenden.
            let candidates = [
                PathBuf::from(&local).join(r"Android\Sdk\platform-tools\adb.exe"),
                here.join("platform-tools").join("adb.exe"),
                here.join("adb.exe"),
            ];
            if let Some(p) = candidates.iter().find(|p| p.exists()) {
                self.adb_exe = p.to_string_lossy().into_owned();
            }
        }
        if self.ffmpeg_path == "ffmpeg" {
            let candidates = [
                here.join("ffmpeg.exe"),
                here.join("ffmpeg").join("bin").join("ffmpeg.exe"),
            ];
            if let Some(p) = candidates.iter().find(|p| p.exists()) {
                self.ffmpeg_exe = p.to_string_lossy().into_owned();
            }
        }
    }
}

static CURRENT: std::sync::Mutex<Option<Config>> = std::sync::Mutex::new(None);

/// Aktuell gültige Einstellungen (nach Änderungen über das Menü).
pub fn current() -> Config {
    let mut g = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(Config::load());
    }
    g.as_ref().cloned().unwrap_or_default()
}

pub fn set_current(c: Config) {
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
}

/// Einstellung ändern und dauerhaft in usbcd.toml speichern.
pub fn update(f: impl FnOnce(&mut Config)) {
    let mut c = current();
    f(&mut c);
    if let Ok(s) = toml::to_string_pretty(&c) {
        let _ = std::fs::write(data_dir().join("usbcd.toml"), s);
    }
    set_current(c);
}

/// Android-Paketname: Teile aus Buchstaben/Ziffern/_ , durch Punkte getrennt.
pub fn valid_package(p: &str) -> bool {
    p.len() <= 128
        && p.split('.').count() >= 2
        && p.split('.').all(|part| {
            let mut c = part.chars();
            c.next().is_some_and(|f| f.is_ascii_alphabetic())
                && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn package_names() {
        assert!(super::valid_package("de.samuel.usbcd"));
        assert!(!super::valid_package("de.samuel.usbcd; reboot"));
        assert!(!super::valid_package("usbcd"));
        assert!(!super::valid_package("de..x"));
        assert!(!super::valid_package("de.1x"));
    }
}
