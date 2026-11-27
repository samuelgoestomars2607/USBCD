//! Windows-Anzeige-APIs (GDI): virtuellen Monitor finden, Auflösung/Hz setzen,
//! HMONITOR für die Bildschirmaufnahme ermitteln.

use crate::win::from_wide;
use crate::win::wide;
use anyhow::{bail, Result};
use std::mem::{size_of, zeroed};
use std::ptr::null;
use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplayMonitors, EnumDisplaySettingsW,
    GetMonitorInfoW, CDS_NORESET, CDS_UPDATEREGISTRY, DEVMODEW, DISPLAYCONFIG_PATH_ACTIVE,
    DISPLAYCONFIG_PATH_MODE_IDX_INVALID, DISPLAY_DEVICEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP,
    DISPLAY_DEVICE_PRIMARY_DEVICE, DISP_CHANGE_SUCCESSFUL, DM_BITSPERPEL, DM_DISPLAYFREQUENCY,
    DM_PELSHEIGHT, DM_PELSWIDTH, DM_POSITION, ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW,
};

#[derive(Debug, Clone)]
pub struct VirtualDisplay {
    /// z. B. "\\.\DISPLAY5"
    pub device_name: String,
    pub description: String,
    pub attached: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Unknown,
}

fn enum_adapter(i: u32) -> Option<DISPLAY_DEVICEW> {
    unsafe {
        let mut d: DISPLAY_DEVICEW = zeroed();
        d.cb = size_of::<DISPLAY_DEVICEW>() as u32;
        if EnumDisplayDevicesW(null(), i, &mut d, 0) == 0 {
            None
        } else {
            Some(d)
        }
    }
}

fn monitors_of(adapter: &[u16]) -> Vec<DISPLAY_DEVICEW> {
    let mut v = Vec::new();
    let name = wide(&from_wide(adapter));
    for j in 0.. {
        unsafe {
            let mut m: DISPLAY_DEVICEW = zeroed();
            m.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            if EnumDisplayDevicesW(name.as_ptr(), j, &mut m, 0) == 0 {
                break;
            }
            v.push(m);
        }
    }
    v
}

