//! Windows-Benachrichtigungen im USBCD-Design.
//!
//! Statt der alten Info-Blasen (blaues „i“) zeigt USBCD echte Windows-11-
//! Benachrichtigungen: Absender „USBCD“ mit Logo, Titel, Text und – wo sinnvoll –
//! Schaltflächen (z. B. „Als Bildschirm verwenden“). Klicks kommen über das
//! Protokoll `usbcd:` zurück; jeder Link enthält ein zufälliges Kennwort dieser
//! Sitzung, damit fremde Webseiten oder Programme keine Aktionen auslösen können.
//! Klappt die Benachrichtigung nicht (z. B. abgeschaltet), erscheint eine Blase mit
//! USBCD-Logo.

use crate::util::wide;
use std::ffi::c_void;
use std::sync::{mpsc, Mutex, OnceLock};
use windows_sys::core::{GUID, HRESULT, PCWSTR};
use windows_sys::Win32::System::Registry::*;

pub const AUMID: &str = "Samuel.USBCD";

#[derive(Debug, Clone, Default)]
pub struct Toast {
    pub title: String,
    pub text: String,
    /// (Beschriftung, Aktion) – Aktion z. B. "device?act=use&serial=…"
    pub actions: Vec<(String, String)>,
    /// ohne Ton (Statusmeldungen); Fehler und Fragen mit Ton
    pub silent: bool,
    /// länger sichtbar (Fragen)
    pub long: bool,
}

static NONCE: OnceLock<String> = OnceLock::new();

/// Kennwort dieser USBCD-Sitzung für `usbcd:`-Links.
pub fn nonce() -> &'static str {
    NONCE.get_or_init(|| crate::auth::hex(&crate::auth::random::<16>()))
}

pub fn link(action: &str) -> String {
    let sep = if action.contains('?') { '&' } else { '?' };
    format!("usbcd:{action}{sep}n={}", nonce())
}

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if c.is_control() && c != '\n' => {}
            c => o.push(c),
        }
    }
    o
}

/// `file:///`-Adresse für das Logo (Leerzeichen etc. kodiert).
fn file_uri(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut o = String::from("file:///");
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/:._-".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn logo_path() -> std::path::PathBuf {
    crate::config::exe_dir().join("USBCD-logo.png")
}

/// Benachrichtigungs-XML (Windows „ToastGeneric“).
pub fn build_xml(t: &Toast, logo: Option<&str>) -> String {
    let mut x = format!(
        r#"<toast launch="{}" activationType="protocol"{}><visual><binding template="ToastGeneric"><text hint-maxLines="1">{}</text>"#,
        xml_escape(&link("none")),
        if t.long { r#" duration="long""# } else { "" },
        xml_escape(&t.title)
    );
    for line in t.text.split('\n').filter(|l| !l.trim().is_empty()).take(2) {
        x.push_str(&format!("<text>{}</text>", xml_escape(line)));
    }
    if let Some(l) = logo {
        x.push_str(&format!(
            r#"<image placement="appLogoOverride" src="{}"/>"#,
            xml_escape(l)
        ));
    }
    x.push_str("</binding></visual>");
    if !t.actions.is_empty() {
        x.push_str("<actions>");
        for (label, action) in t.actions.iter().take(3) {
            x.push_str(&format!(
                r#"<action content="{}" arguments="{}" activationType="protocol"/>"#,
                xml_escape(label),
                xml_escape(&link(action))
            ));
        }
        x.push_str("</actions>");
    }
    if t.silent {
        x.push_str(r#"<audio silent="true"/>"#);
    }
    x.push_str("</toast>");
    x
}

// ------------------------------------------------------------ Registrierung

fn set_reg(sub: &str, name: Option<&str>, value: &str) {
    let k = wide(sub);
    let n = name.map(wide);
    let v = wide(value);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            n.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            REG_SZ,
            v.as_ptr() as *const c_void,
            (v.len() * 2) as u32,
        );
    }
}

