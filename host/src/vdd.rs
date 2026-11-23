//! Steuerung des "Virtual Display Driver" (github.com/VirtualDrivers/Virtual-Display-Driver).
//! Der Treiber erzeugt einen echten Monitor in Windows. Wir tragen die native
//! Auflösung und Bildrate des Tablets in seine vdd_settings.xml ein und schalten
//! das Gerät beim An-/Abstecken ein bzw. aus.

use anyhow::{bail, Context, Result};

/// Ersetzt den Inhalt des ersten `<tag>…</tag>`; fehlt der Abschnitt, wird er vor
/// `</vdd_settings>` eingefügt.
fn replace_section(xml: &str, tag: &str, inner: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(a) = xml.find(&open) {
        if let Some(b_rel) = xml[a..].find(&close) {
            let b = a + b_rel;
            return format!("{}{}{}{}", &xml[..a], open, inner, &xml[b..]);
        }
    }
    let block = format!("  {open}{inner}{close}\n");
    match xml.rfind("</vdd_settings>") {
        Some(p) => format!("{}{}{}", &xml[..p], block, &xml[p..]),
        None => format!(
            "<?xml version='1.0' encoding='utf-8'?>\n<vdd_settings>\n{block}</vdd_settings>\n"
        ),
    }
}

/// Auflösungen, die Windows für den Tablet-Monitor anbietet: die des Tablets und kleinere
/// im selben Seitenverhältnis (ca. 80 %, 64 %, 50 %; Kanten durch 8 teilbar, mind. 480 px
/// hoch). Eine kleinere Auflösung wird so übertragen und erst vom Tablet hochgerechnet –
/// das entlastet den PC. Ganz ohne passende kleinere Größe bleibt 1080p als Rückfall.
pub fn offered_modes(w: u32, h: u32, hz: u32) -> Vec<(u32, u32, u32)> {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let mut modes = vec![(w, h, hz)];
    let g = gcd(w, h).max(1);
    let (aw, ah) = (w / g, h / g);
    for pct in [80u32, 64, 50] {
        let target = (g * pct + 50) / 100;
        // nächstes n mit (n·aw, n·ah) durch 8 teilbar, sonst wenigstens gerade
        let pick = |m: u32| {
            (0..=g)
                .flat_map(|d| [target.saturating_sub(d), target + d])
                .find(|&n| n > 0 && n < g && (n * aw) % m == 0 && (n * ah) % m == 0)
        };
        if let Some(n) = pick(8).or_else(|| pick(2)) {
            let m = (n * aw, n * ah, hz);
            if m.1 >= 480 && !modes.contains(&m) {
                modes.push(m);
            }
        }
    }
    if modes.len() == 1 && (w, h) != (1920, 1080) {
        modes.push((1920, 1080, hz.min(60)));
    }
    modes
}

/// Trägt genau einen Monitor mit den Auflösungen aus `offered_modes` ein.
pub fn patch_settings(xml: &str, w: u32, h: u32, hz: u32) -> String {
    let mut x = replace_section(xml, "monitors", "\n    <count>1</count>\n  ");

    let modes = offered_modes(w, h, hz);
    let mut res = String::from("\n");
    for (w, h, r) in &modes {
        res.push_str(&format!(
            "    <resolution>\n      <width>{w}</width>\n      <height>{h}</height>\n      <refresh_rate>{r}</refresh_rate>\n    </resolution>\n"
        ));
    }
    res.push_str("  ");
    x = replace_section(&x, "resolutions", &res);

    let mut rates = vec![60, hz];
    rates.dedup();
    let mut g = String::from("\n");
    for r in rates {
        g.push_str(&format!("    <g_refresh_rate>{r}</g_refresh_rate>\n"));
    }
    g.push_str("  ");
    replace_section(&x, "global", &g)
}

/// Fester Ort der Treiber-Einstellungen. Wird nur vom USBCD-Hilfsdienst geschrieben;
/// der Ordner ist per Setup nur für Administratoren beschreibbar.
pub const SETTINGS_PATH: &str = r"C:\VirtualDisplayDriver\vdd_settings.xml";

