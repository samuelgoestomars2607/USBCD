//! USBCD-Hilfsdienst „USBCDHelper“ (läuft als LocalSystem) und der Zugang dazu.
//!
//! USBCD selbst läuft ohne Administratorrechte. Die wenigen Dinge, die Rechte
//! brauchen, erledigt dieser Dienst – und zwar NUR diese fest eingebauten Befehle
//! mit geprüften Werten (keine Pfade, keine Programmaufrufe, keine Skripte):
//!
//!   ping
//!   vdd-on <breite> <höhe> <hz>     virtuellen Monitor mit diesem Modus einschalten
//!   vdd-off                         virtuellen Monitor ausschalten
//!   speaker-prepare <0|1> [name]    Tablet-Lautsprecher benennen, Mehrkanal-Variante
//!                                   und ggf. „CABLE Output“ ausblenden
//!
//! Die Leitung `\\.\pipe\USBCD-Helper` ist nur lokal erreichbar; der Dienst nimmt nur
//! Aufträge von der installierten USBCD.exe an, und USBCD prüft umgekehrt, dass am
//! anderen Ende wirklich der Dienst sitzt.

use crate::util::wide;
use anyhow::{bail, Context, Result};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FILE_NOT_FOUND,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SERVICE_ALREADY_RUNNING, HANDLE,
    INVALID_HANDLE_VALUE, NO_ERROR,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    QueryServiceStatusEx, RegisterServiceCtrlHandlerExW, SetServiceStatus,
    StartServiceCtrlDispatcherW, StartServiceW, SC_ACTION, SC_ACTION_RESTART,
    SC_MANAGER_ALL_ACCESS, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN,
    SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONSW,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
    SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STATUS_PROCESS, SERVICE_STOPPED,
    SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub const SERVICE_NAME: &str = "USBCDHelper";
const DISPLAY_NAME: &str = "USBCD Helper";
const PIPE_NAME: &str = r"\\.\pipe\USBCD-Helper";
/// SYSTEM und Administratoren voll, angemeldete (interaktive) Nutzer lesen/schreiben,
/// Netzwerkzugriff ausdrücklich verboten.
const PIPE_SDDL: &str = "D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";
const MAX_LINE: usize = 512;

// ---------------------------------------------------------------- Befehle

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Ping,
    VddOn { w: u32, h: u32, hz: u32 },
    VddOff,
    Speaker { hide_output: bool, name: String },
}

fn valid_name(n: &str) -> bool {
    let len = n.chars().count();
    (1..=40).contains(&len)
        && n.chars()
            .all(|c| c.is_alphanumeric() || " -_().".contains(c))
        && !n.starts_with(' ')
}

pub fn parse(line: &str) -> Result<Cmd, String> {
    if line.len() > MAX_LINE {
        return Err("zu lang".into());
    }
    let mut it = line.trim_end_matches(['\r', '\n']).splitn(2, ' ');
    let verb = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("");
    let num = |s: Option<&str>| -> Result<u32, String> {
        s.filter(|s| !s.is_empty() && s.len() <= 5 && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| "Zahl erwartet".to_string())
    };
    match verb {
        "ping" if rest.is_empty() => Ok(Cmd::Ping),
        "vdd-off" if rest.is_empty() => Ok(Cmd::VddOff),
        "vdd-on" => {
            let mut a = rest.split(' ');
            let (w, h, hz) = (num(a.next())?, num(a.next())?, num(a.next())?);
            if a.next().is_some() || !crate::vdd::mode_ok(w, h, hz) {
                return Err(format!("ungültiger Modus {rest}"));
            }
            Ok(Cmd::VddOn { w, h, hz })
        }
        "speaker-prepare" => {
            let (flag, name) = rest.split_once(' ').unwrap_or((rest, ""));
            let hide_output = match flag {
                "0" => false,
                "1" => true,
                _ => return Err("0 oder 1 erwartet".into()),
            };
            if !name.is_empty() && !valid_name(name) {
                return Err("ungültiger Name".into());
            }
            Ok(Cmd::Speaker {
                hide_output,
                name: name.to_string(),
            })
        }
        _ => Err("unbekannter Befehl".into()),
    }
}