/// Absender „USBCD“ mit Logo und das Protokoll `usbcd:` für Klicks anmelden
/// (alles unter HKCU, ohne Administratorrechte; entfernt die Deinstallation).
pub fn register_app() {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe = exe
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    let base = format!(r"Software\Classes\AppUserModelId\{AUMID}");
    set_reg(&base, Some("DisplayName"), "USBCD");
    if logo_path().exists() {
        set_reg(&base, Some("IconUri"), &logo_path().to_string_lossy());
    }
    set_reg(&base, Some("IconBackgroundColor"), "FF1F1F1F");
    set_reg(r"Software\Classes\usbcd", None, "URL:USBCD");
    set_reg(r"Software\Classes\usbcd", Some("URL Protocol"), "");
    set_reg(
        r"Software\Classes\usbcd\DefaultIcon",
        None,
        &format!("\"{exe}\",0"),
    );
    set_reg(
        r"Software\Classes\usbcd\shell\open\command",
        None,
        &format!("\"{exe}\" --uri \"%1\""),
    );
    let id = wide(AUMID);
    unsafe {
        windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
    }
}

// ------------------------------------------------------------ WinRT (roh)

windows_targets::link!("combase.dll" "system" fn RoGetActivationFactory(activatableclassid : *mut c_void, iid : *const GUID, factory : *mut *mut c_void) -> HRESULT);
windows_targets::link!("combase.dll" "system" fn RoActivateInstance(activatableclassid : *mut c_void, instance : *mut *mut c_void) -> HRESULT);
windows_targets::link!("combase.dll" "system" fn WindowsCreateString(sourcestring : PCWSTR, length : u32, string : *mut *mut c_void) -> HRESULT);
windows_targets::link!("combase.dll" "system" fn WindowsDeleteString(string : *mut c_void) -> HRESULT);

const IID_TOAST_MANAGER_STATICS: GUID = GUID::from_u128(0x50ac103f_d235_4598_bbef_98fe4d1a3ad4);
const IID_TOAST_FACTORY: GUID = GUID::from_u128(0x04124b20_82c6_4229_b109_fd9ed4662b53);
const IID_XML_DOCUMENT: GUID = GUID::from_u128(0xf7f3a506_1e87_42d6_bcfb_b8c809fa5494);
const IID_XML_DOCUMENT_IO: GUID = GUID::from_u128(0x6cd0e74e_ee65_4489_9ebf_ca43e87ba637);

struct HStr(*mut c_void);
impl HStr {
    fn new(s: &str) -> Option<HStr> {
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut h = std::ptr::null_mut();
        (unsafe { WindowsCreateString(w.as_ptr(), w.len() as u32, &mut h) } >= 0).then_some(HStr(h))
    }
}
impl Drop for HStr {
    fn drop(&mut self) {
        unsafe { WindowsDeleteString(self.0) };
    }
}

/// COM-/WinRT-Zeiger mit Release beim Aufräumen.
struct Obj(*mut c_void);
impl Obj {
    unsafe fn f<F: Copy>(&self, idx: usize) -> F {
        let vtbl = *(self.0 as *const *const usize);
        let p = *vtbl.add(idx);
        std::mem::transmute_copy::<usize, F>(&p)
    }
    unsafe fn query(&self, iid: &GUID) -> Option<Obj> {
        let qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT =
            self.f(0);
        let mut p = std::ptr::null_mut();
        (qi(self.0, iid, &mut p) >= 0 && !p.is_null()).then_some(Obj(p))
    }
}
impl Drop for Obj {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let release: unsafe extern "system" fn(*mut c_void) -> u32 = self.f(2);
                release(self.0);
            }
        }
    }
}

type InOut = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;
type In = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;

unsafe fn factory(class: &str, iid: &GUID) -> Option<Obj> {
    let name = HStr::new(class)?;
    let mut p = std::ptr::null_mut();
    (RoGetActivationFactory(name.0, iid, &mut p) >= 0 && !p.is_null()).then_some(Obj(p))
}

/// IToastNotifier für USBCD.
unsafe fn notifier() -> Option<Obj> {
    let statics = factory(
        "Windows.UI.Notifications.ToastNotificationManager",
        &IID_TOAST_MANAGER_STATICS,
    )?;
    let id = HStr::new(AUMID)?;
    let create: InOut = statics.f(7); // CreateToastNotifierWithId
    let mut n = std::ptr::null_mut();
    (create(statics.0, id.0, &mut n) >= 0 && !n.is_null()).then_some(Obj(n))
}

