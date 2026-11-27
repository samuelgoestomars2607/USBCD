//! usbcd_cam.dll – die virtuelle Windows-11-Kamera „USBCD Tablet Webcam“.
//!
//! Aufbau (Windows 11, MFCreateVirtualCamera):
//!   USBCD.exe ruft `UsbcdCamStart` auf → Windows legt die Kamera für den angemeldeten
//!   Nutzer an, solange USBCD läuft (Lebensdauer „Sitzung“, keine Administratorrechte,
//!   wird nie zur Standardkamera gemacht).
//!   Öffnet eine App (Teams, Discord, Zoom, Browser, Kamera-App …) die Kamera, lädt der
//!   Windows-Kameradienst („Frame Server“, Dienstkonto LocalService) diese DLL über die
//!   CLSID und holt die Bilder aus der Media-Foundation-Quelle unten.
//!   Die Quelle verbindet sich nur, solange eine App die Kamera wirklich benutzt, mit
//!   USBCD.exe (Named Pipe `\\.\pipe\USBCD-Kamera`) und bekommt dort fertige NV12-Bilder.
//!   Verbindung steht ⇔ Kamera in Benutzung → erst dann schaltet USBCD die Selfie-Kamera
//!   des Tablets ein.
#![allow(non_snake_case)]
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::*;
use windows::Win32::Media::KernelStreaming::{
    IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::*;
use windows_core::*;

// --- gemeinsam mit host/src/camera.rs -------------------------------------------

/// CLSID der Kameraquelle (Registrierung unter HKLM\Software\Classes\CLSID durch das Setup)
pub const CLSID_USBCD_CAM: GUID = GUID::from_u128(0xce63ec35_a188_4e54_8e6f_8f3c01ef1cba);
pub const CLSID_USBCD_CAM_STR: &str = "{CE63EC35-A188-4E54-8E6F-8F3C01EF1CBA}";
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;
pub const FRAME_LEN: usize = (WIDTH * HEIGHT * 3 / 2) as usize;
pub const PIPE_NAME: &str = r"\\.\pipe\USBCD-Kamera";
/// Kopf jedes Bildes in der Pipe: "UCAM", Breite, Höhe, Länge (je u32 little-endian)
pub const MAGIC: &[u8; 4] = b"UCAM";
pub const HEADER_LEN: usize = 16;

/// Prüft den Kopf eines Bildes aus der Pipe.
pub fn header_ok(h: &[u8; HEADER_LEN]) -> bool {
    let u = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    &h[..4] == MAGIC && u(4) == WIDTH && u(8) == HEIGHT && u(12) as usize == FRAME_LEN
}

pub fn header() -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(MAGIC);
    h[4..8].copy_from_slice(&WIDTH.to_le_bytes());
    h[8..12].copy_from_slice(&HEIGHT.to_le_bytes());
    h[12..16].copy_from_slice(&(FRAME_LEN as u32).to_le_bytes());
    h
}

/// Schwarzes NV12-Bild (Y = 16, U/V = 128).
pub fn black_frame() -> Vec<u8> {
    let y = (WIDTH * HEIGHT) as usize;
    let mut v = vec![16u8; FRAME_LEN];
    v[y..].fill(128);
    v
}

// --- Protokoll (nur Fehler; nach ProgramData\USBCD\kamera.log, falls beschreibbar) ---

fn log(msg: &str) {
    use std::io::Write;
    let Some(dir) = std::env::var_os("ProgramData") else {
        return;
    };
    let path = std::path::PathBuf::from(dir)
        .join("USBCD")
        .join("kamera.log");
    if std::fs::metadata(&path)
        .map(|m| m.len() > 512 << 10)
        .unwrap_or(false)
    {
        let _ = std::fs::write(&path, b"");
    }
    // nur eine vom Setup angelegte Datei beschreiben (keine neuen Dateien anlegen)
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{t}] {msg}");
    }
}