/// Sucht den Monitor des Virtual Display Driver anhand der Textmuster.
pub fn find_virtual_display(patterns: &[String]) -> Option<VirtualDisplay> {
    let pats: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();
    let hit = |s: &str| {
        let s = s.to_lowercase();
        pats.iter().any(|p| !p.is_empty() && s.contains(p.as_str()))
    };
    for i in 0..64 {
        let Some(a) = enum_adapter(i) else { break };
        let desc = from_wide(&a.DeviceString);
        // andere virtuelle Monitore (Parsec, Steam, …) nie anfassen
        let dl = desc.to_lowercase();
        if dl.contains("parsec") || dl.contains("steam") || dl.contains("spacedesk") {
            continue;
        }
        let mut matched = hit(&desc) || hit(&from_wide(&a.DeviceID));
        if !matched {
            for m in monitors_of(&a.DeviceName) {
                if hit(&from_wide(&m.DeviceString)) || hit(&from_wide(&m.DeviceID)) {
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            return Some(VirtualDisplay {
                device_name: from_wide(&a.DeviceName),
                description: desc,
                attached: a.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0,
            });
        }
    }
    None
}

pub fn primary_gpu_vendor() -> GpuVendor {
    for i in 0..64 {
        let Some(a) = enum_adapter(i) else { break };
        if a.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE == 0 {
            continue;
        }
        let s = from_wide(&a.DeviceString).to_lowercase();
        return if s.contains("nvidia") {
            GpuVendor::Nvidia
        } else if s.contains("amd") || s.contains("radeon") {
            GpuVendor::Amd
        } else if s.contains("intel") {
            GpuVendor::Intel
        } else {
            GpuVendor::Unknown
        };
    }
    GpuVendor::Unknown
}

impl GpuVendor {
    /// PCI-Herstellerkennung → Hersteller.
    pub fn from_pci(id: u32) -> GpuVendor {
        match id {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            _ => GpuVendor::Unknown,
        }
    }
}

/// Grafikchip, der einen Monitor zeichnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderGpu {
    /// PCI-Herstellerkennung (0x10DE Nvidia, 0x1002 AMD, 0x8086 Intel)
    pub vendor_id: u32,
    pub name: String,
}

/// Welcher Grafikchip zeichnet den Monitor `device` (z. B. "\\.\DISPLAY8")?
///
/// DXGI führt den virtuellen Monitor unter dem Grafikchip, der ihn zeichnet. Auf Laptops
/// mit zwei Grafikchips ist das nicht immer der, an dem der Hauptbildschirm hängt –
/// Aufnahme und Kodierung sollen dann auf diesem Chip laufen, damit Windows nicht jedes
/// Bild zwischen den Chips kopieren muss.
#[cfg(windows)]
pub fn render_gpu(device: &str) -> Option<RenderGpu> {
    use std::ffi::c_void;
    use windows_sys::core::{GUID, HRESULT};

    #[repr(C)]
    struct AdapterDesc {
        description: [u16; 128],
        vendor_id: u32,
        device_id: u32,
        sub_sys_id: u32,
        revision: u32,
        dedicated_video_memory: usize,
        dedicated_system_memory: usize,
        shared_system_memory: usize,
        luid: windows_sys::Win32::Foundation::LUID,
    }
    #[repr(C)]
    struct OutputDesc {
        device_name: [u16; 32],
        desktop: RECT,
        attached: BOOL,
        rotation: u32,
        monitor: *mut c_void,
    }
    #[link(name = "dxgi")]
    extern "system" {
        fn CreateDXGIFactory1(riid: *const GUID, factory: *mut *mut c_void) -> HRESULT;
    }
    const IID_IDXGI_FACTORY1: GUID = GUID::from_u128(0x770aae78_f26f_4dba_a829_253c83d1b387);

    // COM-Zeiger mit Release beim Verlassen (win.rs)
    use crate::win::ComPtr as Com;

    unsafe {
        let mut f: *mut c_void = std::ptr::null_mut();
        if CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut f) < 0 || f.is_null() {
            return None;
        }
        let factory = Com(f);
        // IDXGIFactory::EnumAdapters (7), IDXGIAdapter::EnumOutputs (7) / GetDesc (8),
        // IDXGIOutput::GetDesc (7)
        let enum_adapters: unsafe extern "system" fn(
            *mut c_void,
            u32,
            *mut *mut c_void,
        ) -> HRESULT = factory.f(7);
        for ai in 0..16 {
            let mut a: *mut c_void = std::ptr::null_mut();
            if enum_adapters(factory.0, ai, &mut a) < 0 || a.is_null() {
                break;
            }
            let adapter = Com(a);
            let enum_outputs: unsafe extern "system" fn(
                *mut c_void,
                u32,
                *mut *mut c_void,
            ) -> HRESULT = adapter.f(7);
            for oi in 0..16 {
                let mut o: *mut c_void = std::ptr::null_mut();
                if enum_outputs(adapter.0, oi, &mut o) < 0 || o.is_null() {
                    break;
                }
                let output = Com(o);
                let get_desc: unsafe extern "system" fn(*mut c_void, *mut OutputDesc) -> HRESULT =
                    output.f(7);
                let mut od: OutputDesc = zeroed();
                if get_desc(output.0, &mut od) < 0
                    || !from_wide(&od.device_name).eq_ignore_ascii_case(device)
                {
                    continue;
                }
                let adapter_desc: unsafe extern "system" fn(
                    *mut c_void,
                    *mut AdapterDesc,
                ) -> HRESULT = adapter.f(8);
                let mut ad: AdapterDesc = zeroed();
                if adapter_desc(adapter.0, &mut ad) < 0 {
                    return None;
                }
                // Microsoft Basic Render Driver (0x1414): kein echter Grafikchip
                if ad.vendor_id == 0x1414 {
                    return None;
                }
                return Some(RenderGpu {
                    vendor_id: ad.vendor_id,
                    name: from_wide(&ad.description),
                });
            }
        }
        None
    }
}

#[cfg(not(windows))]
pub fn render_gpu(_device: &str) -> Option<RenderGpu> {
    None
}

fn current_mode(device: &str) -> Option<DEVMODEW> {
    unsafe {
        let name = wide(device);
        let mut dm: DEVMODEW = zeroed();
        dm.dmSize = size_of::<DEVMODEW>() as u16;
        if EnumDisplaySettingsW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) == 0 {
            None
        } else {
            Some(dm)
        }
    }
}