/// Grenzen für Werte, die vom Tablet kommen.
pub const MIN_SIDE: u32 = 320;
pub const MAX_SIDE: u32 = 7680;
pub const MIN_HZ: u32 = 24;
pub const MAX_HZ: u32 = 240;

pub fn mode_ok(w: u32, h: u32, hz: u32) -> bool {
    (MIN_SIDE..=MAX_SIDE).contains(&w)
        && (MIN_SIDE..=MAX_SIDE).contains(&h)
        && (MIN_HZ..=MAX_HZ).contains(&hz)
}

fn is_reparse(p: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        std::fs::symlink_metadata(p)
            .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::fs::symlink_metadata(p)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }
}

/// Schreibt die Einstellungen; liefert true, wenn sich etwas geändert hat.
pub fn ensure_settings(w: u32, h: u32, hz: u32) -> Result<bool> {
    if !mode_ok(w, h, hz) {
        bail!("ungültiger Modus {w}x{h}@{hz}");
    }
    let path = std::path::Path::new(SETTINGS_PATH);
    // Verknüpfungen nie folgen: sonst ließe sich eine fremde Datei überschreiben
    if path.parent().is_some_and(is_reparse) || is_reparse(path) {
        bail!("{SETTINGS_PATH}: Verknüpfung statt Datei – abgelehnt");
    }
    let old = std::fs::read_to_string(path).with_context(|| {
        format!(
            "{SETTINGS_PATH} {}",
            crate::i18n::tr(
                "nicht gefunden – Virtual Display Driver fehlt – USBCD-Setup erneut ausführen",
                "not found – Virtual Display Driver missing – run the USBCD setup again"
            )
        )
    })?;
    let new = patch_settings(&old, w, h, hz);
    if new == old {
        return Ok(false);
    }
    let backup = path.with_extension("xml.vor-usbcd");
    if !backup.exists() {
        let _ = std::fs::write(&backup, &old);
    }
    std::fs::write(path, new).with_context(|| format!("{SETTINGS_PATH} nicht beschreibbar"))?;
    Ok(true)
}

/// Hardware-ID des Virtual Display Driver. Nur dieses Gerät wird je geschaltet –
/// andere virtuelle Monitore wie Parsec haben eine andere ID und bleiben unberührt.
pub const HWID: &str = r"Root\MttVDD";

