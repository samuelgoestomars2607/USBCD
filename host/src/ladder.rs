//! Welche Übertragung ein Tablet bekommt: eine Reihenfolge von Stufen, von der besten bis
//! zu der, die praktisch immer geht. Scheitert eine Stufe auf dem Tablet, nimmt der PC die
//! nächste – so bekommt jedes Tablet ein Bild, und zwar das beste, das sein Chip schafft.
//!
//! Regel:
//! 1. Hardware-Decoder vor Software-Decoder (Software dekodiert langsamer, Tablet wird warm).
//! 2. Volle Bildrate, wenn sie mit höchstens etwa 80 % der Kantenlänge geht.
//! 3. Sonst Auflösung zuerst, mit mindestens 30 Bildern pro Sekunde.
//! 4. Sonst, was eben geht (höhere Bildrate zuerst) – lieber so als gar nicht.

use serde::{Deserialize, Serialize};

/// Was ein Decoder des Tablets bei einer Größe schafft (aus dem HELLO der App).
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct DecoderMode {
    /// "hevc" oder "avc"
    pub codec: String,
    /// Name des Decoders auf dem Tablet (z. B. „c2.exynos.hevc.decoder“)
    pub decoder: String,
    pub hw: bool,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
}

/// Eine Stufe: so wird übertragen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub codec: String,
    /// leer = die App wählt selbst (ältere App oder Notnagel)
    #[serde(default)]
    pub decoder: String,
    #[serde(default)]
    pub hw: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Step {
    pub fn label(&self) -> String {
        format!(
            "{} {} {}x{} @ {}",
            if self.codec == "hevc" { "HEVC" } else { "H.264" },
            if self.hw { "Hardware" } else { "Software" },
            self.width,
            self.height,
            self.fps
        )
    }
}

pub struct Limits {
    /// Größe des Tablet-Displays (Querformat)
    pub native: (u32, u32),
    /// gewünschte Bildrate (Bildwiederholrate des Tablets, ggf. begrenzt)
    pub target_fps: u32,
    pub allow_hevc: bool,
    pub allow_avc: bool,
    /// H.264 vor HEVC (Einstellung codec = "avc")
    pub prefer_avc: bool,
}

/// unter dieser Bildrate gilt eine Stufe nicht mehr als flüssig
pub const SMOOTH_FPS: u32 = 30;
/// darunter wird eine Stufe gar nicht angeboten
pub const MIN_FPS: u32 = 10;
/// höchstens so viele Stufen (mehr Ausweichen ergibt keinen Sinn)
const MAX_STEPS: usize = 14;

fn codec_ok(lim: &Limits, codec: &str) -> bool {
    match codec {
        "hevc" => lim.allow_hevc,
        "avc" => lim.allow_avc,
        _ => false,
    }
}