fn execute(cmd: Cmd) -> Result<String> {
    match cmd {
        Cmd::Ping => Ok(env!("CARGO_PKG_VERSION").into()),
        Cmd::VddOn { w, h, hz } => {
            let changed = crate::vdd::turn_on(w, h, hz)?;
            if changed {
                crate::log!(
                    "Virtueller Monitor: Modus {w}x{h}@{hz} eingetragen, Treiber neu geladen"
                );
            }
            Ok(if changed { "changed" } else { "unchanged" }.into())
        }
        Cmd::VddOff => crate::vdd::disable().map(|_| String::new()),
        Cmd::Speaker { hide_output, name } => crate::audio::prepare_system(&name, hide_output),
    }
}

// ---------------------------------------------------------------- Handles

struct H(HANDLE);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn read_line(h: HANDLE) -> Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    while !buf.contains(&b'\n') {
        let mut n = 0u32;
        if unsafe {
            ReadFile(
                h,
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                &mut n,
                null_mut(),
            )
        } == 0
            || n == 0
        {
            bail!("Leitung getrennt");
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if buf.len() > MAX_LINE {
            bail!("Nachricht zu lang");
        }
    }
    let end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..end])
        .trim_end_matches('\r')
        .to_string())
}

fn write_line(h: HANDLE, s: &str) -> Result<()> {
    let line = format!("{}\n", s.replace(['\r', '\n'], " "));
    let mut done = 0usize;
    let b = line.as_bytes();
    while done < b.len() {
        let mut n = 0u32;
        if unsafe {
            WriteFile(
                h,
                b[done..].as_ptr(),
                (b.len() - done) as u32,
                &mut n,
                null_mut(),
            )
        } == 0
        {
            bail!("Leitung getrennt");
        }
        done += n as usize;
    }
    unsafe { FlushFileBuffers(h) };
    Ok(())
}

fn norm_path(s: &str) -> String {
    s.trim_start_matches(r"\\?\").to_lowercase()
}

fn process_image(pid: u32) -> Option<String> {
    unsafe {
        let p = H(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid));
        if p.0.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        if QueryFullProcessImageNameW(p.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

// ---------------------------------------------------------------- Client

static CLIENT_LOCK: Mutex<()> = Mutex::new(());

fn service_pid() -> Option<u32> {
    unsafe {
        let scm = OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return None;
        }
        let name = wide(SERVICE_NAME);
        let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
        let mut pid = None;
        if !svc.is_null() {
            let mut st: SERVICE_STATUS_PROCESS = std::mem::zeroed();
            let mut need = 0u32;
            if QueryServiceStatusEx(
                svc,
                SC_STATUS_PROCESS_INFO,
                &mut st as *mut _ as *mut u8,
                std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut need,
            ) != 0
                && st.dwCurrentState == SERVICE_RUNNING
            {
                pid = Some(st.dwProcessId);
            }
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        pid
    }
}

fn not_running() -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        crate::i18n::tr(
            "Der USBCD-Hilfsdienst läuft nicht – bitte das USBCD-Setup erneut ausführen",
            "The USBCD helper service is not running – please run the USBCD setup again"
        )
    )
}

/// Schickt einen Befehl an den Dienst und liefert dessen Antwort (ohne „ok“).
pub fn call(cmd: &str) -> Result<String> {
    let _g = CLIENT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let name = wide(PIPE_NAME);
    let pipe = unsafe {
        let mut tries = 0;
        loop {
            // SECURITY_IDENTIFICATION: ein falscher Server dürfte sich nicht als wir ausgeben
            let h = CreateFileW(
                name.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            );
            if h != INVALID_HANDLE_VALUE {
                break H(h);
            }
            let err = GetLastError();
            tries += 1;
            if err == ERROR_PIPE_BUSY && tries < 10 {
                WaitNamedPipeW(name.as_ptr(), 3000);
                continue;
            }
            if err == ERROR_FILE_NOT_FOUND && tries < 3 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
            return Err(not_running());
        }
    };
    // Sitzt am anderen Ende wirklich der Dienst?
    let mut server = 0u32;
    unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server) };
    if service_pid() != Some(server) {
        bail!(
            "{}",
            crate::i18n::tr(
                "Sicherheitswarnung: Ein fremdes Programm gibt sich als USBCD-Hilfsdienst aus",
                "Security warning: another program pretends to be the USBCD helper service"
            )
        );
    }
    write_line(pipe.0, cmd)?;
    let resp = read_line(pipe.0).context("USBCD-Hilfsdienst")?;
    if let Some(r) = resp.strip_prefix("ok") {
        Ok(r.trim().to_string())
    } else if let Some(e) = resp.strip_prefix("err ") {
        bail!("{e}")
    } else {
        bail!("USBCD-Hilfsdienst: unerwartete Antwort")
    }
}

