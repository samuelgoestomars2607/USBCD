//! Drahtprotokoll zwischen PC und Tablet (identisch in der Android-App, Proto.kt).
//!
//! Jede Nachricht: [Typ: u8][Länge: u32 big-endian][Nutzdaten]
//!
//! Jede Verbindung beginnt mit der gegenseitigen Anmeldung (siehe auth.rs):
//!   Tablet → PC  CHALLENGE, PC → Tablet AUTH, Tablet → PC AUTH_OK
//!
//! Video-Verbindung (Port 27183):
//!   Tablet → PC  HELLO   JSON mit Display- und Decoder-Infos
//!   PC → Tablet  CONFIG  JSON {codec,width,height,fps}
//!   PC → Tablet  VIDEO   [flags u8][pts µs u64][Annex-B-Daten]
//!   PC → Tablet  SETTINGS JSON {brightness: 0.05–1.0}
//!                        flags: 1 = Schlüsselbild, 2 = Codec-Konfiguration
//!   PC → Tablet  CAM     JSON {enabled,on,width,height,fps,key} – Webcam (camera.rs)
//!   Tablet → PC  CAM_STATE JSON {state,rotation,width,height}
//!   Tablet → PC  CAM_DATA  [flags u8][H.264 Annex-B] der Selfie-Kamera
//!   Tablet → PC  VOLUME_KEY [Taste u8: 1 lauter, 2 leiser, 3 stumm] (audio.rs)
//!   PC → Tablet  VOLUME  JSON {level: 0–100, muted} – Anzeige nach einem Tastendruck
//!   Tablet → PC  DECODER JSON {prompt} – gibt der Dekoder jedes Bild sofort aus? (session.rs)
//!   Tablet → PC  KEY_REQUEST – Tablet wartet auf ein Schlüsselbild
//!   PC → Tablet  STILL   [Breite u32][Höhe u32][Format u8][Daten] – verlustfreies Standbild,
//!                        liegt über dem Video bis zum nächsten Videobild (still.rs)
//! Audio-Verbindung (Port 27184):
//!   PC → Tablet  AFMT    JSON {rate,channels}
//!   PC → Tablet  PCM     16-Bit little-endian, verschachtelt

use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Read, Write};

pub const T_HELLO: u8 = 0x01;
pub const T_CONFIG: u8 = 0x02;
pub const T_VIDEO: u8 = 0x03;
pub const T_SETTINGS: u8 = 0x04;
pub const T_AUTH: u8 = 0x05;
pub const T_CHALLENGE: u8 = 0x06;
pub const T_AUTH_OK: u8 = 0x07;
/// Tablet → PC: Touch-/Stift-Ereignis (siehe input.rs)
pub const T_INPUT: u8 = 0x08;
/// PC → Tablet: [u64 Zeitstempel ns][u32 letzte Laufzeit µs] – Tablet antwortet mit PONG
pub const T_PING: u8 = 0x09;
/// Tablet → PC: die ersten 8 Byte des PING zurück
pub const T_PONG: u8 = 0x0A;
pub const T_AFMT: u8 = 0x10;
pub const T_PCM: u8 = 0x11;
/// PC → Tablet: Webcam-Steuerung
pub const T_CAM: u8 = 0x12;
/// Tablet → PC: Zustand der Selfie-Kamera
pub const T_CAM_STATE: u8 = 0x13;
/// Tablet → PC: H.264-Daten der Selfie-Kamera
pub const T_CAM_DATA: u8 = 0x14;
/// Tablet → PC: Lautstärketaste des Tablets
pub const T_VOLUME_KEY: u8 = 0x15;
/// PC → Tablet: neue Lautstärke des Tablet-Lautsprechers (JSON)
pub const T_VOLUME: u8 = 0x16;
/// Tablet → PC: Prüfergebnis des Dekoders (JSON {prompt: bool})
pub const T_DECODER: u8 = 0x17;
/// Tablet → PC: bitte ein Schlüsselbild (Dekoder neu gestartet oder Bilder verworfen)
pub const T_KEY_REQUEST: u8 = 0x18;
/// PC → Tablet: verlustfreies Standbild (siehe still.rs)
pub const T_STILL: u8 = 0x19;