/// Liefert (Breite, Höhe, Hz) des aktuellen Modus.
pub fn mode_of(device: &str) -> Option<(u32, u32, u32)> {
    current_mode(device).map(|m| (m.dmPelsWidth, m.dmPelsHeight, m.dmDisplayFrequency))
}

/// Lage eines Monitors im Desktop (Pixel, Hauptmonitor oben links = 0,0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Placement {
    fn right(&self) -> i32 {
        self.x + self.width as i32
    }
    fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }
    fn overlaps(&self, o: &Placement) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    /// Berührt eine Kante von `o` (Monitore müssen in Windows lückenlos aneinander liegen).
    fn touches(&self, o: &Placement) -> bool {
        let vert = (self.right() == o.x || o.right() == self.x)
            && self.y < o.bottom()
            && o.y < self.bottom();
        let horiz = (self.bottom() == o.y || o.bottom() == self.y)
            && self.x < o.right()
            && o.x < self.right();
        vert || horiz
    }
}

/// Position eines angehängten Monitors laut Windows (physische Pixel, unabhängig
/// von der Anzeigeskalierung – im Gegensatz zu GetMonitorInfo ohne DPI-Bewusstsein).
fn placement_of(device: &str) -> Option<Placement> {
    let m = current_mode(device)?;
    if m.dmPelsWidth == 0 || m.dmPelsHeight == 0 {
        return None;
    }
    let p = unsafe { m.Anonymous1.Anonymous2.dmPosition };
    Some(Placement {
        x: p.x,
        y: p.y,
        width: m.dmPelsWidth,
        height: m.dmPelsHeight,
    })
}

/// Alle anderen angehängten Monitore.
fn other_displays(except: &str) -> Vec<Placement> {
    let mut v = Vec::new();
    for i in 0..64 {
        let Some(a) = enum_adapter(i) else { break };
        let name = from_wide(&a.DeviceName);
        if a.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0
            || name.eq_ignore_ascii_case(except)
        {
            continue;
        }
        if let Some(p) = placement_of(&name) {
            v.push(p);
        }
    }
    v
}

/// Wo der Tablet-Monitor hin soll: an die gemerkte Stelle, sofern sie zum jetzigen
/// Aufbau passt; sonst `None` (dann entscheidet Windows bzw. „rechts daneben“).
pub fn wanted_position(
    saved: Option<Placement>,
    w: u32,
    h: u32,
    others: &[Placement],
) -> Option<(i32, i32)> {
    let p = saved?;
    // Links bzw. oberhalb des Hauptmonitors: an der Kante zum Hauptmonitor ausrichten,
    // damit eine andere Auflösung keine Lücke oder Überlappung erzeugt
    let x = if p.x < 0 { p.right() - w as i32 } else { p.x };
    let y = if p.y < 0 { p.bottom() - h as i32 } else { p.y };
    let me = Placement {
        x,
        y,
        width: w,
        height: h,
    };
    if others.is_empty()
        || others.iter().any(|o| me.overlaps(o))
        || !others.iter().any(|o| me.touches(o))
    {
        return None;
    }
    Some((x, y))
}

fn right_edge(others: &[Placement]) -> i32 {
    others.iter().map(Placement::right).max().unwrap_or(0)
}

/// Linke obere Ecke des gesamten Desktops über alle Monitore (kann negativ sein,
/// wenn ein Monitor links bzw. über dem Hauptbildschirm liegt).
pub fn virtual_origin() -> (i32, i32) {
    let all = other_displays("");
    let x = all.iter().map(|p| p.x).min().unwrap_or(0);
    let y = all.iter().map(|p| p.y).min().unwrap_or(0);
    (x.min(0), y.min(0))
}

/// Aktuelle Lage des Tablet-Monitors (zum Merken).
pub fn current_placement(d: &VirtualDisplay) -> Option<Placement> {
    find_virtual_by_name(&d.device_name)
        .filter(|a| a.attached)
        .and_then(|_| placement_of(&d.device_name))
}

fn find_virtual_by_name(device: &str) -> Option<VirtualDisplay> {
    for i in 0..64 {
        let Some(a) = enum_adapter(i) else { break };
        let name = from_wide(&a.DeviceName);
        if name.eq_ignore_ascii_case(device) {
            return Some(VirtualDisplay {
                device_name: name,
                description: from_wide(&a.DeviceString),
                attached: a.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0,
            });
        }
    }
    None
}

