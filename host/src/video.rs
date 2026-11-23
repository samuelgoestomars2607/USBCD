//! Bildschirmaufnahme + Hardware-Encoding über ffmpeg (≥ 8.0, gfxcapture),
//! anschließend Zerlegung in einzelne Bilder und Versand zum Tablet.

use crate::config::Config;
use crate::display::GpuVendor;
use crate::nal::{AnnexBSplitter, Codec};
use crate::proto::{write_video, VF_CONFIG, VF_KEY};
use crate::util::command;
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::process::Stdio;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    Nvenc,
    Amf,
    Qsv,
    Software,
}

impl Encoder {
    /// PCI-Herstellerkennung des Grafikchips, den dieser Encoder braucht.
    pub fn vendor_id(self) -> Option<u32> {
        match self {
            Encoder::Nvenc => Some(0x10DE),
            Encoder::Amf => Some(0x1002),
            Encoder::Qsv => Some(0x8086),
            Encoder::Software => None,
        }
    }

    pub fn pick(cfg: &Config, vendor: GpuVendor) -> Encoder {
        match cfg.encoder.as_str() {
            "nvenc" => Encoder::Nvenc,
            "amf" => Encoder::Amf,
            "qsv" => Encoder::Qsv,
            "software" => Encoder::Software,
            _ => match vendor {
                GpuVendor::Nvidia => Encoder::Nvenc,
                GpuVendor::Amd => Encoder::Amf,
                GpuVendor::Intel => Encoder::Qsv,
                GpuVendor::Unknown => Encoder::Software,
            },
        }
    }
}

pub struct StreamParams {
    pub hmonitor: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: Codec,
    pub encoder: Encoder,
    pub bitrate: u64,
    /// Grafikchip (PCI-Hersteller) für die Aufnahme; None = Windows-Standard
    pub capture_gpu: Option<u32>,
    /// gleichmäßiger Bildstrom (Kopien füllen Pausen); false = nur bei Änderungen
    pub dup_frames: bool,
    /// Dekoder-Prüfung beim ersten Verbinden: 5 Bilder/s, damit das Tablet eindeutig
    /// messen kann, ob es jedes Bild sofort ausgibt (unsichtbar hinter dem Verbindungsbildschirm)
    pub probe: bool,
}

/// Bildrate während der Dekoder-Prüfung (alle 200 ms ein Bild)
pub const PROBE_FPS: u32 = 5;