/// Media-Foundation-Objekte sind frei verwendbar (MTA) – für die eigenen Threads.
struct Agile<T>(T);
unsafe impl<T> Send for Agile<T> {}
unsafe impl<T> Sync for Agile<T> {}

// --- Bildquelle: liest NV12-Bilder von USBCD.exe ----------------------------------

#[derive(Default)]
struct FeedState {
    latest: Option<Arc<Vec<u8>>>,
    /// zählt jedes neue Bild und jede neue Anfrage hoch (weckt die Ausgabe)
    seq: u64,
    stop: bool,
}

struct Feed {
    state: Mutex<FeedState>,
    cv: Condvar,
}

impl Feed {
    fn start() -> Arc<Feed> {
        let feed = Arc::new(Feed {
            state: Mutex::new(FeedState::default()),
            cv: Condvar::new(),
        });
        let f = feed.clone();
        std::thread::spawn(move || f.run());
        feed
    }

    fn stopped(&self) -> bool {
        self.state.lock().map(|g| g.stop).unwrap_or(true)
    }

    fn stop(&self) {
        if let Ok(mut g) = self.state.lock() {
            g.stop = true;
        }
        self.cv.notify_all();
    }

    fn wake(&self) {
        if let Ok(mut g) = self.state.lock() {
            g.seq += 1;
        }
        self.cv.notify_all();
    }

    fn run(&self) {
        let name: Vec<u16> = PIPE_NAME.encode_utf16().chain(Some(0)).collect();
        let mut logged = false;
        while !self.stopped() {
            let h = unsafe {
                CreateFileW(
                    PCWSTR(name.as_ptr()),
                    FILE_GENERIC_READ.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    // USBCD darf sich nicht als Kameradienst ausgeben können
                    SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    None,
                )
            };
            let h = match h {
                Ok(h) => h,
                Err(e) => {
                    if !logged {
                        log(&format!("USBCD nicht erreichbar ({PIPE_NAME}): {e}"));
                        logged = true;
                    }
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
            };
            logged = false;
            log("mit USBCD verbunden");
            let mut head = [0u8; HEADER_LEN];
            loop {
                if self.stopped() || !read_exact(h, &mut head) || !header_ok(&head) {
                    break;
                }
                let mut frame = vec![0u8; FRAME_LEN];
                if !read_exact(h, &mut frame) {
                    break;
                }
                if let Ok(mut g) = self.state.lock() {
                    g.latest = Some(Arc::new(frame));
                    g.seq += 1;
                }
                self.cv.notify_all();
            }
            unsafe {
                let _ = CloseHandle(h);
            }
            if let Ok(mut g) = self.state.lock() {
                g.latest = None;
            }
            log("Verbindung zu USBCD beendet");
        }
    }
}

fn read_exact(h: HANDLE, buf: &mut [u8]) -> bool {
    let mut off = 0;
    while off < buf.len() {
        let mut n = 0u32;
        let ok = unsafe { ReadFile(h, Some(&mut buf[off..]), Some(&mut n), None) };
        if ok.is_err() || n == 0 {
            return false;
        }
        off += n as usize;
    }
    true
}

// --- Bildausgabe: beantwortet Anfragen der Kamera-Pipeline ------------------------

/// Hält Anfragen (RequestSample) fest und beantwortet sie mit dem nächsten Bild –
/// im Takt der Tablet-Kamera, ohne Bild spätestens alle 100 ms (Standbild/Schwarz).
struct Pump {
    queue: Agile<IMFMediaEventQueue>,
    requests: Mutex<VecDeque<Agile<Option<IUnknown>>>>,
    running: AtomicBool,
    feed: Mutex<Option<Arc<Feed>>>,
}

impl Pump {
    fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let feed = Feed::start();
        if let Ok(mut g) = self.feed.lock() {
            *g = Some(feed.clone());
        }
        let me = self.clone();
        std::thread::spawn(move || me.run(feed));
    }

    fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(f) = self.feed.lock().ok().and_then(|mut g| g.take()) {
            f.stop();
        }
        if let Ok(mut g) = self.requests.lock() {
            g.clear();
        }
    }

    fn request(&self, token: Option<IUnknown>) {
        if let Ok(mut g) = self.requests.lock() {
            // Puffer begrenzen – mehr als ein paar offene Anfragen gibt es nie
            if g.len() >= 8 {
                g.pop_front();
            }
            g.push_back(Agile(token));
        }
        if let Some(f) = self.feed.lock().ok().and_then(|g| g.clone()) {
            f.wake();
        }
    }

    fn run(&self, feed: Arc<Feed>) {
        let black = Arc::new(black_frame());
        let mut seen = 0u64;
        let mut last_sent = Instant::now();
        let mut last_time: i64 = 0;
        while self.running.load(Ordering::SeqCst) {
            // auf ein neues Bild warten (oder eine neue Anfrage), höchstens 100 ms
            let frame = {
                let Ok(g) = feed.state.lock() else { break };
                let Ok((g, _)) = feed
                    .cv
                    .wait_timeout_while(g, Duration::from_millis(100), |s| {
                        s.seq == seen && !s.stop
                    })
                else {
                    break;
                };
                if g.stop {
                    break;
                }
                let fresh = g.seq != seen;
                seen = g.seq;
                let has_frame = g.latest.is_some();
                // neues Bild: sofort; sonst Standbild bzw. Schwarz alle 100 ms
                if !fresh && last_sent.elapsed() < Duration::from_millis(100) {
                    continue;
                }
                if fresh && has_frame && last_sent.elapsed() < Duration::from_millis(15) {
                    // nicht schneller als ~60 Bilder/s ausliefern
                    std::thread::sleep(Duration::from_millis(5));
                }
                g.latest.clone().unwrap_or_else(|| black.clone())
            };
            let Some(token) = self.requests.lock().ok().and_then(|mut g| g.pop_front()) else {
                continue;
            };
            let now = unsafe { MFGetSystemTime() };
            let dur = if last_time > 0 {
                (now - last_time).clamp(1, 10_000_000)
            } else {
                10_000_000 / FPS as i64
            };
            last_time = now;
            last_sent = Instant::now();
            if let Err(e) = self.deliver(&frame, now, dur, token.0) {
                if e.code() != MF_E_SHUTDOWN {
                    log(&format!("Bild ausliefern: {e}"));
                }
            }
        }
    }

    fn deliver(&self, frame: &[u8], time: i64, dur: i64, token: Option<IUnknown>) -> Result<()> {
        unsafe {
            let buf = MFCreateMemoryBuffer(FRAME_LEN as u32)?;
            let mut p: *mut u8 = std::ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            std::ptr::copy_nonoverlapping(frame.as_ptr(), p, FRAME_LEN.min(frame.len()));
            buf.Unlock()?;
            buf.SetCurrentLength(FRAME_LEN as u32)?;
            let s = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(time)?;
            s.SetSampleDuration(dur)?;
            if let Some(t) = &token {
                s.SetUnknown(&MFSampleExtension_Token, t)?;
            }
            self.queue
                .0
                .QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &s)
        }
    }
}

// --- IMFMediaStream2 ---------------------------------------------------------------

#[implement(IMFMediaStream2)]
struct Stream {
    source: Mutex<Option<IMFMediaSource>>,
    queue: IMFMediaEventQueue,
    desc: IMFStreamDescriptor,
    attrs: IMFAttributes,
    state: Mutex<MF_STREAM_STATE>,
    shutdown: AtomicBool,
    pump: Arc<Pump>,
}

fn shut() -> Error {
    Error::from(MF_E_SHUTDOWN)
}