/// Wartet, bis Windows den Monitor selbst an den Desktop gehängt hat (dabei stellt
/// Windows die gespeicherte Anordnung wieder her). Liefert den neuesten Zustand.
pub fn wait_attached(d: &VirtualDisplay, timeout: std::time::Duration) -> VirtualDisplay {
    let end = std::time::Instant::now() + timeout;
    loop {
        let cur = find_virtual_by_name(&d.device_name).unwrap_or_else(|| d.clone());
        if cur.attached || std::time::Instant::now() > end {
            return cur;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Setzt Auflösung + Bildrate dauerhaft und legt den Monitor an die gemerkte Stelle.
/// Ohne gemerkte Stelle bleibt die Position, die Windows gewählt hat; ist der Monitor
/// gar nicht angehängt, kommt er rechts neben die übrigen.
pub fn set_mode(
    d: &VirtualDisplay,
    w: u32,
    h: u32,
    hz: u32,
    saved: Option<Placement>,
) -> Result<()> {
    let others = other_displays(&d.device_name);
    let current = if d.attached {
        placement_of(&d.device_name)
    } else {
        None
    };
    let pos = wanted_position(saved, w, h, &others).or(if d.attached {
        None // Windows' eigene Anordnung behalten
    } else {
        Some((right_edge(&others), 0))
    });
    let mode_ok = d.attached && mode_of(&d.device_name) == Some((w, h, hz));
    let pos_ok = pos.is_none() || current.map(|c| (c.x, c.y)) == pos;
    if mode_ok && pos_ok {
        return Ok(());
    }
    unsafe {
        let name = wide(&d.device_name);
        let mut dm: DEVMODEW = zeroed();
        dm.dmSize = size_of::<DEVMODEW>() as u16;
        dm.dmPelsWidth = w;
        dm.dmPelsHeight = h;
        dm.dmDisplayFrequency = hz;
        dm.dmBitsPerPel = 32;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY | DM_BITSPERPEL;
        if let Some((x, y)) = pos {
            dm.Anonymous1.Anonymous2.dmPosition.x = x;
            dm.Anonymous1.Anonymous2.dmPosition.y = y;
            dm.dmFields |= DM_POSITION;
        }
        let r = ChangeDisplaySettingsExW(
            name.as_ptr(),
            &dm,
            std::ptr::null_mut(),
            CDS_UPDATEREGISTRY | CDS_NORESET,
            null(),
        );
        if r != DISP_CHANGE_SUCCESSFUL {
            bail!(
                "ChangeDisplaySettingsEx({}, {w}x{h}@{hz}) = {r}",
                d.device_name
            );
        }
        // Änderungen anwenden
        ChangeDisplaySettingsExW(null(), null(), std::ptr::null_mut(), 0, null());
    }
    if let Some((x, y)) = pos.filter(|_| !pos_ok) {
        crate::log!("Tablet-Monitor an Position {x},{y} gelegt");
    }
    Ok(())
}

unsafe extern "system" fn monitor_cb(hm: HMONITOR, _hdc: HDC, _r: *mut RECT, lp: LPARAM) -> BOOL {
    let v = &mut *(lp as *mut Vec<(HMONITOR, String, RECT)>);
    let mut mi: MONITORINFOEXW = zeroed();
    mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(hm, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
        v.push((hm, from_wide(&mi.szDevice), mi.monitorInfo.rcMonitor));
    }
    1
}

fn enum_monitors() -> Vec<(HMONITOR, String, RECT)> {
    let mut v: Vec<(HMONITOR, String, RECT)> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            null(),
            Some(monitor_cb),
            &mut v as *mut _ as LPARAM,
        );
    }
    v
}

/// HMONITOR (als Zahl, für ffmpeg gfxcapture=hmonitor=…) des Geräts.
pub fn hmonitor_for(device_name: &str) -> Option<u64> {
    enum_monitors()
        .into_iter()
        .find(|(_, n, _)| n.eq_ignore_ascii_case(device_name))
        .map(|(h, _, _)| h as usize as u64)
}

// --- Tablet-Monitor als erweiterten Bildschirm anhängen ----------------------------------
//
// Laptops spiegeln einen neu angeschlossenen Bildschirm oft zuerst („Duplizieren“) oder
// nutzen nur den eigenen („Nur PC-Bildschirm“). Dann ist der Tablet-Monitor nicht Teil
// des Desktops. `attach_extended` ändert gezielt nur den Pfad des Tablet-Monitors (CCD-API,
// wie die Windows-Anzeigeeinstellungen) – alle anderen Bildschirme bleiben, wie sie sind.

use windows_sys::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, SetDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE,
    SDC_TOPOLOGY_EXTEND, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
};

