//! Hilfsprogramme (ffmpeg, adb) starten: ohne Konsolenfenster, an USBCD gebunden (enden
//! mit USBCD), nicht von Windows gedrosselt und – beim Herunterfahren – gar nicht mehr.

use std::process::Command;
#[cfg(windows)]
use std::time::Instant;

#[cfg(windows)]
use crate::win::wide;

/// Kindprozess ohne aufpoppendes Konsolenfenster.
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Hilfsprozesse (ffmpeg, adb-Beobachter) an USBCD binden: Endet USBCD – auch durch
/// Absturz oder „Task beenden“ –, beendet Windows sie mit. Der adb-Server selbst wird
/// nicht angebunden (andere Programme wie Android Studio nutzen ihn mit).
pub fn bind_to_us(child: &std::process::Child) {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let job = *JOB.get_or_init(|| {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            h as usize // bleibt bis zum Prozessende offen
        });
        if job != 0 {
            AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
        }
    }
    #[cfg(not(windows))]
    let _ = child;
}

/// Startet ffmpeg für die Bildübertragung und bindet es an USBCD.
///
/// Immer: Windows soll ffmpeg nicht als Hintergrundprozess drosseln („Energiedrosselung“
/// auf dem Akku), denn es arbeitet in Echtzeit.
/// Mit `boost` zusätzlich Vorrang wie bei Game-Streaming-Servern: CPU „höher als normal“ und
/// Grafikchip „hoch“ (nie „Echtzeit“ – damit kann NVENC einfrieren). ffmpeg startet
/// angehalten, damit Bindung, Drosselung und CPU-Stufe stehen, bevor es loslegt. Die GPU-Stufe
/// nimmt Windows erst an, wenn der Prozess den Grafikchip benutzt (vorher „ungültiger
/// Parameter“) – sie wird deshalb gleich nach dem Start mehrmals versucht.
/// Klappt etwas davon nicht, läuft es wie bisher.
pub fn spawn_stream(cmd: &mut Command, boost: bool) -> std::io::Result<std::process::Child> {
    may_spawn()?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        if boost {
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
            let mut child = cmd.spawn()?;
            // vor dem Loslaufen binden: endet USBCD jetzt, endet auch der angehaltene ffmpeg
            bind_to_us(&child);
            let h = child.as_raw_handle();
            unsafe {
                process_high_qos(h);
                let cpu = windows_sys::Win32::System::Threading::SetPriorityClass(
                    h as _,
                    windows_sys::Win32::System::Threading::ABOVE_NORMAL_PRIORITY_CLASS,
                ) != 0;
                if resume_threads(child.id()) {
                    crate::log!(
                        "Vorrang für ffmpeg: CPU {}",
                        if cpu {
                            "höher als normal"
                        } else {
                            "unverändert"
                        }
                    );
                    raise_gpu_priority_soon(h);
                    return Ok(child);
                }
            }
            crate::log!("ffmpeg ließ sich nicht fortsetzen – Start ohne Vorrang");
            let _ = child.kill();
            let _ = child.wait();
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn()?;
        bind_to_us(&child);
        unsafe { process_high_qos(child.as_raw_handle()) };
        Ok(child)
    }
    #[cfg(not(windows))]
    {
        let _ = boost;
        cmd.spawn()
    }
}

/// Prozess ausdrücklich nicht drosseln (Gegenstück zu EcoQoS; Windows 11 stuft sonst
/// Kindprozesse eines unsichtbaren Fensters als „Low“ ein → auf dem Akku sparsamster Takt).
#[cfg(windows)]
unsafe fn process_high_qos(h: std::os::windows::io::RawHandle) {
    use windows_sys::Win32::System::Threading::{
        ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    };
    let s = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    SetProcessInformation(
        h as _,
        ProcessPowerThrottling,
        &s as *const _ as *const std::ffi::c_void,
        std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
    );
}

/// Den aktuellen Thread nicht drosseln (Video- und Ton-Übertragung arbeiten in Echtzeit).
pub fn high_qos_current_thread() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadInformation, ThreadPowerThrottling,
            THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            THREAD_POWER_THROTTLING_STATE,
        };
        let s = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            &s as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