pub const VF_KEY: u8 = 1;
pub const VF_CONFIG: u8 = 2;

#[cfg(test)]
pub const MAX_FRAME: usize = 64 << 20;

pub fn write_frame(w: &mut impl Write, t: u8, payload: &[u8]) -> io::Result<()> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.push(t);
    v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    v.extend_from_slice(payload);
    w.write_all(&v)
}

pub fn write_video(w: &mut impl Write, flags: u8, pts_us: u64, data: &[u8]) -> io::Result<()> {
    let len = 9 + data.len();
    let mut v = Vec::with_capacity(5 + len);
    v.push(T_VIDEO);
    v.extend_from_slice(&(len as u32).to_be_bytes());
    v.push(flags);
    v.extend_from_slice(&pts_us.to_be_bytes());
    v.extend_from_slice(data);
    w.write_all(&v)
}

#[cfg(test)]
pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    read_frame_max(r, MAX_FRAME)
}

/// Wie `read_frame`, aber mit eigener Höchstlänge (vor der Anmeldung nur kleine Nachrichten).
pub fn read_frame_max(r: &mut impl Read, max: usize) -> io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 5];
    r.read_exact(&mut h)?;
    let len = u32::from_be_bytes([h[1], h[2], h[3], h[4]]) as usize;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Nachricht zu groß",
        ));
    }
    let mut p = vec![0u8; len];
    r.read_exact(&mut p)?;
    Ok((h[0], p))
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct CodecCaps {
    pub supported: bool,
    pub hardware: bool,
    pub name: String,
    /// höchste dekodierbare Bildrate bei nativer Auflösung
    pub max_fps: f32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Hello {
    pub proto: u32,
    pub model: String,
    /// Gerätename, wie er im Tablet eingestellt ist (z. B. „Galaxy Tab S6 Lite“)
    pub name: String,
    /// Querformat: width ≥ height, physische Pixel
    pub width: u32,
    pub height: u32,
    pub refresh: f32,
    pub density_dpi: u32,
    pub codecs: HashMap<String, CodecCaps>,
}

impl Hello {
    /// Werte vom Tablet nur in sinnvollen Grenzen übernehmen.
    pub fn validate(&mut self) -> Result<(), String> {
        if !crate::vdd::mode_ok(self.width, self.height, self.refresh.round() as u32)
            || !self.refresh.is_finite()
        {
            return Err(format!(
                "{}x{} @ {} Hz",
                self.width, self.height, self.refresh
            ));
        }
        self.density_dpi = self.density_dpi.clamp(72, 1000);
        self.model = clean(&self.model);
        self.name = clean(&self.name);
        if self.codecs.len() > 8 {
            return Err("zu viele Codecs".into());
        }
        for c in self.codecs.values_mut() {
            if !c.max_fps.is_finite() {
                c.max_fps = 0.0;
            }
            c.max_fps = c.max_fps.clamp(0.0, crate::vdd::MAX_HZ as f32);
            c.name = clean(&c.name);
        }
        Ok(())
    }
}

/// Anzeigetext: ohne Steuerzeichen, höchstens 48 Zeichen.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(48)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_bounds() {
        let mut h = Hello {
            width: 2000,
            height: 1200,
            refresh: 60.0,
            name: "Tab\n\u{7}  S6".into(),
            ..Default::default()
        };
        assert!(h.validate().is_ok());
        assert_eq!(h.name, "Tab  S6");
        h.width = 99_999;
        assert!(h.validate().is_err());
        h.width = 2000;
        h.refresh = f32::NAN;
        assert!(h.validate().is_err());
    }

    #[test]
    fn small_limit() {
        let mut v = Vec::new();
        write_frame(&mut v, T_HELLO, &[0u8; 5000]).unwrap();
        assert!(read_frame_max(&mut &v[..], 4096).is_err());
        assert!(read_frame(&mut &v[..]).is_ok());
    }
}