pub fn vdd_on(w: u32, h: u32, hz: u32) -> Result<bool> {
    Ok(call(&format!("vdd-on {w} {h} {hz}"))? == "changed")
}

pub fn vdd_off() -> Result<()> {
    call("vdd-off").map(|_| ())
}

pub fn speaker_prepare(hide_output: bool, name: &str) -> Result<String> {
    let name = if valid_name(name) { name } else { "" };
    call(format!("speaker-prepare {} {name}", hide_output as u8).trim_end())
}

// ---------------------------------------------------------------- Dienst

static STOP: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);

fn report(state: SERVICE_STATUS_CURRENT_STATE, exit: u32) {
    let st = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: exit,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_RUNNING || state == SERVICE_STOPPED {
            0
        } else {
            5000
        },
    };
    let h = STATUS_HANDLE.load(Ordering::SeqCst);
    if h != 0 {
        unsafe { SetServiceStatus(h as SERVICE_STATUS_HANDLE, &st) };
    }
}

/// Weckt den wartenden ConnectNamedPipe auf.
fn poke_pipe() {
    let name = wide(PIPE_NAME);
    unsafe {
        let _h = H(CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        ));
    }
}

unsafe extern "system" fn handler(ctrl: u32, _t: u32, _d: *mut c_void, _c: *mut c_void) -> u32 {
    match ctrl {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            STOP.store(true, Ordering::SeqCst);
            report(SERVICE_STOP_PENDING, 0);
            std::thread::spawn(poke_pipe);
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
    let name = wide(SERVICE_NAME);
    let h = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), null());
    STATUS_HANDLE.store(h as usize, Ordering::SeqCst);
    report(SERVICE_START_PENDING, 0);
    // COM für die Audio-Befehle (Endpunkt umbenennen/ausblenden)
    windows_sys::Win32::System::Com::CoInitializeEx(
        null(),
        windows_sys::Win32::System::Com::COINIT_MULTITHREADED as u32,
    );
    report(SERVICE_RUNNING, 0);
    crate::log!("USBCD-Hilfsdienst {} gestartet", env!("CARGO_PKG_VERSION"));
    let r = serve();
    if let Err(e) = &r {
        crate::log!("Hilfsdienst: {e:#}");
    }
    crate::log!("USBCD-Hilfsdienst beendet");
    report(SERVICE_STOPPED, if r.is_ok() { 0 } else { 1 });
}

/// Einstiegspunkt für `USBCD.exe --service` (nur vom Dienststeuerungs-Manager aufgerufen).
pub fn run_service() -> i32 {
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        eprintln!("--service ist nur für den Windows-Dienst „{SERVICE_NAME}“ gedacht.");
        return 1;
    }
    0
}

