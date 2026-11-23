package de.samuel.usbcd

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.util.Range
import android.util.Size
import android.view.Surface
import org.json.JSONObject
import kotlin.math.abs

/**
 * Webcam für den PC: nimmt NUR die Frontkamera (Selfie) auf und schickt sie als H.264
 * zum PC (host/src/camera.rs). Läuft nur, solange der PC das verlangt – also während
 * eine App am PC die Webcam benutzt. Die Rückkamera wird nie geöffnet.
 *
 * Alles läuft im eigenen Thread „camera“.
 */
class TabletCamera(
    private val ctx: Context,
    private val send: (Int, ByteArray) -> Boolean,
    /** im UI-Thread: Android-Abfrage „Kamerazugriff erlauben?“ zeigen */
    private val askPermission: () -> Unit,
    /** aktuelle Drehung des Displays in Grad (0, 90, 180, 270) */
    private val displayRotation: () -> Int,
) {
    private val thread = HandlerThread("camera").apply { start() }
    private val h = Handler(thread.looper)

    // vom PC gewünscht
    private var enabled = false
    private var wantOn = false
    private var reqW = 1280
    private var reqH = 720
    private var reqFps = 30
    private var keyCounter = -1L
    private var asked = false
    private var waitingForAnswer = false
    /** Activity im Vordergrund (sonst sperrt Android die Kamera ohnehin) */
    private var foreground = true

    // laufende Aufnahme
    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var encoder: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var drain: Thread? = null
    @Volatile private var running = false
    private var opening = false
    private var sensorOrientation = 0
    private var size = Size(1280, 720)
    private var lastRotation = -1

    private fun granted() =
        ctx.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

    /** T_CAM vom PC; null = Verbindung beendet. */
    fun control(j: JSONObject?) = h.post {
        if (j == null) {
            enabled = false; wantOn = false; asked = false; keyCounter = -1
            stopCapture()
            return@post
        }
        enabled = j.optBoolean("enabled", false)
        wantOn = enabled && j.optBoolean("on", false)
        reqW = j.optInt("width", 1280).coerceIn(160, 1920)
        reqH = j.optInt("height", 720).coerceIn(120, 1080)
        reqFps = j.optInt("fps", 30).coerceIn(10, 30)
        val key = j.optLong("key", 0)
        val keyChanged = keyCounter >= 0 && key != keyCounter
        keyCounter = key
        apply()
        if (keyChanged) requestSync()
    }

    /** Antwort auf die Android-Abfrage. */
    fun onPermissionResult(ok: Boolean) = h.post {
        waitingForAnswer = false
        if (!ok) report("denied")
        apply()
    }

    fun setForeground(fg: Boolean) = h.post {
        foreground = fg
        apply()
    }

    /** Display gedreht: neue Drehung melden, damit der PC das Bild aufrecht hält. */
    fun onRotationChanged() = h.post {
        if (running && rotation() != lastRotation) {
            reportOn()
            requestSync()
        }
    }

    fun release() = h.post {
        stopCapture()
        thread.quitSafely()
    }

    private fun apply() {
        if (!enabled) {
            stopCapture()
            return
        }
        if (!granted()) {
            stopCapture()
            // einmal je Verbindung fragen (sobald die Webcam am PC eingeschaltet ist,
            // nicht erst mitten im Anruf); danach nur noch melden
            if (!asked && foreground) {
                asked = true
                waitingForAnswer = true
                report("asking")
                askPermission()
            } else if (!waitingForAnswer && wantOn) {
                report("denied")
            }
            return
        }
        if (wantOn && foreground) startCapture() else stopCapture()
    }

    private fun rotation() = (sensorOrientation + displayRotation()) % 360

    private fun report(state: String, extra: JSONObject.() -> Unit = {}) {
        val j = JSONObject().put("state", state).apply(extra)
        send(Proto.T_CAM_STATE, j.toString().toByteArray())
    }

    private fun reportOn() {
        lastRotation = rotation()
        report("on") {
            put("rotation", lastRotation)
            put("width", size.width)
            put("height", size.height)
        }
    }

    private fun requestSync() {
        val e = encoder ?: return
        try {
            e.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
        } catch (ex: Exception) {
            Log.w(TAG, "Schlüsselbild anfordern", ex)
        }
    }

    // --- Aufnahme ---------------------------------------------------------------

    @SuppressLint("MissingPermission") // granted() wurde vorher geprüft
    private fun startCapture() {
        if (running || opening) return
        val mgr = ctx.getSystemService(CameraManager::class.java) ?: return report("error")
        // nur die Frontkamera – nie die Rückkamera
        val id = try {
            mgr.cameraIdList.firstOrNull {
                mgr.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) ==
                    CameraCharacteristics.LENS_FACING_FRONT
            }
        } catch (e: Exception) {
            Log.w(TAG, "Kameraliste", e); null
        } ?: return report("none")
        try {
            val ch = mgr.getCameraCharacteristics(id)
            sensorOrientation = ch.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 0
            val map = ch.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            val sizes = map?.getOutputSizes(MediaCodec::class.java)
                ?: map?.getOutputSizes(SurfaceTexture::class.java) ?: emptyArray()
            size = chooseSize(sizes, reqW, reqH)
            val range = chooseRange(ch.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES), reqFps)
            val afModes = ch.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: IntArray(0)

            val enc = createEncoder(size, reqFps)
            val surface = enc.createInputSurface()
            enc.start()
            encoder = enc
            inputSurface = surface
            running = true
            drain = Thread({ drainLoop(enc) }, "camera-out").apply { start() }

            opening = true
            mgr.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(cam: CameraDevice) {
                    opening = false
                    if (!running) { cam.close(); return }
                    device = cam
                    @Suppress("DEPRECATION")
                    cam.createCaptureSession(listOf(surface), object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(s: CameraCaptureSession) {
                            if (!running) { s.close(); return }
                            session = s
                            try {
                                val req = cam.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
                                    addTarget(surface)
                                    set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
                                    if (range != null) set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range)
                                    if (CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO in afModes) {
                                        set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
                                    }
                                }
                                s.setRepeatingRequest(req.build(), null, h)
                                Log.i(TAG, "Selfie-Kamera an: ${size.width}x${size.height}, $range, Sensor $sensorOrientation°")
                                reportOn()
                            } catch (e: Exception) {
                                Log.w(TAG, "Aufnahme starten", e)
                                stopCapture(); report("error")
                            }
                        }

                        override fun onConfigureFailed(s: CameraCaptureSession) {
                            stopCapture(); report("error")
                        }
                    }, h)
                }

                override fun onDisconnected(cam: CameraDevice) {
                    // z. B. eine andere App hat die Kamera übernommen
                    opening = false
                    cam.close()
                    if (device === cam) device = null
                    stopCapture(); report("error")
                }

                override fun onError(cam: CameraDevice, error: Int) {
                    opening = false
                    Log.w(TAG, "Kamerafehler $error")
                    cam.close()
                    if (device === cam) device = null
                    stopCapture(); report("error")
                }
            }, h)
        } catch (e: Exception) {
            Log.w(TAG, "Kamera öffnen", e)
            opening = false
            stopCapture()
            report("error")
        }
    }

    private fun createEncoder(s: Size, fps: Int): MediaCodec {
        fun format(baseline: Boolean) = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, s.width, s.height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            // ~3,5 Mbit/s bei 720p30: scharf genug für Videoanrufe, über USB vernachlässigbar
            setInteger(MediaFormat.KEY_BIT_RATE, (s.width * s.height * fps / 8).coerceIn(1_500_000, 6_000_000))
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
            if (Build.VERSION.SDK_INT >= 29) {
                setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1)
                setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            }
            if (baseline) setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
        }
        val enc = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        try {
            enc.configure(format(true), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (e: Exception) {
            // manche Encoder lehnen ein festes Profil ab
            enc.reset()
            enc.configure(format(false), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        }
        return enc
    }

    private fun drainLoop(enc: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        try {
            while (running) {
                val i = enc.dequeueOutputBuffer(info, 100_000)
                if (i < 0) continue
                val buf = enc.getOutputBuffer(i)
                if (buf != null && info.size > 0) {
                    var flags = 0
                    if (info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0) flags = flags or 1
                    if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) flags = flags or 2
                    val out = ByteArray(1 + info.size)
                    out[0] = flags.toByte()
                    buf.position(info.offset)
                    buf.get(out, 1, info.size)
                    enc.releaseOutputBuffer(i, false)
                    if (!send(Proto.T_CAM_DATA, out)) {
                        h.post { stopCapture() } // Verbindung weg
                        break
                    }
                } else {
                    enc.releaseOutputBuffer(i, false)
                }
            }
        } catch (e: Exception) {
            if (running) Log.w(TAG, "Kamera-Encoder", e)
        }
    }

    private fun stopCapture() {
        val wasRunning = running || opening
        running = false
        runCatching { session?.close() }
        session = null
        runCatching { device?.close() }
        device = null
        drain?.let { runCatching { it.join(500) } }
        drain = null
        encoder?.let { runCatching { it.stop() }; runCatching { it.release() } }
        encoder = null
        inputSurface?.release()
        inputSurface = null
        lastRotation = -1
        if (wasRunning) {
            Log.i(TAG, "Selfie-Kamera aus")
            report("off")
        }
    }

    companion object {
        private const val TAG = "USBCD"

        /** Größe nahe der gewünschten, bevorzugt im selben Seitenverhältnis. */
        fun chooseSize(sizes: Array<Size>, w: Int, h: Int): Size {
            if (sizes.isEmpty()) return Size(w, h)
            val want = w.toDouble() / h
            val area = w.toLong() * h
            val usable = sizes.filter { it.width % 2 == 0 && it.height % 2 == 0 && it.width <= 1920 && it.height <= 1920 }
                .ifEmpty { sizes.toList() }
            val same = usable.filter { abs(it.width.toDouble() / it.height - want) < 0.02 }
            val pool = same.ifEmpty { usable }
            return pool.minByOrNull { abs(it.width.toLong() * it.height - area) } ?: Size(w, h)
        }

        /** Bildrate: oben genau `fps`, unten möglichst 15 (helleres Bild bei wenig Licht). */
        fun chooseRange(ranges: Array<Range<Int>>?, fps: Int): Range<Int>? {
            if (ranges.isNullOrEmpty()) return null
            val top = ranges.filter { it.upper == fps }
            if (top.isNotEmpty()) return top.minByOrNull { abs(it.lower - 15) }
            return ranges.filter { it.upper <= fps }.maxByOrNull { it.upper } ?: ranges.first()
        }
    }
}