unsafe fn show_winrt(notifier: &Obj, xml: &str) -> bool {
    let Some(class) = HStr::new("Windows.Data.Xml.Dom.XmlDocument") else {
        return false;
    };
    let mut inst = std::ptr::null_mut();
    if RoActivateInstance(class.0, &mut inst) < 0 || inst.is_null() {
        return false;
    }
    let inst = Obj(inst);
    let (Some(io), Some(doc)) = (
        inst.query(&IID_XML_DOCUMENT_IO),
        inst.query(&IID_XML_DOCUMENT),
    ) else {
        return false;
    };
    let Some(text) = HStr::new(xml) else {
        return false;
    };
    let load: In = io.f(6); // LoadXml
    if load(io.0, text.0) < 0 {
        crate::log!("Benachrichtigung: XML abgelehnt");
        return false;
    }
    let Some(fac) = factory(
        "Windows.UI.Notifications.ToastNotification",
        &IID_TOAST_FACTORY,
    ) else {
        return false;
    };
    let create: InOut = fac.f(6); // CreateToastNotification
    let mut toast = std::ptr::null_mut();
    if create(fac.0, doc.0, &mut toast) < 0 || toast.is_null() {
        return false;
    }
    let toast = Obj(toast);
    let show: In = notifier.f(6); // Show
    show(notifier.0, toast.0) >= 0
}

// ------------------------------------------------------------ Versand

static TX: OnceLock<Mutex<mpsc::Sender<Toast>>> = OnceLock::new();

/// Startet den Benachrichtigungs-Thread (einmal beim Programmstart).
pub fn init() {
    register_app();
    let (tx, rx) = mpsc::channel::<Toast>();
    let _ = TX.set(Mutex::new(tx));
    std::thread::spawn(move || unsafe {
        windows_sys::Win32::System::Com::CoInitializeEx(
            std::ptr::null(),
            windows_sys::Win32::System::Com::COINIT_MULTITHREADED as u32,
        );
        let n = notifier();
        if n.is_none() {
            crate::log!("Windows-Benachrichtigungen nicht verfügbar – nutze Info-Blasen");
        }
        let logo = logo_path().exists().then(|| file_uri(&logo_path()));
        let mut warned = false;
        for t in rx {
            let ok = n
                .as_ref()
                .is_some_and(|n| show_winrt(n, &build_xml(&t, logo.as_deref())));
            if !ok {
                if !warned && n.is_some() {
                    crate::log!("Benachrichtigung nicht anzeigbar – nutze Info-Blase");
                    warned = true;
                }
                crate::tray::balloon_fallback(&t.title, &t.text);
            }
        }
    });
}

/// Zeigt eine Benachrichtigung (aus beliebigem Thread).
pub fn show(t: Toast) {
    match TX.get().and_then(|m| m.lock().ok()) {
        Some(tx) => {
            let _ = tx.send(t);
        }
        None => crate::tray::balloon_fallback(&t.title, &t.text),
    }
}

// ------------------------------------------------------------ Klicks

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    OpenLog,
    /// wie Windows-Taste + P → „Erweitern“
    ExtendDisplays,
    UseDevice(String),
    NeverDevice(String),
    /// Update auf diese Version installieren
    UpdateInstall(String),
    /// diese Version nicht mehr per Benachrichtigung anbieten
    UpdateSkip(String),
}