fn serve() -> Result<()> {
    let sddl = wide(PIPE_SDDL);
    let mut sd: *mut c_void = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        )
    } == 0
    {
        bail!("Sicherheitsbeschreibung der Leitung ungültig");
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let me = std::env::current_exe()
        .map(|p| norm_path(&p.to_string_lossy()))
        .unwrap_or_default();
    let name = wide(PIPE_NAME);
    let mut squat_logged = false;

    while !STOP.load(Ordering::SeqCst) {
        // FIRST_PIPE_INSTANCE + nur eine Instanz: hat ein anderes Programm den Namen
        // belegt, schlägt das fehl, statt dass wir uns die Leitung teilen.
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                &sa,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            if !squat_logged {
                crate::log!(
                    "Leitung {PIPE_NAME} nicht anlegbar ({}) – belegt ein anderes Programm den Namen?",
                    std::io::Error::last_os_error()
                );
                squat_logged = true;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }
        squat_logged = false;
        let pipe = H(h);
        let connected = unsafe { ConnectNamedPipe(pipe.0, null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        if !connected {
            continue;
        }
        let mut pid = 0u32;
        unsafe { GetNamedPipeClientProcessId(pipe.0, &mut pid) };
        let client = process_image(pid)
            .map(|p| norm_path(&p))
            .unwrap_or_default();
        let resp = if client.is_empty() || client != me {
            crate::log!("Auftrag von fremdem Programm abgelehnt: {client:?} (PID {pid})");
            "err denied".to_string()
        } else {
            match read_line(pipe.0) {
                Err(_) => continue,
                Ok(line) => match parse(&line) {
                    Err(e) => {
                        crate::log!("Ungültiger Auftrag abgelehnt: {e}");
                        format!("err {e}")
                    }
                    Ok(cmd) => {
                        let quiet = cmd == Cmd::Ping;
                        match execute(cmd) {
                            Ok(r) => format!("ok {r}"),
                            Err(e) => {
                                if !quiet {
                                    crate::log!("{line}: {e:#}");
                                }
                                format!("err {e:#}")
                            }
                        }
                    }
                },
            }
        };
        let _ = write_line(pipe.0, &resp);
        unsafe { DisconnectNamedPipe(pipe.0) };
    }
    unsafe { LocalFree(sd as _) };
    Ok(())
}

// ---------------------------------------------------------------- Einrichtung (Setup)

/// Legt den Dienst an bzw. aktualisiert ihn und startet ihn (`--install-service`).
pub fn install() -> Result<()> {
    let exe = std::env::current_exe().context("Pfad der exe")?;
    let exe = exe
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    let bin = wide(&format!("\"{exe}\" --service"));
    let name = wide(SERVICE_NAME);
    let display = wide(DISPLAY_NAME);
    let mut desc = wide(crate::i18n::tr(
        "Schaltet für USBCD den virtuellen Tablet-Monitor ein und aus. Nimmt nur feste Befehle der installierten USBCD.exe an.",
        "Turns the virtual tablet monitor on and off for USBCD. Accepts only fixed commands from the installed USBCD.exe.",
    ));
    unsafe {
        let scm = OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS);
        if scm.is_null() {
            bail!(
                "Dienstverwaltung nicht erreichbar ({})",
                std::io::Error::last_os_error()
            );
        }
        let mut svc = OpenServiceW(scm, name.as_ptr(), SERVICE_ALL_ACCESS);
        if svc.is_null() {
            svc = CreateServiceW(
                scm,
                name.as_ptr(),
                display.as_ptr(),
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                bin.as_ptr(),
                null(),
                null_mut(),
                null(),
                null(), // LocalSystem
                null(),
            );
            if svc.is_null() {
                let e = std::io::Error::last_os_error();
                CloseServiceHandle(scm);
                bail!("Dienst nicht anlegbar ({e})");
            }
        } else if ChangeServiceConfigW(
            svc,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            bin.as_ptr(),
            null(),
            null_mut(),
            null(),
            null(),
            null(),
            display.as_ptr(),
        ) == 0
        {
            crate::log!("Dienst aktualisieren: {}", std::io::Error::last_os_error());
        }
        let d = SERVICE_DESCRIPTIONW {
            lpDescription: desc.as_mut_ptr(),
        };
        ChangeServiceConfig2W(
            svc,
            SERVICE_CONFIG_DESCRIPTION,
            &d as *const _ as *const c_void,
        );
        let mut actions = [
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 5000,
            },
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 15000,
            },
            SC_ACTION {
                Type: SC_ACTION_RESTART,
                Delay: 60000,
            },
        ];
        let fa = SERVICE_FAILURE_ACTIONSW {
            dwResetPeriod: 86400,
            lpRebootMsg: null_mut(),
            lpCommand: null_mut(),
            cActions: actions.len() as u32,
            lpsaActions: actions.as_mut_ptr(),
        };
        ChangeServiceConfig2W(
            svc,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            &fa as *const _ as *const c_void,
        );
        let started =
            StartServiceW(svc, 0, null()) != 0 || GetLastError() == ERROR_SERVICE_ALREADY_RUNNING;
        let e = std::io::Error::last_os_error();
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        if !started {
            bail!("Dienst nicht startbar ({e})");
        }
    }
    crate::log!("Hilfsdienst {SERVICE_NAME} eingerichtet ({exe})");
    Ok(())
}

