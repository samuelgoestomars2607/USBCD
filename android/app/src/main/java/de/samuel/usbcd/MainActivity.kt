package de.samuel.usbcd

import android.Manifest
import android.app.Activity
import android.content.res.Configuration
import android.hardware.display.DisplayManager
import android.graphics.Color
import android.graphics.Outline
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Display
import android.view.Gravity
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.view.ViewOutlineProvider
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Vollbild-Anzeige für den PC-Bildschirm.
 * - wählt den höchsten Display-Modus (Auflösung, dann Bildrate)
 * - setzt die Helligkeit (Standard 100 %, vom PC aus änderbar)
 * - meldet dem PC Auflösung, Bildrate und Decoder-Fähigkeiten
 */
class MainActivity : Activity(), SurfaceHolder.Callback {
    private lateinit var surfaceView: SurfaceView
    /** verlustfreies Standbild vom PC, liegt über dem Video bis zum nächsten Videobild */
    private lateinit var stillView: ImageView
    /** Entpacken der Standbilder – nie im UI- oder Lese-Thread */
    private val stillDecode = java.util.concurrent.Executors.newSingleThreadExecutor()
    private lateinit var overlay: FrameLayout
    private lateinit var statusView: TextView
    private lateinit var detailView: TextView
    private lateinit var mode: Display.Mode
    private var server: StreamServer? = null
    private var decoder: VideoDecoder? = null
    private var audio: AudioPlayer? = null
    private var input: InputForwarder? = null
    private var camera: TabletCamera? = null
    /** Drehung des Displays in Grad – für die Webcam (Bild aufrecht halten) */
    @Volatile private var rotationDeg = 0
    private lateinit var statsView: TextView
    private val ui = Handler(Looper.getMainLooper())
    /** Videobild ist sichtbar (kein Warte-/Verbindungsbildschirm) */
    @Volatile private var showingVideo = false
    /** Verbindung zum PC steht (Monitorbetrieb) */
    @Volatile private var streaming = false
    /** Lautstärketasten an den PC geben (Schalter im USBCD-Menü, nur mit Ton) */
    @Volatile private var volumeKeys = false
    /** Sendet Lautstärketasten – nie im UI-Thread */
    private val keyOut = java.util.concurrent.Executors.newSingleThreadExecutor()
    private lateinit var volumePill: VolumePill
    private lateinit var dnd: TabletDnd

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Auth.takeFrom(intent)
        // Rest eines abgebrochenen Laufs zurücknehmen; der PC schaltet bei Bedarf neu ein
        dnd = TabletDnd(this).also { it.release() }

        @Suppress("DEPRECATION")
        val disp: Display = if (Build.VERSION.SDK_INT >= 30) display!! else windowManager.defaultDisplay
        mode = DisplayInfo.bestMode(disp)
        applyWindowSettings()

        val (w, h) = DisplayInfo.landscapeSize(mode)
        val decoders = mapOf(
            "hevc" to DisplayInfo.findDecoder(DisplayInfo.MIME_HEVC, w, h),
            "avc" to DisplayInfo.findDecoder(DisplayInfo.MIME_AVC, w, h),
        )
        val modes = DisplayInfo.decoderModes(w, h)
        Log.i(TAG, "Modus ${mode.modeId}: ${w}x$h @ ${mode.refreshRate} Hz, Decoder: $decoders, Stufen: $modes")

        surfaceView = SurfaceView(this)
        surfaceView.holder.setFixedSize(w, h)
        surfaceView.holder.addCallback(this)