/// Erlaubte Zeichen einer adb-Seriennummer.
pub fn valid_serial(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Liest einen `usbcd:`-Link. Ohne gültiges Kennwort dieser Sitzung: abgelehnt.
pub fn parse_uri(uri: &str, expected_nonce: &str) -> Option<Action> {
    let rest = uri.strip_prefix("usbcd:")?;
    let (verb, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut n = None;
    let mut act = None;
    let mut serial = None;
    let mut version = None;
    for kv in query.split('&') {
        match kv.split_once('=') {
            Some(("n", v)) => n = Some(v),
            Some(("act", v)) => act = Some(v),
            Some(("serial", v)) => serial = Some(v),
            Some(("v", v)) => version = Some(v),
            _ => {}
        }
    }
    if n != Some(expected_nonce) || expected_nonce.is_empty() {
        return None;
    }
    match (verb, act, serial) {
        ("none", _, _) => Some(Action::None),
        ("log", _, _) => Some(Action::OpenLog),
        ("extend", _, _) => Some(Action::ExtendDisplays),
        ("device", Some("use"), Some(s)) if valid_serial(s) => Some(Action::UseDevice(s.into())),
        ("device", Some("never"), Some(s)) if valid_serial(s) => {
            Some(Action::NeverDevice(s.into()))
        }
        ("update", Some(a), _) => match (a, version) {
            ("install", Some(v)) if crate::update::valid_version(v) => {
                Some(Action::UpdateInstall(v.into()))
            }
            ("skip", Some(v)) if crate::update::valid_version(v) => {
                Some(Action::UpdateSkip(v.into()))
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_escaped() {
        let t = Toast {
            title: "Neues Gerät".into(),
            text: "Tab <S6> & \"Lite\"\nZweite Zeile".into(),
            actions: vec![("Verwenden".into(), "device?act=use&serial=R5".into())],
            silent: true,
            long: true,
        };
        let x = build_xml(&t, Some("file:///C:/x.png"));
        assert!(x.contains("Tab &lt;S6&gt; &amp; &quot;Lite&quot;"));
        assert!(x.contains("<text>Zweite Zeile</text>"));
        assert!(x.contains(r#"duration="long""#));
        assert!(x.contains(r#"<audio silent="true"/>"#));
        assert!(x.contains("usbcd:device?act=use&amp;serial=R5&amp;n="));
        assert!(!x.contains("<S6>"));
    }

    #[test]
    fn uri_needs_nonce() {
        assert_eq!(
            parse_uri("usbcd:device?act=use&serial=ABC123&n=abc", "abc"),
            Some(Action::UseDevice("ABC123".into()))
        );
        assert_eq!(
            parse_uri("usbcd:device?act=never&serial=ABC123&n=abc", "abc"),
            Some(Action::NeverDevice("ABC123".into()))
        );
        assert_eq!(
            parse_uri("usbcd:device?act=use&serial=ABC123&n=x", "abc"),
            None
        );
        assert_eq!(parse_uri("usbcd:device?act=use&serial=ABC123", "abc"), None);
        assert_eq!(
            parse_uri("usbcd:device?act=use&serial=a;b&n=abc", "abc"),
            None
        );
        assert_eq!(parse_uri("usbcd:format?n=abc", "abc"), None);
        // „Bildschirme erweitern“ nur mit gültigem Kennwort
        assert_eq!(parse_uri("usbcd:extend?n=abc", "abc"), Some(Action::ExtendDisplays));
        assert_eq!(parse_uri("usbcd:extend?n=x", "abc"), None);
        assert_eq!(parse_uri("http:x", "abc"), None);
        assert_eq!(
            parse_uri("usbcd:update?act=install&v=0.6.15&n=abc", "abc"),
            Some(Action::UpdateInstall("0.6.15".into()))
        );
        assert_eq!(
            parse_uri("usbcd:update?act=skip&v=0.6.15&n=abc", "abc"),
            Some(Action::UpdateSkip("0.6.15".into()))
        );
        assert_eq!(parse_uri("usbcd:update?act=install&v=0.6.15&n=x", "abc"), None);
        assert_eq!(parse_uri("usbcd:update?act=install&v=..%2f&n=abc", "abc"), None);
        assert_eq!(parse_uri("usbcd:update?act=run&v=0.6.15&n=abc", "abc"), None);
        assert_eq!(parse_uri("usbcd:none?n=", ""), None);
    }

    #[test]
    fn file_uri_encodes() {
        assert_eq!(
            file_uri(std::path::Path::new(
                r"C:\Program Files\USBCD\USBCD-logo.png"
            )),
            "file:///C:/Program%20Files/USBCD/USBCD-logo.png"
        );
    }
}