fn set_state(enable: bool) -> Result<()> {
    use std::mem::{size_of, zeroed};
    use windows_sys::core::GUID;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
    const GUID_DEVCLASS_DISPLAY: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);
    let same = |a: &GUID, b: &GUID| {
        a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
    };
    let (mut ok, mut failed) = (0, 0);
    let found = crate::driver::for_each_device(HWID, |devs, info| unsafe {
        if !same(&info.ClassGuid, &GUID_DEVCLASS_DISPLAY) {
            return;
        }
        let mut p: SP_PROPCHANGE_PARAMS = zeroed();
        p.ClassInstallHeader.cbSize = size_of::<SP_CLASSINSTALL_HEADER>() as u32;
        p.ClassInstallHeader.InstallFunction = DIF_PROPERTYCHANGE;
        p.StateChange = if enable { DICS_ENABLE } else { DICS_DISABLE };
        p.Scope = DICS_FLAG_GLOBAL;
        p.HwProfile = 0;
        if SetupDiSetClassInstallParamsW(
            devs,
            info,
            &p.ClassInstallHeader,
            size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        ) != 0
            && SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, devs, info) != 0
        {
            ok += 1;
        } else {
            failed += 1;
        }
    });
    if found == 0 || ok + failed == 0 {
        bail!(
            "{}",
            crate::i18n::tr(
                "Virtual Display Driver nicht im Geräte-Manager gefunden – USBCD-Setup erneut ausführen",
                "Virtual Display Driver not found in Device Manager – run the USBCD setup again"
            )
        );
    }
    if ok == 0 {
        bail!(
            "Virtual Display Driver {}: {}",
            if enable { "einschalten" } else { "ausschalten" },
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

pub fn enable() -> Result<()> {
    set_state(true)
}

pub fn disable() -> Result<()> {
    set_state(false)
}

/// Aus- und wieder einschalten, damit der Treiber die XML neu einliest.
pub fn restart() -> Result<()> {
    let _ = disable();
    std::thread::sleep(std::time::Duration::from_millis(700));
    enable()
}

/// Monitor mit diesem Modus einschalten (Einstellungen schreiben, Treiber ggf. neu laden).
pub fn turn_on(w: u32, h: u32, hz: u32) -> Result<bool> {
    let changed = ensure_settings(w, h, hz)?;
    if changed {
        restart()?;
    } else {
        enable()?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<?xml version='1.0' encoding='utf-8'?>
<vdd_settings>
  <monitors>
    <count>3</count>
  </monitors>
  <gpu>
    <friendlyname>default</friendlyname>
  </gpu>
  <global>
    <g_refresh_rate>60</g_refresh_rate>
    <g_refresh_rate>90</g_refresh_rate>
  </global>
  <resolutions>
    <resolution>
      <width>800</width>
      <height>600</height>
      <refresh_rate>30</refresh_rate>
    </resolution>
  </resolutions>
</vdd_settings>
";

    #[test]
    fn patches_existing() {
        let x = patch_settings(SAMPLE, 2560, 1600, 120);
        assert!(x.contains("<count>1</count>"));
        assert!(!x.contains("<count>3</count>"));
        assert!(x.contains("<width>2560</width>"));
        assert!(x.contains("<refresh_rate>120</refresh_rate>"));
        assert!(!x.contains("<width>800</width>"));
        assert!(x.contains("<g_refresh_rate>120</g_refresh_rate>"));
        assert!(!x.contains("<g_refresh_rate>90</g_refresh_rate>"));
        assert!(x.contains("<friendlyname>default</friendlyname>"));
        // idempotent
        assert_eq!(patch_settings(&x, 2560, 1600, 120), x);
    }

    #[test]
    fn modes_keep_aspect() {
        assert_eq!(
            offered_modes(2000, 1200, 60),
            vec![(2000, 1200, 60), (1600, 960, 60), (1280, 768, 60), (1000, 600, 60)]
        );
        assert_eq!(
            offered_modes(2560, 1600, 120),
            vec![(2560, 1600, 120), (2048, 1280, 120), (1664, 1040, 120), (1280, 800, 120)]
        );
        // jedes Angebot im Seitenverhältnis des Tablets und gerade
        for (w, h) in [(2000, 1200), (2560, 1600), (2340, 1080), (1920, 1200), (2800, 1752)] {
            for (mw, mh, _) in offered_modes(w, h, 60) {
                assert_eq!(mw as u64 * h as u64, mh as u64 * w as u64, "{w}x{h} → {mw}x{mh}");
                assert!(mw % 2 == 0 && mh % 2 == 0 && mh >= 480);
            }
        }
        // ohne passende kleinere Größe: 1080p als Rückfall wie bisher
        assert_eq!(offered_modes(1366, 768, 60), vec![(1366, 768, 60), (1920, 1080, 60)]);
    }

    #[test]
    fn bounds() {
        assert!(mode_ok(2000, 1200, 60));
        assert!(!mode_ok(2000, 1200, 1000));
        assert!(!mode_ok(100_000, 1200, 60));
        assert!(!mode_ok(0, 0, 60));
    }

    #[test]
    fn inserts_missing_sections() {
        let x = patch_settings("<vdd_settings>\n</vdd_settings>\n", 1920, 1200, 60);
        assert!(x.contains("<resolutions>"));
        assert!(x.contains("<monitors>"));
        assert_eq!(x.matches("<g_refresh_rate>").count(), 1);
        assert!(x.trim_end().ends_with("</vdd_settings>"));
    }
}