/// Hält den Dienst an und entfernt ihn (`--uninstall-service`).
pub fn uninstall() {
    let name = wide(SERVICE_NAME);
    unsafe {
        let scm = OpenSCManagerW(null(), null(), SC_MANAGER_ALL_ACCESS);
        if scm.is_null() {
            return;
        }
        let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_ALL_ACCESS);
        if !svc.is_null() {
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            ControlService(svc, SERVICE_CONTROL_STOP, &mut st);
            for _ in 0..50 {
                if QueryServiceStatus(svc, &mut st) == 0 || st.dwCurrentState == SERVICE_STOPPED {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            DeleteService(svc);
            CloseServiceHandle(svc);
            crate::log!("Hilfsdienst {SERVICE_NAME} entfernt");
        }
        CloseServiceHandle(scm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_known_commands() {
        assert_eq!(parse("ping"), Ok(Cmd::Ping));
        assert_eq!(parse("vdd-off\r\n"), Ok(Cmd::VddOff));
        assert_eq!(
            parse("vdd-on 2000 1200 60"),
            Ok(Cmd::VddOn {
                w: 2000,
                h: 1200,
                hz: 60
            })
        );
        assert!(parse("vdd-on 2000 1200 60 x").is_err());
        assert!(parse("vdd-on 2000 1200 999").is_err());
        assert!(parse("vdd-on -1 1200 60").is_err());
        assert!(parse("vdd-on 2000 1200").is_err());
        assert!(parse("vdd-off now").is_err());
        assert!(parse("powershell whoami").is_err());
        assert_eq!(
            parse("speaker-prepare 1 USBCD Tablet Lautsprecher"),
            Ok(Cmd::Speaker {
                hide_output: true,
                name: "USBCD Tablet Lautsprecher".into()
            })
        );
        assert_eq!(
            parse("speaker-prepare 0"),
            Ok(Cmd::Speaker {
                hide_output: false,
                name: String::new()
            })
        );
        assert!(parse("speaker-prepare 1 a'b").is_err());
        assert!(parse("speaker-prepare 1 a$(x)").is_err());
        assert!(parse("speaker-prepare 2 x").is_err());
        assert!(parse(&"x".repeat(600)).is_err());
    }

    /// Randfälle: nur genau die erlaubten Formen gehen durch.
    #[test]
    fn rejects_edge_cases() {
        // Groß-/Kleinschreibung, Leerzeichen, Steuerzeichen
        assert!(parse("PING").is_err());
        assert_eq!(parse("ping "), Ok(Cmd::Ping)); // leerer Rest, harmlos
        assert!(parse(" ping").is_err());
        assert!(parse("ping\0").is_err());
        // Zahlen: keine Vorzeichen, keine Leerstellen, höchstens 5 Stellen
        assert!(parse("vdd-on +2000 1200 60").is_err());
        assert!(parse("vdd-on 2000  1200 60").is_err());
        assert!(parse("vdd-on 2000 1200 060").is_ok());
        assert!(parse("vdd-on 2000 1200 000060").is_err());
        assert!(parse("vdd-on 0x7d0 1200 60").is_err());
        // Namen: höchstens 40 Zeichen, nicht mit Leerzeichen beginnend, keine Steuerzeichen
        assert!(parse(&format!("speaker-prepare 1 {}", "a".repeat(40))).is_ok());
        assert!(parse(&format!("speaker-prepare 1 {}", "a".repeat(41))).is_err());
        assert!(parse("speaker-prepare 1  Name").is_err());
        assert!(parse("speaker-prepare 1 a\nb").is_err());
        assert!(parse("speaker-prepare 1 a\tb").is_err());
        assert!(parse("speaker-prepare 1 Lautsprecher (Tablet)").is_ok());
        assert!(parse("speaker-prepare 1 a;b").is_err());
        assert!(parse("speaker-prepare 1 a|b").is_err());
    }

    /// Rechte der Leitung: Netzwerkzugriff verboten, SYSTEM/Admins voll, angemeldete Nutzer
    /// nur Lesen/Schreiben. Eine Änderung hier muss bewusst passieren.
    #[test]
    fn pipe_rights_unchanged() {
        assert_eq!(
            PIPE_SDDL,
            "D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)"
        );
        assert_eq!(MAX_LINE, 512);
    }
}