impl Stream {
    fn new() -> Result<Stream> {
        unsafe {
            let mt = MFCreateMediaType()?;
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            mt.SetUINT64(&MF_MT_FRAME_SIZE, ((WIDTH as u64) << 32) | HEIGHT as u64)?;
            mt.SetUINT64(&MF_MT_FRAME_RATE, ((FPS as u64) << 32) | 1)?;
            mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
            mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            mt.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
            mt.SetUINT32(&MF_MT_SAMPLE_SIZE, FRAME_LEN as u32)?;
            mt.SetUINT32(&MF_MT_DEFAULT_STRIDE, WIDTH)?;
            mt.SetUINT32(&MF_MT_AVG_BITRATE, (FRAME_LEN as u32) * 8 * FPS)?;

            let desc = MFCreateStreamDescriptor(0, &[Some(mt.clone())])?;
            desc.GetMediaTypeHandler()?.SetCurrentMediaType(&mt)?;
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 4)?;
            let attrs = attrs.ok_or_else(|| Error::from(E_OUTOFMEMORY))?;
            for a in [&attrs, &desc.cast::<IMFAttributes>()?] {
                a.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
                a.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
                a.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
                a.SetUINT32(
                    &MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
                    MFFrameSourceTypes_Color.0 as u32,
                )?;
            }
            let queue = MFCreateEventQueue()?;
            Ok(Stream {
                source: Mutex::new(None),
                pump: Arc::new(Pump {
                    queue: Agile(queue.clone()),
                    requests: Mutex::new(VecDeque::new()),
                    running: AtomicBool::new(false),
                    feed: Mutex::new(None),
                }),
                queue,
                desc,
                attrs,
                state: Mutex::new(MF_STREAM_STATE_STOPPED),
                shutdown: AtomicBool::new(false),
            })
        }
    }

    fn set_state(&self, s: MF_STREAM_STATE) -> Result<()> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(shut());
        }
        if let Ok(mut g) = self.state.lock() {
            *g = s;
        }
        if s == MF_STREAM_STATE_RUNNING {
            self.pump.start();
        } else {
            self.pump.stop();
        }
        Ok(())
    }

    fn start(&self, pos: *const PROPVARIANT) -> Result<()> {
        self.set_state(MF_STREAM_STATE_RUNNING)?;
        unsafe {
            self.queue
                .QueueEventParamVar(MEStreamStarted.0 as u32, &GUID::zeroed(), S_OK, pos)
        }
    }

    fn stop(&self) -> Result<()> {
        self.set_state(MF_STREAM_STATE_STOPPED)?;
        unsafe {
            self.queue.QueueEventParamVar(
                MEStreamStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )
        }
    }

    fn close(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.pump.stop();
        unsafe {
            let _ = self.queue.Shutdown();
        }
        if let Ok(mut g) = self.source.lock() {
            *g = None; // Kreisbezug Quelle ↔ Strom auflösen
        }
    }
}

impl IMFMediaEventGenerator_Impl for Stream_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.queue.clone();
        unsafe { q.GetEvent(dwflags.0) }
    }
    fn BeginGetEvent(
        &self,
        pcallback: Ref<'_, IMFAsyncCallback>,
        punkstate: Ref<'_, IUnknown>,
    ) -> Result<()> {
        unsafe {
            self.queue
                .BeginGetEvent(pcallback.as_ref(), punkstate.as_ref())
        }
    }
    fn EndGetEvent(&self, presult: Ref<'_, IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(presult.as_ref()) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        guidextendedtype: *const GUID,
        hrstatus: HRESULT,
        pvvalue: *const PROPVARIANT,
    ) -> Result<()> {
        unsafe {
            self.queue
                .QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue)
        }
    }
}

impl IMFMediaStream_Impl for Stream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        self.source
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(shut)
    }
    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(shut());
        }
        Ok(self.desc.clone())
    }
    fn RequestSample(&self, ptoken: Ref<'_, IUnknown>) -> Result<()> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(shut());
        }
        let running = self
            .state
            .lock()
            .map(|g| *g == MF_STREAM_STATE_RUNNING)
            .unwrap_or(false);
        if !running {
            return Err(Error::from(MF_E_MEDIA_SOURCE_WRONGSTATE));
        }
        self.pump.request(ptoken.as_ref().cloned());
        Ok(())
    }
}