const PATH_ACTIVE: u32 = DISPLAYCONFIG_PATH_ACTIVE;
const MODE_IDX_INVALID: u32 = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;

fn query_config(
    flags: u32,
) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    // Die Anzahl kann sich zwischen den beiden Aufrufen ändern → ein paar Versuche
    for _ in 0..5 {
        unsafe {
            let (mut np, mut nm) = (0u32, 0u32);
            let r = GetDisplayConfigBufferSizes(flags, &mut np, &mut nm);
            if r != 0 {
                bail!("GetDisplayConfigBufferSizes = {r}");
            }
            let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![zeroed(); np as usize];
            let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![zeroed(); nm as usize];
            let r = QueryDisplayConfig(
                flags,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            );
            if r == 122 {
                continue; // ERROR_INSUFFICIENT_BUFFER
            }
            if r != 0 {
                bail!("QueryDisplayConfig = {r}");
            }
            paths.truncate(np as usize);
            modes.truncate(nm as usize);
            return Ok((paths, modes));
        }
    }
    bail!("QueryDisplayConfig: Anzeigeaufbau ändert sich ständig")
}

/// GDI-Name der Quelle eines Pfads, z. B. "\\.\DISPLAY8".
fn source_name(p: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    unsafe {
        let mut n: DISPLAYCONFIG_SOURCE_DEVICE_NAME = zeroed();
        n.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: p.sourceInfo.adapterId,
            id: p.sourceInfo.id,
        };
        if DisplayConfigGetDeviceInfo(&mut n.header) != 0 {
            return None;
        }
        Some(from_wide(&n.viewGdiDeviceName))
    }
}

fn same_luid(
    a: &windows_sys::Win32::Foundation::LUID,
    b: &windows_sys::Win32::Foundation::LUID,
) -> bool {
    a.LowPart == b.LowPart && a.HighPart == b.HighPart
}

fn same_target(a: &DISPLAYCONFIG_PATH_INFO, b: &DISPLAYCONFIG_PATH_INFO) -> bool {
    same_luid(&a.targetInfo.adapterId, &b.targetInfo.adapterId)
        && a.targetInfo.id == b.targetInfo.id
}

fn same_source(a: &DISPLAYCONFIG_PATH_INFO, b: &DISPLAYCONFIG_PATH_INFO) -> bool {
    same_luid(&a.sourceInfo.adapterId, &b.sourceInfo.adapterId)
        && a.sourceInfo.id == b.sourceInfo.id
}

/// Hängt den Tablet-Monitor als eigenen (erweiterten) Bildschirm an. Wird er gerade
/// gespiegelt, endet nur seine Spiegelung; andere Bildschirme bleiben unverändert.
pub fn attach_extended(d: &VirtualDisplay) -> Result<()> {
    let (all, _) = query_config(QDC_ALL_PATHS)?;
    let (active, modes) = query_config(QDC_ONLY_ACTIVE_PATHS)?;

    // Pfade von der Quelle des Tablet-Monitors zu einem Ausgang desselben (virtuellen) Adapters
    let ours: Vec<&DISPLAYCONFIG_PATH_INFO> = all
        .iter()
        .filter(|p| {
            p.targetInfo.targetAvailable != 0
                && same_luid(&p.sourceInfo.adapterId, &p.targetInfo.adapterId)
                && source_name(p).is_some_and(|n| n.eq_ignore_ascii_case(&d.device_name))
        })
        .collect();
    if ours.is_empty() {
        bail!("kein Anzeigepfad für {} gefunden", d.device_name);
    }
    if active
        .iter()
        .any(|a| ours.iter().any(|p| same_source(a, p)))
    {
        return Ok(()); // schon angehängt
    }
    // Ausgang wählen: bevorzugt den, den Windows gerade spiegelt, sonst einen freien
    let pick = ours
        .iter()
        .find(|p| active.iter().any(|a| same_target(a, p)))
        .or_else(|| ours.first())
        .copied()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("kein Ausgang"))?;

    // Alle aktiven Pfade behalten – außer der Spiegelung auf diesen Ausgang
    let kept: Vec<DISPLAYCONFIG_PATH_INFO> = active
        .iter()
        .filter(|a| !same_target(a, &pick))
        .copied()
        .collect();
    let (mut paths, new_modes) = remap_modes(&kept, &modes);
    let mut p = pick;
    p.flags = PATH_ACTIVE;
    p.sourceInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
    p.targetInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
    paths.push(p);

    let r = unsafe {
        SetDisplayConfig(
            paths.len() as u32,
            paths.as_ptr(),
            new_modes.len() as u32,
            new_modes.as_ptr(),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE,
        )
    };
    if r != 0 {
        bail!("SetDisplayConfig = {r}");
    }
    Ok(())
}