        val root = FrameLayout(this)
        root.setBackgroundColor(Color.BLACK)
        root.addView(surfaceView, FrameLayout.LayoutParams(-1, -1))
        stillView = ImageView(this).apply {
            scaleType = ImageView.ScaleType.FIT_XY
            visibility = View.GONE
        }
        root.addView(stillView, FrameLayout.LayoutParams(-1, -1))
        root.addView(buildOverlay(), FrameLayout.LayoutParams(-1, -1))
        statsView = TextView(this).apply {
            typeface = Fonts.get(this@MainActivity, 500)
            textSize = 13f
            setTextColor(Color.WHITE)
            setLineSpacing(0f, 1.2f)
            setPadding(dp(12), dp(8), dp(12), dp(8))
            background = android.graphics.drawable.GradientDrawable().apply {
                cornerRadius = dp(10).toFloat()
                setColor(Color.argb(150, 0, 0, 0))
            }
            visibility = View.GONE
        }
        root.addView(statsView, FrameLayout.LayoutParams(-2, -2, Gravity.TOP or Gravity.START).apply {
            leftMargin = dp(16); topMargin = dp(16)
        })
        volumePill = VolumePill(this).apply { visibility = View.GONE }
        root.addView(volumePill, FrameLayout.LayoutParams(dp(280), dp(64), Gravity.TOP or Gravity.CENTER_HORIZONTAL).apply {
            topMargin = dp(24)
        })
        setContentView(root)
        showWaiting()