impl IMFMediaStream2_Impl for Stream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> Result<()> {
        self.set_state(value)
    }
    fn GetStreamState(&self) -> Result<MF_STREAM_STATE> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(shut());
        }
        Ok(self
            .state
            .lock()
            .map(|g| *g)
            .unwrap_or(MF_STREAM_STATE_STOPPED))
    }
}

// --- IMFMediaSourceEx ----------------------------------------------------------------

#[implement(IMFMediaSourceEx, IMFGetService, IKsControl)]
struct Source {
    queue: IMFMediaEventQueue,
    attrs: IMFAttributes,
    pd: IMFPresentationDescriptor,
    stream: Mutex<Option<ComObject<Stream>>>,
    announced: AtomicBool,
    shutdown: AtomicBool,
}

impl Source {
    fn create() -> Result<IMFMediaSourceEx> {
        let stream = ComObject::new(Stream::new()?);
        let pd = unsafe { MFCreatePresentationDescriptor(Some(&[Some(stream.desc.clone())]))? };
        unsafe { pd.SelectStream(0)? };
        let mut attrs = None;
        unsafe { MFCreateAttributes(&mut attrs, 4)? };
        let src = ComObject::new(Source {
            queue: unsafe { MFCreateEventQueue()? },
            attrs: attrs.ok_or_else(|| Error::from(E_OUTOFMEMORY))?,
            pd,
            stream: Mutex::new(Some(stream.clone())),
            announced: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });
        if let Ok(mut g) = stream.source.lock() {
            *g = Some(
                src.to_interface::<IMFMediaSourceEx>()
                    .cast::<IMFMediaSource>()?,
            );
        }
        Ok(src.to_interface())
    }

    fn check(&self) -> Result<ComObject<Stream>> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(shut());
        }
        self.stream
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(shut)
    }
}

impl IMFMediaEventGenerator_Impl for Source_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.queue.clone();
        unsafe { q.GetEvent(dwflags.0) }
    }
    fn BeginGetEvent(
        &self,
        pcallback: Ref<'_, IMFAsyncCallback>,
        punkstate: Ref<'_, IUnknown>,
    ) -> Result<()> {
        unsafe {
            self.queue
                .BeginGetEvent(pcallback.as_ref(), punkstate.as_ref())
        }
    }
    fn EndGetEvent(&self, presult: Ref<'_, IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(presult.as_ref()) }
    }
    fn QueueEvent(
        &self,
        met: u32,
        guidextendedtype: *const GUID,
        hrstatus: HRESULT,
        pvvalue: *const PROPVARIANT,
    ) -> Result<()> {
        unsafe {
            self.queue
                .QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue)
        }
    }
}

impl IMFMediaSource_Impl for Source_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.check()?;
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        self.check()?;
        unsafe { self.pd.Clone() }
    }

    fn Start(
        &self,
        ppresentationdescriptor: Ref<'_, IMFPresentationDescriptor>,
        pguidtimeformat: *const GUID,
        pvarstartposition: *const PROPVARIANT,
    ) -> Result<()> {
        let stream = self.check()?;
        unsafe {
            if !pguidtimeformat.is_null() && *pguidtimeformat != GUID::zeroed() {
                return Err(Error::from(MF_E_UNSUPPORTED_TIME_FORMAT));
            }
            let pd = ppresentationdescriptor.ok()?;
            let mut selected = BOOL(0);
            let mut sd = None;
            pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd)?;
            if selected.as_bool() {
                let unk: IUnknown = stream.to_interface();
                let met = if self.announced.swap(true, Ordering::SeqCst) {
                    MEUpdatedStream
                } else {
                    MENewStream
                };
                self.queue
                    .QueueEventParamUnk(met.0 as u32, &GUID::zeroed(), S_OK, &unk)?;
                stream.start(pvarstartposition)?;
            } else {
                stream.stop()?;
            }
            self.queue.QueueEventParamVar(
                MESourceStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
                pvarstartposition,
            )
        }
    }

    fn Stop(&self) -> Result<()> {
        let stream = self.check()?;
        stream.stop()?;
        unsafe {
            self.queue.QueueEventParamVar(
                MESourceStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
                std::ptr::null(),
            )
        }
    }

    fn Pause(&self) -> Result<()> {
        self.check()?;
        Err(Error::from(MF_E_INVALID_STATE_TRANSITION))
    }

    fn Shutdown(&self) -> Result<()> {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(s) = self.stream.lock().ok().and_then(|mut g| g.take()) {
            s.close();
        }
        unsafe { self.queue.Shutdown() }
    }
}