/// Reihenfolge der Stufen nach der Regel oben. Leer nur, wenn kein erlaubter Codec gemeldet
/// wurde – dann hängt der Aufrufer einen Notnagel an (siehe `with_safety_net`).
pub fn build(modes: &[DecoderMode], lim: &Limits) -> Vec<Step> {
    let (nw, nh) = lim.native;
    let native_area = (nw as u64 * nh as u64).max(1);
    let target = lim.target_fps.max(1);
    struct Ranked {
        step: Step,
        tier: u8,
        area: u64,
        codec_rank: u8,
    }
    let mut ranked: Vec<Ranked> = Vec::new();
    for m in modes {
        if !codec_ok(lim, &m.codec) || m.width == 0 || m.height == 0 {
            continue;
        }
        if m.width > nw || m.height > nh {
            continue;
        }
        // unbekannte Bildrate (0): Hardware wie gewünscht, Software vorsichtig
        let claimed = if m.fps.is_finite() && m.fps > 0.0 {
            m.fps.floor() as u32
        } else if m.hw {
            target
        } else {
            SMOOTH_FPS
        };
        let fps = claimed.min(target);
        if fps < MIN_FPS {
            continue;
        }
        let area = m.width as u64 * m.height as u64;
        let full = fps + 1 >= target;
        // „höchstens eine Stufe kleiner“: ≥ 60 % der Fläche (≈ 78 % der Kantenlänge)
        let near_native = area * 100 >= native_area * 60;
        let tier = match (m.hw, full && near_native, fps >= SMOOTH_FPS) {
            (true, true, _) => 0,
            (true, false, true) => 1,
            (false, true, _) => 2,
            (false, false, true) => 3,
            (true, false, false) => 4,
            (false, false, false) => 5,
        };
        let codec_rank = match (m.codec.as_str(), lim.prefer_avc) {
            ("hevc", false) | ("avc", true) => 0,
            _ => 1,
        };
        ranked.push(Ranked {
            step: Step {
                codec: m.codec.clone(),
                decoder: m.decoder.clone(),
                hw: m.hw,
                width: m.width,
                height: m.height,
                fps,
            },
            tier,
            area,
            codec_rank,
        });
    }
    ranked.sort_by(|a, b| {
        a.tier.cmp(&b.tier).then_with(|| {
            if a.tier >= 4 {
                // unter 30 Bildern: flüssiger zuerst, dann größer
                b.step.fps.cmp(&a.step.fps).then(b.area.cmp(&a.area))
            } else {
                b.area.cmp(&a.area).then(b.step.fps.cmp(&a.step.fps))
            }
            .then(a.codec_rank.cmp(&b.codec_rank))
        })
    });
    let mut out: Vec<Step> = Vec::new();
    for r in ranked {
        let dup = out.iter().any(|s| {
            s.codec == r.step.codec
                && s.decoder == r.step.decoder
                && s.width == r.step.width
                && s.height == r.step.height
        });
        if !dup {
            out.push(r.step);
        }
        if out.len() >= MAX_STEPS {
            break;
        }
    }
    out
}

/// Kantenlänge in festem Seitenverhältnis verkleinern (durch 8 teilbar).
pub fn scaled(native: (u32, u32), factor: f32) -> (u32, u32) {
    let h = ((native.1 as f32 * factor) as u32 / 8 * 8).max(8);
    let w = ((h as u64 * native.0 as u64 / native.1.max(1) as u64) as u32 / 8 * 8).max(8);
    (w, h)
}

/// Notnagel ans Ende: H.264 in halber Größe mit 30 Bildern, Decoder wählt die App
/// (Android bringt immer einen H.264-Decoder mit). Nur wenn H.264 erlaubt ist.
pub fn with_safety_net(mut steps: Vec<Step>, lim: &Limits) -> Vec<Step> {
    if !lim.allow_avc {
        return steps;
    }
    let (w, h) = scaled(lim.native, 0.5);
    let net = Step {
        codec: "avc".into(),
        decoder: String::new(),
        hw: false,
        width: w,
        height: h,
        fps: SMOOTH_FPS.min(lim.target_fps.max(MIN_FPS)),
    };
    let covered = steps
        .iter()
        .any(|s| s.codec == "avc" && s.width <= w && s.height <= h);
    if !covered {
        steps.push(net);
    }
    steps
}

/// Was sich der PC je Tablet merkt, wenn Stufen gescheitert sind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Fallback {
    /// erste Stufe, die nicht gescheitert ist (None = keine ging)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<Step>,
    /// App-Version und Android-Stand, für die das gilt (sonst neu probieren)
    pub sig: String,
    /// Zeitpunkt (Unix-Sekunden)
    pub since: u64,
}

/// So lange gilt das Gemerkte, danach wird wieder von oben probiert (Treiber-Updates usw.).
pub const REMEMBER_SECS: u64 = 14 * 24 * 3600;