        val dec = VideoDecoder(decoders, modes.map { it.name }.toSet()) { runOnUiThread { hideOverlay() } }
        dec.reporter = { type, payload -> server?.send(type, payload) }
        val aud = AudioPlayer(this)
        decoder = dec
        audio = aud
        val appVersion = try {
            packageManager.getPackageInfo(packageName, 0).longVersionCode
        } catch (e: Exception) {
            0L
        }
        val hello = DisplayInfo.helloJson(
            mode, resources.displayMetrics.densityDpi, decoders, DisplayInfo.deviceName(this), modes, appVersion,
        )
        server = StreamServer(
            hello, dec, aud,
            onState = { connected, info ->
                runOnUiThread {
                    streaming = connected
                    dnd.setActive(connected)
                    if (connected) {
                        unsupportedShown = false
                        showConnecting(info)
                    } else if (!unsupportedShown) {
                        showWaiting() // der Hinweis „nicht unterstützt“ bleibt stehen
                    }
                }
            },
            onSettings = { s -> runOnUiThread { applySettings(s) } },
            onBlocked = { blocked -> runOnUiThread { if (blocked) showBlocked() else showWaiting() } },
            onCamera = { j -> camera?.control(j) },
            onVolume = { j -> runOnUiThread { showVolume(j) } },
            onUnsupported = { runOnUiThread { showUnsupported() } },
            onStill = { data, gen -> showStill(data, gen) },
            onStillHide = { runOnUiThread { hideStill() } },
        ).also { it.start() }
        updateRotation()
        getSystemService(DisplayManager::class.java)?.registerDisplayListener(displayListener, ui)
        // Webcam: nur die Selfie-Kamera, nur solange der PC sie anfordert
        camera = TabletCamera(
            this,
            send = { type, payload -> server?.send(type, payload) ?: false },
            askPermission = { runOnUiThread { requestPermissions(arrayOf(Manifest.permission.CAMERA), REQ_CAMERA) } },
            displayRotation = { rotationDeg },
        )
        input = InputForwarder { type, payload -> server?.send(type, payload) ?: false }
    }

    // --- Touch und S Pen → PC ---------------------------------------------------

    private fun forward(ev: MotionEvent): Boolean {
        if (!showingVideo) return false
        val v = window.decorView
        return input?.handle(ev, v.width, v.height) == true
    }

    override fun dispatchTouchEvent(ev: MotionEvent): Boolean =
        forward(ev) || super.dispatchTouchEvent(ev)

    /** Schwebender Stift (Hover) kommt als „generisches“ Ereignis. */
    override fun dispatchGenericMotionEvent(ev: MotionEvent): Boolean =
        forward(ev) || super.dispatchGenericMotionEvent(ev)

    // --- Lautstärketasten → Windows-Lautstärke des Tablet-Lautsprechers ------------

    override fun dispatchKeyEvent(ev: KeyEvent): Boolean {
        val key = when (ev.keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP -> 1
            KeyEvent.KEYCODE_VOLUME_DOWN -> 2
            KeyEvent.KEYCODE_VOLUME_MUTE -> 3
            else -> return super.dispatchKeyEvent(ev)
        }
        // ohne Verbindung oder ausgeschaltet: normale Android-Lautstärke
        if (!volumeKeys || !streaming) return super.dispatchKeyEvent(ev)
        // Gedrückthalten wiederholt lauter/leiser, Stumm nur einmal
        if (ev.action == KeyEvent.ACTION_DOWN && !(key == 3 && ev.repeatCount > 0)) {
            keyOut.execute { server?.send(Proto.T_VOLUME_KEY, byteArrayOf(key.toByte())) }
        }
        return true
    }

    private val hidePill = Runnable {
        volumePill.animate().alpha(0f).setDuration(200).withEndAction { volumePill.visibility = View.GONE }.start()
    }

    private fun showVolume(j: org.json.JSONObject) {
        volumePill.show(j.optInt("level", 0), j.optBoolean("muted", false))
        volumePill.animate().cancel()
        volumePill.alpha = 1f
        volumePill.visibility = View.VISIBLE
        ui.removeCallbacks(hidePill)
        ui.postDelayed(hidePill, 1500)
    }

    // --- Verlustfreie Standbilder ----------------------------------------------

    /**
     * Steht das Bild am PC still, schickt USBCD ein pixelgenaues Bild (exakte Farben, volle
     * Farbauflösung). Es wird nur gezeigt, wenn es genau die Auflösung des Displays hat
     * (1:1 aufs Panel) und seitdem kein neues Videobild kam.
     */
    private fun showStill(data: ByteArray, gen: Long) {
        try {
            stillDecode.execute {
                val bm = decodeStill(data) ?: return@execute
                runOnUiThread {
                    val srv = server
                    val (w, h) = DisplayInfo.landscapeSize(mode)
                    if (srv == null || !showingVideo || bm.width != w || bm.height != h) {
                        bm.recycle()
                        return@runOnUiThread
                    }
                    // erst als sichtbar markieren, dann prüfen: kommt gleichzeitig ein
                    // Videobild, wird das Standbild sicher wieder entfernt
                    srv.stillVisible.set(true)
                    if (srv.videoGen != gen) {
                        srv.stillVisible.set(false)
                        bm.recycle()
                        return@runOnUiThread
                    }
                    stillView.setImageBitmap(bm)
                    stillView.visibility = View.VISIBLE
                }
            }
        } catch (_: Exception) {
        }
    }

    private fun hideStill() {
        if (stillView.visibility != View.GONE) {
            stillView.visibility = View.GONE
            stillView.setImageDrawable(null)
        }
    }

    /** [Breite u32][Höhe u32][Format u8][zlib-RGBA] → Bitmap; null bei Unstimmigkeiten. */
    private fun decodeStill(data: ByteArray): android.graphics.Bitmap? {
        if (data.size < 10) return null
        val head = java.nio.ByteBuffer.wrap(data, 0, 9)
        val w = head.int
        val h = head.int
        if (head.get().toInt() != Proto.STILL_RGBA_ZLIB) return null
        if (w !in 1..8192 || h !in 1..8192) return null
        val size = w.toLong() * h * 4
        if (size > Proto.MAX_FRAME) return null
        val px = ByteArray(size.toInt())
        val inf = java.util.zip.Inflater()
        try {
            inf.setInput(data, 9, data.size - 9)
            var off = 0
            while (off < px.size && !inf.finished()) {
                val n = inf.inflate(px, off, px.size - off)
                if (n == 0 && (inf.needsInput() || inf.needsDictionary())) break
                off += n
            }
            if (off != px.size) return null
        } catch (e: Exception) {
            Log.w(TAG, "Standbild fehlerhaft", e)
            return null
        } finally {
            inf.end()
        }
        return android.graphics.Bitmap.createBitmap(w, h, android.graphics.Bitmap.Config.ARGB_8888).apply {
            copyPixelsFromBuffer(java.nio.ByteBuffer.wrap(px))
        }
    }

    // --- Info-Anzeige ---------------------------------------------------------

    private val statsTick = object : Runnable {
        override fun run() {
            val s = Stats.take()
            statsView.text = getString(R.string.stats_format, s.fps, s.mbit, s.decodeMs, s.usbMs, s.stream)
            ui.postDelayed(this, 1000)
        }
    }

    private fun showStats(on: Boolean) {
        ui.removeCallbacks(statsTick)
        statsView.visibility = if (on) View.VISIBLE else View.GONE
        if (on) {
            Stats.take() // Zähler zurücksetzen
            ui.postDelayed(statsTick, 1000)
            statsView.text = getString(R.string.stats_wait)
        }
    }

    // --- Webcam ------------------------------------------------------------------

    private fun updateRotation() {
        @Suppress("DEPRECATION")
        val d: Display? = if (Build.VERSION.SDK_INT >= 30) display else windowManager.defaultDisplay
        val r = (d?.rotation ?: Surface.ROTATION_0) * 90
        if (r != rotationDeg) {
            rotationDeg = r
            camera?.onRotationChanged()
        }
    }

    /**
     * Drehung um 180° (Querformat ↔ umgedrehtes Querformat) meldet Android nicht als
     * Konfigurationsänderung – nur über den DisplayListener.
     */
    private val displayListener = object : DisplayManager.DisplayListener {
        override fun onDisplayAdded(displayId: Int) {}
        override fun onDisplayRemoved(displayId: Int) {}
        override fun onDisplayChanged(displayId: Int) {
            runOnUiThread { updateRotation() }
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        updateRotation()
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQ_CAMERA) {
            camera?.onPermissionResult(grantResults.firstOrNull() == android.content.pm.PackageManager.PERMISSION_GRANTED)
        }
    }

    override fun onStart() {
        super.onStart()
        camera?.setForeground(true)
    }

    override fun onStop() {
        // im Hintergrund sperrt Android die Kamera – sauber beenden
        camera?.setForeground(false)
        super.onStop()
    }

    /** Vom PC erneut gestartet (App lief schon): neues Sitzungsgeheimnis übernehmen. */
    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        Auth.takeFrom(intent)
    }

    // --- Wartebildschirm ---------------------------------------------------------

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun text(weight: Int, sizeSp: Float, color: Int) = TextView(this).apply {
        typeface = Fonts.get(this@MainActivity, weight)
        textSize = sizeSp
        setTextColor(color)
        gravity = Gravity.CENTER
        includeFontPadding = false
    }

    /** Animierter Verlauf mit Logo und Statustext – liegt über dem Videobild. */
    private fun buildOverlay(): View {
        val primary = getColor(R.color.text_primary)
        val secondary = getColor(R.color.text_secondary)
        overlay = FrameLayout(this)
        overlay.addView(GradientBackground(this), FrameLayout.LayoutParams(-1, -1))

        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
        }
        val logo = ImageView(this).apply {
            setImageResource(R.drawable.logo)
            outlineProvider = object : ViewOutlineProvider() {
                override fun getOutline(view: View, outline: Outline) {
                    outline.setRoundRect(0, 0, view.width, view.height, view.width * 0.22f)
                }
            }
            clipToOutline = true
            elevation = dp(6).toFloat()
        }
        column.addView(logo, LinearLayout.LayoutParams(dp(112), dp(112)).apply { bottomMargin = dp(28) })
        column.addView(text(700, 40f, primary).apply { text = "USBCD" })
        statusView = text(500, 22f, primary)
        column.addView(statusView, LinearLayout.LayoutParams(-2, -2).apply { topMargin = dp(18) })
        detailView = text(400, 16f, secondary).apply { setLineSpacing(0f, 1.25f) }
        column.addView(detailView, LinearLayout.LayoutParams(-2, -2).apply { topMargin = dp(10) })

        overlay.addView(column, FrameLayout.LayoutParams(-2, -2, Gravity.CENTER).apply {
            leftMargin = dp(32); rightMargin = dp(32)
        })
        return overlay
    }

    private fun showOverlay() {
        showingVideo = false
        hideStill()
        overlay.animate().cancel()
        overlay.alpha = 1f
        overlay.visibility = View.VISIBLE
    }

    private fun hideOverlay() {
        showingVideo = true
        overlay.animate().alpha(0f).setDuration(250).withEndAction { overlay.visibility = View.GONE }.start()
    }

    private fun showWaiting() {
        val (w, h) = DisplayInfo.landscapeSize(mode)
        showOverlay()
        statusView.text = getString(R.string.waiting)
        detailView.text = getString(R.string.waiting_detail, w, h, "%.0f".format(mode.refreshRate))
    }

    private fun showBlocked() {
        showOverlay()
        statusView.text = getString(R.string.port_busy)
        detailView.text = getString(R.string.port_busy_detail)
    }

    /** Hinweis „Bild nicht unterstützt“ steht (bleibt auch nach dem Trennen) */
    private var unsupportedShown = false

    /** Kein Decoder dieses Tablets kann das Bild anzeigen – klar sagen statt schwarzem Bild. */
    private fun showUnsupported() {
        unsupportedShown = true
        showOverlay()
        statusView.text = getString(R.string.unsupported)
        detailView.text = getString(R.string.unsupported_detail)
    }

    private fun showConnecting(info: String) {
        showOverlay()
        statusView.text = getString(R.string.connected)
        detailView.text = getString(R.string.starting, info)
    }

    /** Einstellungen vom PC (Taskleisten-Menü), z. B. Helligkeit. */
    private fun applySettings(s: org.json.JSONObject) {
        if (s.has("brightness")) {
            val b = s.getDouble("brightness").toFloat().coerceIn(0.05f, 1f)
            getSharedPreferences("usbcd", MODE_PRIVATE).edit().putFloat("brightness", b).apply()
            applyWindowSettings()
        }
        if (s.has("input")) input?.enabled = s.getBoolean("input")
        if (s.has("stats")) showStats(s.getBoolean("stats"))
        if (s.has("volume_keys")) volumeKeys = s.getBoolean("volume_keys")
        if (s.has("dnd")) dnd.setWanted(s.getBoolean("dnd"))
    }

    /** Modus, Helligkeit, Bildschirm-an, Vollbild. */
    private fun applyWindowSettings() {
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        // Beim Anstecken auch über dem Sperrbildschirm erscheinen und Display einschalten
        if (Build.VERSION.SDK_INT >= 27) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            @Suppress("DEPRECATION")
            window.addFlags(
                WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON
            )
        }
        val lp = window.attributes
        lp.preferredDisplayModeId = mode.modeId
        lp.preferredRefreshRate = mode.refreshRate
        // Standard 100 %, über das USBCD-Menü am PC änderbar
        lp.screenBrightness = getSharedPreferences("usbcd", MODE_PRIVATE).getFloat("brightness", 1f)
        if (Build.VERSION.SDK_INT >= 30) {
            lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        } else if (Build.VERSION.SDK_INT >= 28) {
            lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
        }
        window.attributes = lp
    }

    private fun hideSystemBars() {
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false)
            window.insetsController?.let {
                it.hide(WindowInsets.Type.systemBars())
                it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            }
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility = (View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                or View.SYSTEM_UI_FLAG_FULLSCREEN
                or View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                or View.SYSTEM_UI_FLAG_LAYOUT_STABLE)
        }
    }

    override fun onResume() {
        super.onResume()
        applyWindowSettings()
        hideSystemBars()
        dnd.onResume()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }

    override fun onDestroy() {
        ui.removeCallbacks(statsTick)
        ui.removeCallbacks(hidePill)
        dnd.release()
        keyOut.shutdown()
        stillDecode.shutdown()
        server?.stop()
        server = null
        getSystemService(DisplayManager::class.java)?.unregisterDisplayListener(displayListener)
        camera?.release()
        camera = null
        super.onDestroy()
    }

    // --- Surface ---

    override fun surfaceCreated(holder: SurfaceHolder) {
        requestFrameRate(holder.surface)
        decoder?.setSurface(holder.surface)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        decoder?.setSurface(null)
    }

    /** Verhindert, dass "adaptive" Displays bei Video auf 60 Hz herunterschalten. */
    private fun requestFrameRate(s: Surface) {
        try {
            if (Build.VERSION.SDK_INT >= 31) {
                s.setFrameRate(
                    mode.refreshRate,
                    Surface.FRAME_RATE_COMPATIBILITY_DEFAULT,
                    Surface.CHANGE_FRAME_RATE_ALWAYS
                )
            } else if (Build.VERSION.SDK_INT >= 30) {
                s.setFrameRate(mode.refreshRate, Surface.FRAME_RATE_COMPATIBILITY_DEFAULT)
            }
        } catch (e: Exception) {
            Log.w(TAG, "setFrameRate", e)
        }
    }

    companion object {
        private const val TAG = "USBCD"
        private const val REQ_CAMERA = 7
    }
}