impl IMFMediaSourceEx_Impl for Source_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        self.check()?;
        Ok(self.attrs.clone())
    }
    fn GetStreamAttributes(&self, dwstreamidentifier: u32) -> Result<IMFAttributes> {
        let s = self.check()?;
        if dwstreamidentifier != 0 {
            return Err(Error::from(MF_E_INVALIDSTREAMNUMBER));
        }
        Ok(s.attrs.clone())
    }
    fn SetD3DManager(&self, _pmanager: Ref<'_, IUnknown>) -> Result<()> {
        // Bilder liegen im Arbeitsspeicher – kein Direct3D nötig
        self.check()?;
        Ok(())
    }
}

impl IMFGetService_Impl for Source_Impl {
    fn GetService(
        &self,
        _guidservice: *const GUID,
        _riid: *const GUID,
        _ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        Err(Error::from(MF_E_UNSUPPORTED_SERVICE))
    }
}

fn not_found() -> Error {
    Error::from(HRESULT::from_win32(ERROR_SET_NOT_FOUND.0))
}

impl IKsControl_Impl for Source_Impl {
    // keine Kamera-Einstellungen (Zoom, Fokus …) – wie eine einfache Webcam
    fn KsProperty(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        Err(not_found())
    }
    fn KsMethod(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        Err(not_found())
    }
    fn KsEvent(
        &self,
        _p: *const KSIDENTIFIER,
        _pl: u32,
        _d: *mut c_void,
        _dl: u32,
        _r: *mut u32,
    ) -> Result<()> {
        Err(not_found())
    }
}

// --- IMFActivate (das legt der Kameradienst über die CLSID an) ----------------------

#[implement(IMFActivate)]
struct Activator {
    attrs: IMFAttributes,
    source: Mutex<Option<IMFMediaSourceEx>>,
}

impl Activator {
    fn create() -> Result<IMFActivate> {
        let mut attrs = None;
        unsafe { MFCreateAttributes(&mut attrs, 4)? };
        Ok(Activator {
            attrs: attrs.ok_or_else(|| Error::from(E_OUTOFMEMORY))?,
            source: Mutex::new(None),
        }
        .into())
    }
}

impl IMFActivate_Impl for Activator_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        let mut g = self.source.lock().map_err(|_| Error::from(E_FAIL))?;
        if g.is_none() {
            *g = Some(Source::create().inspect_err(|e| log(&format!("Quelle anlegen: {e}")))?);
        }
        let src = g.as_ref().ok_or_else(|| Error::from(E_FAIL))?;
        unsafe { src.query(riid, ppv).ok() }
    }
    fn ShutdownObject(&self) -> Result<()> {
        if let Some(s) = self.source.lock().ok().and_then(|mut g| g.take()) {
            unsafe {
                let _ = s.Shutdown();
            }
        }
        Ok(())
    }
    fn DetachObject(&self) -> Result<()> {
        if let Ok(mut g) = self.source.lock() {
            *g = None;
        }
        Ok(())
    }
}