/// Kurzer Fingerabdruck aus App-Version und Android-Stand (kein Klartext in usbcd.toml).
pub fn signature(app_version: u64, build: &str) -> String {
    // FNV-1a, 64 Bit – reicht zum Wiedererkennen, kein Geheimnis
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in build.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{app_version}-{h:016x}")
}

/// Wo beginnen? Index in `steps`; `None` = gemerkt, dass nichts geht.
pub fn start_index(steps: &[Step], remembered: Option<&Fallback>, sig: &str, now: u64) -> Option<usize> {
    let Some(f) = remembered else { return Some(0) };
    if f.sig != sig || now.saturating_sub(f.since) > REMEMBER_SECS {
        return Some(0);
    }
    f.step
        .as_ref()
        .map(|s| steps.iter().position(|x| x == s).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(codec: &str, dec: &str, hw: bool, w: u32, h: u32, fps: f32) -> DecoderMode {
        DecoderMode {
            codec: codec.into(),
            decoder: dec.into(),
            hw,
            width: w,
            height: h,
            fps,
        }
    }

    fn lim() -> Limits {
        Limits {
            native: (1920, 1200),
            target_fps: 60,
            allow_hevc: true,
            allow_avc: true,
            prefer_avc: false,
        }
    }

    fn sizes(v: &[Step]) -> Vec<(String, bool, u32, u32, u32)> {
        v.iter()
            .map(|s| (s.codec.clone(), s.hw, s.width, s.height, s.fps))
            .collect()
    }

    #[test]
    fn strong_tablet_unchanged() {
        // wie das Tab S6 Lite: alles nativ mit 60 → HEVC 1920×1200 @ 60 zuerst
        let modes = [
            m("hevc", "c2.exynos.hevc.decoder", true, 1920, 1200, 60.0),
            m("hevc", "c2.exynos.hevc.decoder", true, 1536, 960, 120.0),
            m("avc", "c2.exynos.h264.decoder", true, 1920, 1200, 60.0),
            m("avc", "c2.android.avc.decoder", false, 1920, 1200, 30.0),
        ];
        let s = build(&modes, &lim());
        assert_eq!(s[0].label(), "HEVC Hardware 1920x1200 @ 60");
        assert_eq!(s[1].label(), "H.264 Hardware 1920x1200 @ 60");
        assert_eq!(s[2].label(), "HEVC Hardware 1536x960 @ 60");
    }

    #[test]
    fn tab_a8_like_limit_1080() {
        // Chip dekodiert höchstens 1080 Zeilen: größte Stufe 1728×1080
        let modes = [
            m("hevc", "c2.unisoc.hevc.decoder", true, 1728, 1080, 60.0),
            m("hevc", "c2.unisoc.hevc.decoder", true, 1536, 960, 60.0),
            m("avc", "c2.unisoc.avc.decoder", true, 1728, 1080, 60.0),
            m("avc", "c2.android.avc.decoder", false, 1728, 1080, 24.0),
            m("avc", "c2.android.avc.decoder", false, 960, 600, 60.0),
        ];
        let s = with_safety_net(build(&modes, &lim()), &lim());
        assert_eq!(
            sizes(&s),
            vec![
                ("hevc".into(), true, 1728, 1080, 60),
                ("avc".into(), true, 1728, 1080, 60),
                ("hevc".into(), true, 1536, 960, 60),
                ("avc".into(), false, 960, 600, 60),
                ("avc".into(), false, 1728, 1080, 24),
            ]
        );
    }

    #[test]
    fn full_rate_one_step_down_beats_native_at_30() {
        let modes = [
            m("hevc", "hw", true, 1920, 1200, 30.0),
            m("hevc", "hw", true, 1536, 960, 60.0),
        ];
        let s = build(&modes, &lim());
        assert_eq!((s[0].width, s[0].fps), (1536, 60));
        assert_eq!((s[1].width, s[1].fps), (1920, 30));
    }

    #[test]
    fn resolution_first_above_30() {
        // nur kleine Stufen schaffen 60: dann lieber groß mit 30–45
        let modes = [
            m("hevc", "hw", true, 1920, 1200, 40.0),
            m("hevc", "hw", true, 960, 600, 60.0),
        ];
        let s = build(&modes, &lim());
        assert_eq!((s[0].width, s[0].fps), (1920, 40));
        assert_eq!((s[1].width, s[1].fps), (960, 60));
    }

    #[test]
    fn below_30_smoother_first_and_hardware_before_software() {
        let modes = [
            m("avc", "hw", true, 1920, 1200, 12.0),
            m("avc", "hw", true, 960, 600, 25.0),
            m("avc", "sw", false, 1216, 760, 30.0),
            m("avc", "sw", false, 960, 600, 5.0), // zu langsam → weg
        ];
        let s = build(&modes, &lim());
        assert_eq!(
            sizes(&s),
            vec![
                ("avc".into(), false, 1216, 760, 30),
                ("avc".into(), true, 960, 600, 25),
                ("avc".into(), true, 1920, 1200, 12),
            ]
        );
    }

    #[test]
    fn respects_codec_setting_and_size() {
        let mut l = lim();
        l.allow_hevc = false; // z. B. Software-Encoder am PC
        let modes = [
            m("hevc", "hw", true, 1920, 1200, 60.0),
            m("avc", "hw", true, 1920, 1200, 60.0),
            m("avc", "hw", true, 4000, 2400, 60.0), // größer als das Display → weg
            m("vp9", "hw", true, 1920, 1200, 60.0),
        ];
        let s = build(&modes, &l);
        assert_eq!(sizes(&s), vec![("avc".into(), true, 1920, 1200, 60)]);
        // gewünscht H.264: bei Gleichstand vor HEVC
        let mut l = lim();
        l.prefer_avc = true;
        let s = build(&modes[..2], &l);
        assert_eq!(s[0].codec, "avc");
    }

    #[test]
    fn safety_net_when_nothing_reported() {
        let s = with_safety_net(build(&[], &lim()), &lim());
        assert_eq!(sizes(&s), vec![("avc".into(), false, 960, 600, 30)]);
        assert!(s[0].decoder.is_empty());
        let mut l = lim();
        l.allow_avc = false;
        assert!(with_safety_net(Vec::new(), &l).is_empty());
    }

    #[test]
    fn remembered_start() {
        let modes = [
            m("hevc", "hw", true, 1920, 1200, 60.0),
            m("avc", "hw", true, 1920, 1200, 60.0),
        ];
        let s = build(&modes, &lim());
        let sig = signature(21, "samsung/gta8/…");
        let f = Fallback {
            step: Some(s[1].clone()),
            sig: sig.clone(),
            since: 1000,
        };
        assert_eq!(start_index(&s, Some(&f), &sig, 2000), Some(1));
        // andere App-/Android-Version oder zu alt → wieder von oben
        assert_eq!(start_index(&s, Some(&f), &signature(22, "x"), 2000), Some(0));
        assert_eq!(start_index(&s, Some(&f), &sig, 1000 + REMEMBER_SECS + 1), Some(0));
        // gemerkte Stufe gibt es nicht mehr → oben
        let gone = Fallback {
            step: Some(Step {
                width: 1,
                ..s[0].clone()
            }),
            ..f.clone()
        };
        assert_eq!(start_index(&s, Some(&gone), &sig, 2000), Some(0));
        // gemerkt: nichts ging
        let none = Fallback { step: None, ..f };
        assert_eq!(start_index(&s, Some(&none), &sig, 2000), None);
        assert_eq!(start_index(&s, None, &sig, 2000), Some(0));
        assert_ne!(signature(21, "a"), signature(21, "b"));
    }

    #[test]
    fn scaled_sizes() {
        assert_eq!(scaled((1920, 1200), 0.5), (960, 600));
        assert_eq!(scaled((2000, 1200), 0.8), (1600, 960));
    }
}