/// GPU-Stufe „hoch“ im Hintergrund setzen: sofort nach dem Start und dann mit wachsenden
/// Abständen, bis Windows sie annimmt (spätestens nach ~4 s aufgeben). Eigene Kopie des
/// Prozess-Handles, damit es gültig bleibt, auch wenn ffmpeg inzwischen beendet wurde.
#[cfg(windows)]
unsafe fn raise_gpu_priority_soon(h: std::os::windows::io::RawHandle) {
    use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut dup: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
    if DuplicateHandle(
        GetCurrentProcess(),
        h as _,
        GetCurrentProcess(),
        &mut dup,
        0,
        0,
        DUPLICATE_SAME_ACCESS,
    ) == 0
    {
        crate::log!("GPU-Vorrang nicht gesetzt (kein Prozess-Handle) – läuft normal weiter");
        return;
    }
    let dup = dup as usize;
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut last = 0;
        for wait_ms in [20u64, 30, 50, 100, 200, 400, 800, 1200, 1600] {
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            match raise_gpu_priority(dup as _) {
                Ok(level) => {
                    crate::log!(
                        "Vorrang für ffmpeg: Grafikchip {level} (nach {} ms)",
                        start.elapsed().as_millis()
                    );
                    CloseHandle(dup as _);
                    return;
                }
                Err(st) => last = st,
            }
        }
        crate::log!(
            "GPU-Vorrang nicht gesetzt (NTSTATUS 0x{:08x}) – läuft normal weiter",
            last as u32
        );
        CloseHandle(dup as _);
    });
}

/// GPU-Stufe „hoch“ setzen und zurücklesen; Ok(Beschreibung) oder Err(NTSTATUS).
#[cfg(windows)]
unsafe fn raise_gpu_priority(h: std::os::windows::io::RawHandle) -> Result<String, i32> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    // D3DKMT_SCHEDULINGPRIORITYCLASS: 0 Idle … 2 Normal, 3 AboveNormal, 4 High, 5 Realtime
    const HIGH: i32 = 4;
    type SetFn = unsafe extern "system" fn(*mut std::ffi::c_void, i32) -> i32;
    type GetFn = unsafe extern "system" fn(*mut std::ffi::c_void, *mut i32) -> i32;
    // STATUS_NOT_IMPLEMENTED: Funktion fehlt in diesem Windows
    const NOT_FOUND: i32 = 0xC000_0002u32 as i32;
    let lib = LoadLibraryW(wide("gdi32.dll").as_ptr());
    if lib.is_null() {
        return Err(NOT_FOUND);
    }
    let set = GetProcAddress(
        lib,
        c"D3DKMTSetProcessSchedulingPriorityClass".as_ptr() as _,
    )
    .ok_or(NOT_FOUND)?;
    let get = GetProcAddress(
        lib,
        c"D3DKMTGetProcessSchedulingPriorityClass".as_ptr() as _,
    );
    let set: SetFn = std::mem::transmute(set);
    let st = set(h as _, HIGH);
    if st < 0 {
        return Err(st);
    }
    let mut now = -1i32;
    if let Some(get) = get {
        let get: GetFn = std::mem::transmute(get);
        get(h as _, &mut now);
    }
    Ok(match now {
        HIGH => "hoch".into(),
        -1 => "hoch (nicht zurückgelesen)".into(),
        n => format!("Stufe {n} statt hoch"),
    })
}

/// Alle Threads eines angehalten gestarteten Prozesses fortsetzen.
#[cfg(windows)]
unsafe fn resume_threads(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return false;
    }
    let mut te: THREADENTRY32 = std::mem::zeroed();
    te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let (mut found, mut ok) = (0, true);
    let mut more = Thread32First(snap, &mut te) != 0;
    while more {
        if te.th32OwnerProcessID == pid {
            found += 1;
            let t = OpenThread(THREAD_SUSPEND_RESUME, 0, te.th32ThreadID);
            if t.is_null() || ResumeThread(t) == u32::MAX {
                ok = false;
            }
            if !t.is_null() {
                CloseHandle(t);
            }
        }
        more = Thread32Next(snap, &mut te) != 0;
    }
    CloseHandle(snap);
    ok && found > 0
}

/// Darf jetzt ein Hilfsprogramm (ffmpeg, adb) starten? Beim Herunterfahren nicht mehr:
/// Neue Prozesse scheitern dann beim Laden (0xc0000142), und Windows zeigt dafür ein
/// Fehlerfenster.
pub fn may_spawn() -> std::io::Result<()> {
    if crate::state::windows_ending() {
        Err(std::io::Error::other(
            "Windows wird beendet – kein Programmstart mehr",
        ))
    } else {
        Ok(())
    }
}

/// Hilfsprogramme sollen nie eigene Fehlerfenster zeigen (Ladefehler, Absturz): Sie laufen
/// unsichtbar, ihre Fehler landen im Log. Die Kindprozesse erben diese Einstellung.
pub fn quiet_error_boxes() {
    #[cfg(windows)]
    {
        windows_targets::link!("kernel32.dll" "system" fn SetErrorMode(umode: u32) -> u32);
        const SEM_FAILCRITICALERRORS: u32 = 0x0001;
        const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
        const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        }
    }
}