/// IMFActivate erbt IMFAttributes – alles an den eigenen Attributspeicher weiterreichen.
impl IMFAttributes_Impl for Activator_Impl {
    fn GetItem(&self, k: *const GUID, v: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.GetItem(k, Some(v)) }
    }
    fn GetItemType(&self, k: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.attrs.GetItemType(k) }
    }
    fn CompareItem(&self, k: *const GUID, v: *const PROPVARIANT) -> Result<BOOL> {
        unsafe { self.attrs.CompareItem(k, v) }
    }
    fn Compare(&self, theirs: Ref<'_, IMFAttributes>, t: MF_ATTRIBUTES_MATCH_TYPE) -> Result<BOOL> {
        unsafe { self.attrs.Compare(theirs.as_ref(), t) }
    }
    fn GetUINT32(&self, k: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetUINT32(k) }
    }
    fn GetUINT64(&self, k: *const GUID) -> Result<u64> {
        unsafe { self.attrs.GetUINT64(k) }
    }
    fn GetDouble(&self, k: *const GUID) -> Result<f64> {
        unsafe { self.attrs.GetDouble(k) }
    }
    fn GetGUID(&self, k: *const GUID) -> Result<GUID> {
        unsafe { self.attrs.GetGUID(k) }
    }
    fn GetStringLength(&self, k: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetStringLength(k) }
    }
    fn GetString(&self, k: *const GUID, v: PWSTR, cch: u32, len: *mut u32) -> Result<()> {
        unsafe {
            let buf = std::slice::from_raw_parts_mut(v.0, cch as usize);
            self.attrs.GetString(k, buf, Some(len))
        }
    }
    fn GetAllocatedString(&self, k: *const GUID, v: *mut PWSTR, len: *mut u32) -> Result<()> {
        unsafe { self.attrs.GetAllocatedString(k, v, len) }
    }
    fn GetBlobSize(&self, k: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetBlobSize(k) }
    }
    fn GetBlob(&self, k: *const GUID, buf: *mut u8, size: u32, got: *mut u32) -> Result<()> {
        unsafe {
            let b = std::slice::from_raw_parts_mut(buf, size as usize);
            self.attrs.GetBlob(k, b, Some(got))
        }
    }
    fn GetAllocatedBlob(&self, k: *const GUID, buf: *mut *mut u8, size: *mut u32) -> Result<()> {
        unsafe { self.attrs.GetAllocatedBlob(k, buf, size) }
    }
    fn GetUnknown(&self, k: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        unsafe {
            let vt = Interface::vtable(&self.attrs);
            (vt.GetUnknown)(Interface::as_raw(&self.attrs), k, riid, ppv).ok()
        }
    }
    fn SetItem(&self, k: *const GUID, v: *const PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.SetItem(k, v) }
    }
    fn DeleteItem(&self, k: *const GUID) -> Result<()> {
        unsafe { self.attrs.DeleteItem(k) }
    }
    fn DeleteAllItems(&self) -> Result<()> {
        unsafe { self.attrs.DeleteAllItems() }
    }
    fn SetUINT32(&self, k: *const GUID, v: u32) -> Result<()> {
        unsafe { self.attrs.SetUINT32(k, v) }
    }
    fn SetUINT64(&self, k: *const GUID, v: u64) -> Result<()> {
        unsafe { self.attrs.SetUINT64(k, v) }
    }
    fn SetDouble(&self, k: *const GUID, v: f64) -> Result<()> {
        unsafe { self.attrs.SetDouble(k, v) }
    }
    fn SetGUID(&self, k: *const GUID, v: *const GUID) -> Result<()> {
        unsafe { self.attrs.SetGUID(k, v) }
    }
    fn SetString(&self, k: *const GUID, v: &PCWSTR) -> Result<()> {
        unsafe { self.attrs.SetString(k, *v) }
    }
    fn SetBlob(&self, k: *const GUID, buf: *const u8, size: u32) -> Result<()> {
        unsafe {
            let b = std::slice::from_raw_parts(buf, size as usize);
            self.attrs.SetBlob(k, b)
        }
    }
    fn SetUnknown(&self, k: *const GUID, u: Ref<'_, IUnknown>) -> Result<()> {
        unsafe { self.attrs.SetUnknown(k, u.as_ref()) }
    }
    fn LockStore(&self) -> Result<()> {
        unsafe { self.attrs.LockStore() }
    }
    fn UnlockStore(&self) -> Result<()> {
        unsafe { self.attrs.UnlockStore() }
    }
    fn GetCount(&self) -> Result<u32> {
        unsafe { self.attrs.GetCount() }
    }
    fn GetItemByIndex(&self, i: u32, k: *mut GUID, v: *mut PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.GetItemByIndex(i, k, Some(v)) }
    }
    fn CopyAllItems(&self, dest: Ref<'_, IMFAttributes>) -> Result<()> {
        unsafe { self.attrs.CopyAllItems(dest.as_ref()) }
    }
}