/// Läuft gerade die Dekoder-Prüfung?
static PROBING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_probing(on: bool) {
    PROBING.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_probing() -> bool {
    PROBING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Kann diese FFmpeg-Version Pausen mit Kopien füllen (nötig für die Prüfung)?
pub fn has_dup_frames() -> bool {
    ffmpeg_has_dup_frames()
}

/// Läuft die Übertragung gerade „nur bei Änderungen“? (für Anfragen des Tablets)
static SPARSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_sparse(on: bool) {
    SPARSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_sparse() -> bool {
    SPARSE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Bitrate automatisch: ~0,12 Bit pro Pixel und Bild (HEVC 25 % weniger), 20–80 Mbit/s –
/// genug für scharfe Schrift; über USB problemlos.
pub fn auto_bitrate(w: u32, h: u32, fps: u32, codec: Codec) -> u64 {
    let mut b = w as f64 * h as f64 * fps as f64 * 0.12;
    if codec == Codec::Hevc {
        b *= 0.75;
    }
    b.clamp(20e6, 80e6) as u64
}

pub fn ffmpeg_args(cfg: &Config, p: &StreamParams) -> Vec<String> {
    let cursor = if cfg.capture_cursor { 1 } else { 0 };
    let src = if cfg.capture_method == "ddagrab" {
        format!(
            "ddagrab=output_idx={}:framerate={}:draw_mouse={cursor}",
            cfg.ddagrab_output_idx, p.fps
        )
    } else {
        format!(
            "gfxcapture=hmonitor={}:max_framerate={}:capture_cursor={cursor}{}:width={}:height={}:resize_mode=scale_aspect",
            p.hmonitor,
            if p.probe { PROBE_FPS } else { p.fps },
            // erst ab FFmpeg > 9.0 vorhanden
            if (p.dup_frames || p.probe) && ffmpeg_has_dup_frames() { ":dup_frames=1" } else { "" },
            p.width,
            p.height
        )
    };
    let post = match p.encoder {
        Encoder::Nvenc | Encoder::Amf => "",
        Encoder::Qsv => ",hwmap=derive_device=qsv,format=qsv,vpp_qsv=format=nv12",
        Encoder::Software => ",hwdownload,format=bgra,format=yuv420p",
    };
    let c = match p.codec {
        Codec::Avc => "h264",
        Codec::Hevc => "hevc",
    };
    let b = p.bitrate.to_string();
    let buf = (p.bitrate / p.fps.max(1) as u64 * 4).to_string(); // ~4 Bilder Puffer
    let gop = (p.fps * 2).to_string();

    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "warning", "-nostdin"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Aufnahme auf dem Grafikchip, der den Tablet-Monitor zeichnet und kodiert –
    // auf Laptops mit zwei Grafikchips muss Windows die Bilder dann nicht hin- und herkopieren
    if let (Some(v), false) = (p.capture_gpu, cfg.capture_method == "ddagrab") {
        a.push("-init_hw_device".into());
        a.push(format!("d3d11va=usbcd:,vendor_id=0x{v:04x}"));
        a.push("-filter_hw_device".into());
        a.push("usbcd".into());
    }
    a.push("-filter_complex".into());
    a.push(format!("{src}{post}[v]"));
    // passthrough: Bilder 1:1 weitergeben. Sonst füllt ffmpeg Pausen mit Kopien auf
    // und schiebt sie gesammelt nach – das erzeugte Sekunden an Verzögerung.
    a.extend(["-map", "[v]", "-an", "-fps_mode", "passthrough"].map(String::from));

    let enc: Vec<String> = match p.encoder {
        Encoder::Nvenc => {
            let mut v = vec![
                format!("{c}_nvenc"),
                "-preset".into(),
                "p1".into(),
                "-tune".into(),
                "ull".into(),
                "-rc".into(),
                "cbr".into(),
                "-zerolatency".into(),
                "1".into(),
                "-delay".into(),
                "0".into(),
                "-forced-idr".into(),
                "1".into(),
                "-bf".into(),
                "0".into(),
            ];
            if p.codec == Codec::Avc {
                v.extend(["-profile:v".into(), "high".into()]);
            }
            v
        }
        Encoder::Amf => {
            let mut v = vec![
                format!("{c}_amf"),
                "-usage".into(),
                "ultralowlatency".into(),
                "-quality".into(),
                "speed".into(),
                "-rc".into(),
                "cbr".into(),
            ];
            if p.codec == Codec::Avc {
                v.extend(["-bf".into(), "0".into()]);
            }
            v
        }
        Encoder::Qsv => vec![
            format!("{c}_qsv"),
            "-preset".into(),
            "veryfast".into(),
            "-async_depth".into(),
            "1".into(),
            "-low_delay_brc".into(),
            "1".into(),
            "-bf".into(),
            "0".into(),
        ],
        Encoder::Software => vec![
            "libx264".into(),
            "-preset".into(),
            "ultrafast".into(),
            "-tune".into(),
            "zerolatency".into(),
            "-bf".into(),
            "0".into(),
        ],
    };
    a.push("-c:v".into());
    a.extend(enc);
    a.extend([
        "-b:v".into(),
        b.clone(),
        "-maxrate".into(),
        b,
        "-bufsize".into(),
        buf,
        "-g".into(),
        gop,
        "-f".into(),
        c.into(),
        "-flush_packets".into(),
        "1".into(),
        "-avioflags".into(),
        "direct".into(),
        "pipe:1".into(),
    ]);
    a
}

static DUP_FRAMES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn ffmpeg_has_dup_frames() -> bool {
    DUP_FRAMES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Prüft, ob ffmpeg vorhanden ist und den gfxcapture-Filter kennt; merkt sich,
/// welche Optionen die installierte Version unterstützt.
pub fn check_ffmpeg(path: &str) -> Result<bool> {
    let o = command(path)
        .args(["-hide_banner", "-h", "filter=gfxcapture"])
        .output()
        .with_context(|| {
            format!("ffmpeg nicht startbar ({path}) – USBCD-Setup erneut ausführen")
        })?;
    let s = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    DUP_FRAMES.store(
        s.contains("dup_frames"),
        std::sync::atomic::Ordering::Relaxed,
    );
    Ok(s.contains("gfxcapture") && !s.to_lowercase().contains("unknown filter"))
}

/// Tablet-Einstellungen (Helligkeit, Touch, Info-Anzeige, „Nicht stören“, Lautstärketasten) senden.
pub fn send_settings(sock: &mut TcpStream) -> std::io::Result<()> {
    let c = crate::config::current();
    let b = c.tablet_brightness.clamp(5, 100) as f32 / 100.0;
    let j = serde_json::json!({
        "brightness": b,
        "input": c.tablet_input,
        "stats": c.show_stats,
        "dnd": c.tablet_dnd,
        // nur, wenn der Windows-Regler den Tablet-Ton auch wirklich beeinflusst
        "volume_keys": c.tablet_volume_keys && c.audio_enabled && c.volume_mode != "off",
    })
    .to_string();
    crate::proto::write_frame(sock, crate::proto::T_SETTINGS, j.as_bytes())
}

/// Zeitbasis für PING/PONG.
static CLOCK: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// Letzte gemessene Laufzeit PC → Tablet → PC in µs (für die Info-Anzeige).
pub static LAST_RTT_US: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn clock_ns() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Antwort des Tablets auf ein PING auswerten.
pub fn on_pong(payload: &[u8]) {
    if payload.len() >= 8 {
        let sent = u64::from_be_bytes(payload[..8].try_into().unwrap_or_default());
        let rtt = clock_ns().saturating_sub(sent) / 1000;
        if rtt < 10_000_000 {
            LAST_RTT_US.store(rtt as u32, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

fn send_ping(sock: &mut TcpStream) -> std::io::Result<()> {
    let mut p = Vec::with_capacity(12);
    p.extend_from_slice(&clock_ns().to_be_bytes());
    p.extend_from_slice(
        &LAST_RTT_US
            .load(std::sync::atomic::Ordering::Relaxed)
            .to_be_bytes(),
    );
    crate::proto::write_frame(sock, crate::proto::T_PING, &p)
}

pub enum StreamEnd {
    /// ffmpeg hat sich beendet – Neustart sinnvoll
    EncoderExited,
    /// Einstellung geändert (Bildqualität, Ton) – Encoder mit neuen Werten neu starten,
    /// Verbindung und Monitor bleiben bestehen
    Restart,
}

/// Startet ffmpeg und schickt jedes fertige Bild an das Tablet. Kehrt mit
/// Err zurück, wenn die Verbindung zum Tablet abreißt.
pub fn stream(cfg: &Config, p: &StreamParams, sock: &mut TcpStream) -> Result<StreamEnd> {
    let args = ffmpeg_args(cfg, p);
    crate::log!("ffmpeg {}", args.join(" "));
    let mut child = command(&cfg.ffmpeg_exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("ffmpeg konnte nicht gestartet werden")?;
    crate::util::bind_to_us(&child);

    if let Some(err) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                // harmlose Meldungen bei gleichmäßigem Bildstrom nicht protokollieren
                if line.contains("non monotonically increasing dts")
                    || line.contains("Last message repeated")
                {
                    continue;
                }
                crate::log!("ffmpeg: {line}");
            }
        });
    }
    let mut out = child.stdout.take().context("ffmpeg stdout")?;
    // Lesen in eigenem Thread: so merken wir, wann ffmpeg mit einem Bild fertig
    // ist (kurze Schreibpause) und können es ohne ein Bild Verzögerung senden.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match out.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut splitter = AnnexBSplitter::new(p.codec);
    let mut aus = Vec::new();
    let mut frames: u64 = 0;
    let mut last_report = Instant::now();
    let idle = Duration::from_micros(1500);
    let mut pending = false;
    let mut last_ping = Instant::now();
    let mut cam_sent = String::new();

    let res: Result<StreamEnd> = loop {
        // nie unbegrenzt blockieren, damit „Beenden“ greift; 100 ms, damit die Anzeige
        // der Lautstärke auch bei stehendem Bild sofort reagiert
        let msg = rx.recv_timeout(if pending {
            idle
        } else {
            Duration::from_millis(100)
        });
        let eof = match msg {
            Ok(chunk) => {
                splitter.push(&chunk, &mut aus);
                pending = true;
                false
            }
            Err(RecvTimeoutError::Timeout) => {
                if crate::util::take_reconnect() {
                    break Ok(StreamEnd::Restart);
                }
                if pending {
                    splitter.flush_pending(&mut aus);
                    pending = false;
                }
                if crate::util::shutdown() {
                    break Err(crate::session::Disconnected.into());
                }
                if crate::util::paused() {
                    break Err(crate::session::Paused.into());
                }
                if crate::util::take_end_session() {
                    break Err(crate::session::Disconnected.into());
                }
                false
            }
            Err(RecvTimeoutError::Disconnected) => {
                splitter.flush_pending(&mut aus);
                true
            }
        };
        let mut failed = None;
        if crate::util::take_settings_dirty() {
            if let Err(e) = send_settings(sock) {
                failed = Some(e);
            }
        }
        // Lautstärketaste am Tablet: neue Lautstärke zur Anzeige zurückmelden
        if let Some(v) = crate::audio::take_volume_report() {
            if let Err(e) = crate::proto::write_frame(sock, crate::proto::T_VOLUME, v.as_bytes()) {
                failed = Some(e);
            }
        }
        // Webcam: Tablet-Kamera nur einschalten, während eine App sie benutzt
        let cam = crate::camera::control_json();
        if cam != cam_sent {
            match crate::proto::write_frame(sock, crate::proto::T_CAM, cam.as_bytes()) {
                Ok(()) => cam_sent = cam,
                Err(e) => failed = Some(e),
            }
        }
        if last_ping.elapsed() >= Duration::from_secs(1) {
            last_ping = Instant::now();
            if let Err(e) = send_ping(sock) {
                failed = Some(e);
            }
        }
        for au in aus.drain(..) {
            // fortlaufend über Encoder-Neustarts hinweg (das Tablet sieht nie Rücksprünge)
            let pts = clock_ns() / 1000;
            if !au.config.is_empty() {
                if let Err(e) = write_video(sock, VF_CONFIG, pts, &au.config) {
                    failed = Some(e);
                    break;
                }
            }
            let flags = if au.key { VF_KEY } else { 0 };
            if let Err(e) = write_video(sock, flags, pts, &au.data) {
                failed = Some(e);
                break;
            }
            frames += 1;
        }
        if let Some(e) = failed {
            crate::log!("Video-Verbindung beendet: {e}");
            break Err(crate::session::Disconnected.into());
        }
        if eof {
            break Ok(StreamEnd::EncoderExited);
        }
        if last_report.elapsed().as_secs() >= 600 {
            let secs = last_report.elapsed().as_secs_f32();
            if is_sparse() {
                crate::log!("Video: {:.1} Bilder/s (nur bei Änderungen)", frames as f32 / secs);
            } else {
                crate::log!("Video: {:.1} Bilder/s", frames as f32 / secs);
            }
            frames = 0;
            last_report = Instant::now();
        }
    };
    // Grund festhalten, wenn ffmpeg von selbst endet (hilft bei der Fehlersuche)
    if matches!(res, Ok(StreamEnd::EncoderExited)) {
        for _ in 0..10 {
            if let Ok(Some(status)) = child.try_wait() {
                crate::log!("ffmpeg endete mit {status}");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(capture_gpu: Option<u32>) -> StreamParams {
        StreamParams {
            hmonitor: 1,
            width: 2000,
            height: 1200,
            fps: 60,
            codec: Codec::Hevc,
            encoder: Encoder::Nvenc,
            bitrate: 40_000_000,
            capture_gpu,
            dup_frames: true,
            probe: false,
        }
    }

    #[test]
    fn capture_on_render_gpu() {
        let cfg = Config::default();
        let a = ffmpeg_args(&cfg, &params(Some(0x10DE)));
        let i = a.iter().position(|s| s == "-init_hw_device").expect("Gerät fehlt");
        assert_eq!(a[i + 1], "d3d11va=usbcd:,vendor_id=0x10de");
        assert_eq!(a[i + 2..i + 4], ["-filter_hw_device", "usbcd"]);
        // muss vor dem Filtergraphen stehen
        assert!(i < a.iter().position(|s| s == "-filter_complex").unwrap());
        // unbekannter Chip: wie bisher ohne eigenes Gerät
        assert!(!ffmpeg_args(&cfg, &params(None)).contains(&"-init_hw_device".to_string()));
    }

    #[test]
    fn sparse_without_dup_frames() {
        let cfg = Config::default();
        DUP_FRAMES.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut p = params(None);
        assert!(ffmpeg_args(&cfg, &p).iter().any(|s| s.contains(":dup_frames=1")));
        p.dup_frames = false;
        assert!(!ffmpeg_args(&cfg, &p).iter().any(|s| s.contains("dup_frames")));
        // Prüfung: 5 Bilder/s mit Kopien, unabhängig vom Modus
        p.probe = true;
        let a = ffmpeg_args(&cfg, &p);
        assert!(a.iter().any(|s| s.contains(":max_framerate=5:") && s.contains(":dup_frames=1")));
    }
}