/// Nur die Modi behalten, auf die die Pfade verweisen, und die Verweise neu nummerieren.
fn remap_modes(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    modes: &[DISPLAYCONFIG_MODE_INFO],
) -> (Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>) {
    let mut out_modes: Vec<DISPLAYCONFIG_MODE_INFO> = Vec::new();
    let mut map: Vec<(u32, u32)> = Vec::new();
    let mut take = |idx: u32| -> u32 {
        if idx == MODE_IDX_INVALID || idx as usize >= modes.len() {
            return MODE_IDX_INVALID;
        }
        if let Some((_, n)) = map.iter().find(|(o, _)| *o == idx) {
            return *n;
        }
        let n = out_modes.len() as u32;
        out_modes.push(modes[idx as usize]);
        map.push((idx, n));
        n
    };
    let out_paths = paths
        .iter()
        .map(|p| {
            let mut p = *p;
            unsafe {
                p.sourceInfo.Anonymous.modeInfoIdx = take(p.sourceInfo.Anonymous.modeInfoIdx);
                p.targetInfo.Anonymous.modeInfoIdx = take(p.targetInfo.Anonymous.modeInfoIdx);
            }
            p
        })
        .collect();
    (out_paths, out_modes)
}

/// Wie Windows-Taste + P → „Erweitern“ (nur auf ausdrücklichen Klick des Nutzers).
pub fn extend_all() -> Result<()> {
    let r = unsafe {
        SetDisplayConfig(
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            SDC_APPLY | SDC_TOPOLOGY_EXTEND,
        )
    };
    if r != 0 {
        bail!("SetDisplayConfig(Erweitern) = {r}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Placement = Placement {
        x: 0,
        y: 0,
        width: 2560,
        height: 1440,
    };

    #[test]
    fn keeps_left_placement() {
        let saved = Placement {
            x: -2000,
            y: 240,
            width: 2000,
            height: 1200,
        };
        assert_eq!(
            wanted_position(Some(saved), 2000, 1200, &[MAIN]),
            Some((-2000, 240))
        );
        // andere Auflösung: bleibt bündig links am Hauptmonitor
        assert_eq!(
            wanted_position(Some(saved), 1920, 1200, &[MAIN]),
            Some((-1920, 240))
        );
    }

    #[test]
    fn rejects_unfitting_saved_position() {
        let overlapping = Placement {
            x: 100,
            y: 0,
            width: 2000,
            height: 1200,
        };
        assert_eq!(
            wanted_position(Some(overlapping), 2000, 1200, &[MAIN]),
            None
        );
        let floating = Placement {
            x: 9000,
            y: 0,
            width: 2000,
            height: 1200,
        };
        assert_eq!(wanted_position(Some(floating), 2000, 1200, &[MAIN]), None);
        assert_eq!(wanted_position(None, 2000, 1200, &[MAIN]), None);
    }

    #[test]
    fn right_of_main() {
        let saved = Placement {
            x: 2560,
            y: 0,
            width: 2000,
            height: 1200,
        };
        assert_eq!(
            wanted_position(Some(saved), 2000, 1200, &[MAIN]),
            Some((2560, 0))
        );
        assert_eq!(right_edge(&[MAIN]), 2560);
    }
}