// --- COM-Einstieg ------------------------------------------------------------------

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> Result<()> {
        if outer.is_some() {
            return Err(Error::from(CLASS_E_NOAGGREGATION));
        }
        let a = Activator::create()?;
        unsafe { a.query(riid, ppv).ok() }
    }
    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// # Safety
/// Von COM aufgerufen.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();
    if *rclsid != CLSID_USBCD_CAM {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let f: IClassFactory = Factory.into();
    f.query(riid, ppv)
}

/// Die DLL bleibt geladen, solange der Kameradienst läuft.
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

// --- für USBCD.exe: Kamera anlegen und wieder entfernen ------------------------------

/// Legt die virtuelle Kamera für den angemeldeten Nutzer an (verschwindet spätestens,
/// wenn USBCD beendet wird). `out` erhält einen Zeiger für `UsbcdCamStop`.
///
/// # Safety
/// `name` muss ein nullterminierter UTF-16-Text sein, `out` gültig.
#[no_mangle]
pub unsafe extern "system" fn UsbcdCamStart(name: PCWSTR, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || name.is_null() {
        return E_POINTER;
    }
    *out = std::ptr::null_mut();
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let r = (|| -> Result<IMFVirtualCamera> {
        MFStartup(MF_VERSION, MFSTARTUP_LITE)?;
        let id: Vec<u16> = CLSID_USBCD_CAM_STR.encode_utf16().chain(Some(0)).collect();
        let cam = MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            name,
            PCWSTR(id.as_ptr()),
            None,
        )?;
        cam.Start(None)?;
        Ok(cam)
    })();
    match r {
        Ok(cam) => {
            *out = cam.into_raw();
            S_OK
        }
        Err(e) => e.code(),
    }
}

/// Entfernt die mit `UsbcdCamStart` angelegte Kamera.
///
/// # Safety
/// `cam` muss von `UsbcdCamStart` stammen und darf danach nicht mehr benutzt werden.
#[no_mangle]
pub unsafe extern "system" fn UsbcdCamStop(cam: *mut c_void) {
    if cam.is_null() {
        return;
    }
    let cam = IMFVirtualCamera::from_raw(cam);
    let _ = cam.Stop();
    let _ = cam.Remove();
    let _ = cam.Shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = header();
        assert!(header_ok(&h));
        let mut bad = h;
        bad[4] = 0;
        assert!(!header_ok(&bad));
    }

    #[test]
    fn black_is_nv12() {
        let f = black_frame();
        assert_eq!(f.len(), FRAME_LEN);
        assert_eq!(f[0], 16);
        assert_eq!(f[FRAME_LEN - 1], 128);
    }

    #[test]
    fn clsid_string_matches() {
        assert_eq!(format!("{{{:?}}}", CLSID_USBCD_CAM), CLSID_USBCD_CAM_STR);
    }
}
