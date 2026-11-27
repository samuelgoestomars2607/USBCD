package de.samuel.usbcd

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.ColorMatrix
import android.graphics.ColorMatrixColorFilter
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.PorterDuff
import android.graphics.PorterDuffXfermode
import android.graphics.RadialGradient
import android.graphics.RectF
import android.graphics.Shader
import android.graphics.SurfaceTexture
import android.graphics.drawable.Drawable
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLSurface
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.Choreographer
import android.view.TextureView
import android.view.View
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import java.nio.ShortBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.exp
import kotlin.math.min
import kotlin.math.pow
import kotlin.math.sin
import kotlin.math.sqrt

/*
 * Ladebildschirm: Ein offener Schlauch läuft auf einem Kleeblattknoten (schmaler Kopf vorne,
 * dicker Schwanz hinten) und dreht sich dabei; nach 5 s steht wieder dasselbe Bild.
 * Steht die Verbindung, zieht der Kopf den Umriss des Croissant-Logos nach, danach wachsen
 * die Streben des Logos von außen nach innen. Dahinter atmet ein schwacher orangefarbener Schein.
 *
 * Knoten: OpenGL ES 2.0 (echtes Netz mit Tiefenpuffer, damit Kreuzungen sauber sind).
 * Hintergrund, Schein und Logo: normale Android-Zeichnung.
 * Alles pausiert, sobald der Ladebildschirm nicht sichtbar ist.
 */

private const val TAG = "USBCD-Loader"

/** Zeitpunkte und Zustand des Ladebildschirms – wird vom UI- und vom GL-Thread gelesen. */
class LoaderState {
    enum class Mode { WAIT, MORPH, DIM }

    @Volatile var mode = Mode.WAIT
        private set
    @Volatile private var waitStart = SystemClock.uptimeMillis()
    @Volatile private var baseS = 0f
    @Volatile var morphStart = 0L
        private set
    @Volatile private var s0 = 0f
    /** Zuordnung Knotenpunkt → Croissant-Punkt für den laufenden Morph */
    @Volatile var morphIdx: IntArray = IntArray(0)
        private set
    /** OpenGL ging nicht – dann steht einfach das Logo da */
    @Volatile var glFailed = false

    private val listeners = ArrayList<() -> Unit>()
    private val main = Handler(Looper.getMainLooper())
    /** Wird im UI-Thread aufgerufen, wenn sich der Zustand ändert */
    fun onChange(l: () -> Unit) { listeners.add(l) }
    private fun changed() { for (l in listeners) l() }
    /** vom GL-Thread: OpenGL ging nicht → Logo zeigen */
    fun notifyFailed() { main.post { changed() } }

    /** Fortschritt des Knotens in Runden (eine Runde = 5 s, danach gleiches Bild) */
    fun knotS(now: Long): Float = when (mode) {
        Mode.WAIT -> baseS + (now - waitStart) / LOOP_MS
        Mode.MORPH -> {
            val tau = min((now - morphStart) / 1000f, MORPH_S)
            s0 + SPEED * (tau - tau * tau / (2 * MORPH_S)) // bremst bis zum Stillstand
        }
        Mode.DIM -> s0
    }

    fun toWait() {
        if (mode == Mode.WAIT) return
        val now = SystemClock.uptimeMillis()
        baseS = knotS(now); waitStart = now
        mode = Mode.WAIT
        changed()
    }

    fun toMorph() {
        if (mode == Mode.MORPH) return
        val now = SystemClock.uptimeMillis()
        s0 = knotS(now)
        morphIdx = Knot.match(s0)
        morphStart = now
        mode = Mode.MORPH
        changed()
    }

    fun toDim() {
        if (mode == Mode.DIM) return
        s0 = knotS(SystemClock.uptimeMillis())
        mode = Mode.DIM
        changed()
    }

    /** 0..1: Fortschritt des Morphs (Kopf läuft voraus, siehe Knot.morphWeight) */
    fun morphX(now: Long): Float = if (mode == Mode.MORPH) (now - morphStart) / (MORPH_S * 1000f) else 0f

    /** 0..1: wie weit die Logo-Streben gewachsen sind */
    fun growG(now: Long): Float =
        if (mode == Mode.MORPH) ease((now - morphStart - GROW_START_MS) / GROW_MS.toFloat()) else 0f

    fun morphDone(now: Long) = mode == Mode.MORPH && now - morphStart >= DONE_MS

    /** Muss noch Bild für Bild gezeichnet werden? */
    fun animating(now: Long) = mode == Mode.WAIT || (mode == Mode.MORPH && now - morphStart < DONE_MS + 600)

    companion object {
        const val LOOP_MS = 5000f
        const val SPEED = 1f / 5f            // Runden pro Sekunde
        const val MORPH_S = 0.95f
        const val GROW_START_MS = 750L
        const val GROW_MS = 450L
        /** ab hier ist das Logo fertig und der Bildschirm darf ausblenden */
        const val DONE_MS = 1250L
    }
}

fun ease(x: Float): Float {
    val t = x.coerceIn(0f, 1f)
    return t * t * t * (t * (6 * t - 15) + 10)
}

/** Geometrie und Farben des Knotens, in Bruchteilen der Loader-Kachel. */
internal object Knot {
    const val N = 480
    private const val LENGTH = 0.88f           // Anteil der Knotenbahn, den der Schlauch bedeckt
    private const val R_HEAD = 0.58f
    private const val R_TAIL = 1.18f
    private const val R_POW = 0.8f
    private const val CAM = 14f
    private const val SCALE = 0.11f            // Kachel pro Knoten-Einheit
    private const val CX = 0.5f
    private const val CY = 0.51667f
    private val TILT = Math.toRadians(22.0).toFloat()
    private val ROT0 = Math.toRadians(90.0).toFloat()

    /** Farbverlauf entlang des Schlauchs (Kopf → Schwanz): Bernstein, Gold, Silber, Bernstein */
    private val STOP_POS = floatArrayOf(0f, 0.25f, 0.45f, 0.70f, 0.86f, 1f)
    private val STOP_COL = intArrayOf(0xD9782A, 0xF2A445, 0xFFC96C, 0xEFEFEF, 0xE3E3E3, 0xE9A04A)

    /** Kopf- bis Schwanzfarbe je Punkt (r, g, b in 0..1) */
    val colours = FloatArray(N * 3).also { c ->
        for (i in 0 until N) {
            val u = i / N.toFloat()
            var k = 0
            while (k < STOP_POS.size - 2 && u > STOP_POS[k + 1]) k++
            var f = (u - STOP_POS[k]) / (STOP_POS[k + 1] - STOP_POS[k])
            f = f * f * (3 - 2 * f)
            for (ch in 0..2) {
                val sh = 16 - 8 * ch
                val a = (STOP_COL[k] shr sh and 0xFF) / 255f
                val b = (STOP_COL[k + 1] shr sh and 0xFF) / 255f
                c[i * 3 + ch] = a + (b - a) * f
            }
        }
    }

    /** Farben im gedimmten Zustand (Fehler): entsättigt und abgedunkelt */
    val dimColours = FloatArray(N * 3).also { d ->
        val bg = 0x1F / 255f
        for (i in 0 until N) {
            val g = (colours[i * 3] + colours[i * 3 + 1] + colours[i * 3 + 2]) / 3f
            for (ch in 0..2) d[i * 3 + ch] = (colours[i * 3 + ch] * 0.35f + g * 0.65f) * 0.32f + bg * 0.68f
        }
    }

    /** Logofarbe (Silber → Gold) an jedem Croissant-Zielpunkt */
    val logoColours = FloatArray(N * 3).also { c ->
        val gx = CroissantShape.G1X - CroissantShape.G0X
        val gy = CroissantShape.G1Y - CroissantShape.G0Y
        for (i in 0 until N) {
            val qx = (CroissantShape.TARGET[i * 2] - CroissantShape.VIEW_X) / CroissantShape.VIEW_SIZE * 940f
            val qy = (CroissantShape.TARGET[i * 2 + 1] - CroissantShape.VIEW_Y) / CroissantShape.VIEW_SIZE * 940f
            val f = (((qx - CroissantShape.G0X) * gx + (qy - CroissantShape.G0Y) * gy) / (gx * gx + gy * gy)).coerceIn(0f, 1f)
            c[i * 3] = (0xE3 + (0xFF - 0xE3) * f) / 255f
            c[i * 3 + 1] = (0xE3 + (0xC9 - 0xE3) * f) / 255f
            c[i * 3 + 2] = (0xE3 + (0x6C - 0xE3) * f) / 255f
        }
    }

    private fun radius(u: Float) = R_HEAD + (R_TAIL - R_HEAD) * u.pow(R_POW)

    /**
     * Mittellinie bei Fortschritt [s]: je Punkt x, y, z (Tiefe, größer = weiter hinten) und Radius,
     * alles in Kachel-Bruchteilen.
     */
    fun centreline(s: Float, out: FloatArray) {
        val phase = s * 2f * PI.toFloat() / 3f       // ein Drittel weiterfließen …
        val rot = ROT0 - s * 2f * PI.toFloat() / 3f  // … und ein Drittel zurückdrehen → nahtlose Schleife
        val c = cos(rot); val sn = sin(rot)
        val ct = cos(TILT); val st = sin(TILT)
        for (i in 0 until N) {
            val u = i / N.toFloat()
            val t = phase - u * LENGTH * 2f * PI.toFloat()
            val x0 = sin(t) + 2f * sin(2f * t)
            val y0 = cos(t) - 2f * cos(2f * t)
            val z0 = -sin(3f * t)
            val x1 = c * x0 - sn * y0
            val y1 = sn * x0 + c * y0
            val y2 = ct * y1 - st * z0
            val z2 = st * y1 + ct * z0
            val persp = CAM / (CAM + z2)
            out[i * 4] = CX + x1 * SCALE * persp
            out[i * 4 + 1] = CY + y2 * SCALE * persp
            out[i * 4 + 2] = z2 * SCALE
            out[i * 4 + 3] = radius(u) * SCALE * persp
        }
    }

    /** Startpunkt und Richtung auf dem Croissant so wählen, dass die Wege am kürzesten sind. */
    fun match(s: Float): IntArray {
        val p = FloatArray(N * 4)
        centreline(s, p)
        val tg = CroissantShape.TARGET
        var bestCost = Float.MAX_VALUE
        var bestOff = 0
        var bestDir = 1
        for (dir in intArrayOf(1, -1)) {
            var off = 0
            while (off < N) {
                var cost = 0f
                for (i in 0 until N) {
                    val j = Math.floorMod(off + dir * i, N)
                    val dx = tg[j * 2] - p[i * 4]
                    val dy = tg[j * 2 + 1] - p[i * 4 + 1]
                    cost += dx * dx + dy * dy
                    if (cost >= bestCost) break
                }
                if (cost < bestCost) { bestCost = cost; bestOff = off; bestDir = dir }
                off += 2
            }
        }
        return IntArray(N) { Math.floorMod(bestOff + bestDir * it, N) }
    }

    /** Gewicht des Morphs je Punkt: der Kopf (u = 0) startet zuerst, der Rest folgt. */
    fun morphWeight(x: Float, u: Float) = ease((x - 0.35f * u) / 0.65f)
}

/** Hintergrund (Logo-Dunkel) mit weichem, atmendem Schein hinter dem Knoten. */
class LoaderBackdrop(context: Context, private val state: LoaderState, private val anchor: View) : View(context) {
    private val glowPaint = Paint().apply {
        isDither = true // gegen Stufen im dunklen Verlauf
        val stops = FloatArray(12) { it / 11f }
        // Gaußförmig: Deckkraft exp(-1,6 · d²), d in Radien; Verlauf reicht bis 2 Radien
        val colours = IntArray(12) { Color.argb((255 * exp(-1.6f * (2f * stops[it]).pow(2))).toInt(), 0xF2, 0xA4, 0x45) }
        shader = RadialGradient(0f, 0f, 1f, colours, stops, Shader.TileMode.CLAMP)
    }
    private val m = Matrix()
    private val a = IntArray(2)
    private val b = IntArray(2)
    private val density = resources.displayMetrics.density

    override fun onDraw(canvas: Canvas) {
        canvas.drawColor(BG)
        val now = SystemClock.uptimeMillis()
        if (state.mode != LoaderState.Mode.DIM) {
            val s = state.knotS(now)
            val p = 0.5f - 0.5f * cos(2f * PI.toFloat() * s)       // atmet einmal pro Knotenrunde
            var strength = 0.10f + 0.07f * p
            if (state.mode == LoaderState.Mode.MORPH) {
                // beim Morph ein kräftigerer Puls, am stärksten wenn das Croissant steht
                val d = ((now - state.morphStart) / 1000f - LoaderState.MORPH_S) / 0.45f
                strength += 0.10f * exp(-d * d)
            }
            anchor.getLocationInWindow(a); getLocationInWindow(b)
            val cx = a[0] - b[0] + anchor.width / 2f
            val cy = a[1] - b[1] + anchor.height * 0.62f
            val r = (300f + 40f * p) * density
            m.setScale(2f * r, 2f * r / 1.15f)
            m.postTranslate(cx, cy)
            glowPaint.shader.setLocalMatrix(m)
            glowPaint.alpha = (strength * 255).toInt().coerceIn(0, 255)
            canvas.drawRect(0f, 0f, width.toFloat(), height.toFloat(), glowPaint)
        }
        if (isShown && state.animating(now)) postInvalidateOnAnimation()
    }

    companion object { const val BG = 0xFF1F1F1F.toInt() }
}

/** Croissant-Logo, dessen Streben beim Morph mit runden Enden von außen nach innen wachsen. */
class LogoRevealView(context: Context, private val state: LoaderState) : View(context) {
    private val logo: Drawable = context.getDrawable(R.drawable.logo)!!.mutate()
    private val grow: Bitmap = BitmapFactory.decodeResource(
        resources, R.drawable.logo_grow, BitmapFactory.Options().apply { inScaled = false },
    )
    private val maskPaint = Paint(Paint.FILTER_BITMAP_FLAG).apply { xfermode = PorterDuffXfermode(PorterDuff.Mode.DST_IN) }
    private val cm = ColorMatrix()
    private val dst = RectF()

    override fun onDraw(canvas: Canvas) {
        val now = SystemClock.uptimeMillis()
        val g = when {
            state.glFailed -> 1f
            else -> state.growG(now)
        }
        if (g > 0f) {
            val size = CroissantShape.VIEW_SIZE * width
            val l = CroissantShape.VIEW_X * width
            val t = CroissantShape.VIEW_Y * height
            logo.setBounds(l.toInt(), t.toInt(), (l + size).toInt(), (t + size).toInt())
            if (g >= 1f) {
                logo.draw(canvas)
            } else {
                // Karte: Helligkeit = 1 − Zeitpunkt, ab dem ein Pixel erscheint. Schwelle über die Alpha-Zeile.
                val sc = canvas.saveLayer(0f, 0f, width.toFloat(), height.toFloat(), null)
                logo.draw(canvas)
                val k = 24f
                val off = 255f - k * 255f * (1f - g)
                cm.set(floatArrayOf(0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, 0f, k, off))
                maskPaint.colorFilter = ColorMatrixColorFilter(cm)
                dst.set(l, t, l + size, t + size)
                canvas.drawBitmap(grow, null, dst, maskPaint)
                canvas.restoreToCount(sc)
            }
        }
        if (isShown && state.mode == LoaderState.Mode.MORPH && !state.morphDone(now)) postInvalidateOnAnimation()
    }
}

/** Der Knoten auf der Grafikeinheit (transparente TextureView). */
class KnotView(context: Context, private val state: LoaderState) : TextureView(context), TextureView.SurfaceTextureListener {
    private var renderer: KnotRenderer? = null
    private var shown = false

    init {
        isOpaque = false
        surfaceTextureListener = this
    }

    fun kick() { renderer?.kick() }

    override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) {
        renderer = KnotRenderer(st, w, h, state).also { it.begin(); it.setVisible(shown) }
    }

    override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) { renderer?.resize(w, h) }

    override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean {
        renderer?.finish()
        renderer = null
        return true
    }

    override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}

    override fun onVisibilityAggregated(isVisible: Boolean) {
        super.onVisibilityAggregated(isVisible)
        shown = isVisible
        renderer?.setVisible(isVisible)
    }
}

/** Eigener Render-Thread mit EGL; zeichnet im Bildtakt des Displays (Choreographer). */
private class KnotRenderer(
    private val st: SurfaceTexture,
    @Volatile private var w: Int,
    @Volatile private var h: Int,
    private val state: LoaderState,
) : HandlerThread("USBCD-Knoten"), Choreographer.FrameCallback {
    private lateinit var handler: Handler
    @Volatile private var visible = false
    private var frameRequested = false
    private var ready = false

    private var dpy: EGLDisplay = EGL14.EGL_NO_DISPLAY
    private var ctx: EGLContext = EGL14.EGL_NO_CONTEXT
    private var surf: EGLSurface = EGL14.EGL_NO_SURFACE
    private var program = 0
    private var aPos = 0
    private var aCol = 0
    private var uScale = 0
    private var uZ = 0

    private val mesh = KnotMesh()
    private val verts: FloatBuffer = ByteBuffer.allocateDirect(mesh.vArr.size * 4).order(ByteOrder.nativeOrder()).asFloatBuffer()
    private val indices: ShortBuffer = ByteBuffer.allocateDirect(mesh.indices.size * 2).order(ByteOrder.nativeOrder())
        .asShortBuffer().put(mesh.indices).also { it.position(0) }

    fun begin() {
        start()
        handler = Handler(looper)
        handler.post {
            ready = try { initGl() } catch (e: Exception) { Log.w(TAG, "OpenGL-Start fehlgeschlagen", e); false }
            if (!ready) failed() else schedule()
        }
    }

    fun setVisible(v: Boolean) { visible = v; kick() }
    fun kick() { if (::handler.isInitialized) handler.post { schedule() } }
    fun resize(nw: Int, nh: Int) { w = nw; h = nh; kick() }

    fun finish() {
        if (!::handler.isInitialized) return
        val done = CountDownLatch(1)
        handler.post {
            Choreographer.getInstance().removeFrameCallback(this)
            releaseGl()
            done.countDown()
            quitSafely()
        }
        done.await(500, TimeUnit.MILLISECONDS)
    }

    private fun failed() {
        state.glFailed = true
        state.notifyFailed()
    }

    private fun schedule() {
        if (frameRequested || !ready || !visible) return
        frameRequested = true
        Choreographer.getInstance().postFrameCallback(this)
    }

    override fun doFrame(frameTimeNanos: Long) {
        frameRequested = false
        if (!ready || !visible) return
        val now = SystemClock.uptimeMillis()
        if (!draw(now)) { ready = false; failed(); return }
        if (state.animating(now)) schedule()
    }

    // --- EGL / GL ----------------------------------------------------------------------

    private fun initGl(): Boolean {
        dpy = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        val ver = IntArray(2)
        if (!EGL14.eglInitialize(dpy, ver, 0, ver, 1)) return false
        val cfg = chooseConfig(4) ?: chooseConfig(0) ?: return false
        ctx = EGL14.eglCreateContext(dpy, cfg, EGL14.EGL_NO_CONTEXT, intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0)
        if (ctx == EGL14.EGL_NO_CONTEXT) return false
        surf = EGL14.eglCreateWindowSurface(dpy, cfg, st, intArrayOf(EGL14.EGL_NONE), 0)
        if (surf == EGL14.EGL_NO_SURFACE) return false
        if (!EGL14.eglMakeCurrent(dpy, surf, surf, ctx)) return false
        program = link(VS, FS)
        if (program == 0) return false
        aPos = GLES20.glGetAttribLocation(program, "aPos")
        aCol = GLES20.glGetAttribLocation(program, "aCol")
        uScale = GLES20.glGetUniformLocation(program, "uScale")
        uZ = GLES20.glGetUniformLocation(program, "uZ")
        return true
    }

    private fun chooseConfig(samples: Int): EGLConfig? {
        val attrs = mutableListOf(
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8, EGL14.EGL_ALPHA_SIZE, 8,
            EGL14.EGL_DEPTH_SIZE, 16, EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
        )
        if (samples > 0) attrs += listOf(EGL14.EGL_SAMPLE_BUFFERS, 1, EGL14.EGL_SAMPLES, samples)
        attrs += EGL14.EGL_NONE
        val out = arrayOfNulls<EGLConfig>(1)
        val num = IntArray(1)
        if (!EGL14.eglChooseConfig(dpy, attrs.toIntArray(), 0, out, 0, 1, num, 0) || num[0] == 0) return null
        return out[0]
    }

    private fun link(vs: String, fs: String): Int {
        fun compile(type: Int, src: String): Int {
            val s = GLES20.glCreateShader(type)
            GLES20.glShaderSource(s, src)
            GLES20.glCompileShader(s)
            val ok = IntArray(1)
            GLES20.glGetShaderiv(s, GLES20.GL_COMPILE_STATUS, ok, 0)
            if (ok[0] == 0) { Log.w(TAG, GLES20.glGetShaderInfoLog(s)); GLES20.glDeleteShader(s); return 0 }
            return s
        }
        val v = compile(GLES20.GL_VERTEX_SHADER, vs)
        val f = compile(GLES20.GL_FRAGMENT_SHADER, fs)
        if (v == 0 || f == 0) return 0
        val p = GLES20.glCreateProgram()
        GLES20.glAttachShader(p, v); GLES20.glAttachShader(p, f)
        GLES20.glLinkProgram(p)
        val ok = IntArray(1)
        GLES20.glGetProgramiv(p, GLES20.GL_LINK_STATUS, ok, 0)
        if (ok[0] == 0) { Log.w(TAG, GLES20.glGetProgramInfoLog(p)); return 0 }
        return p
    }

    private fun releaseGl() {
        if (dpy == EGL14.EGL_NO_DISPLAY) return
        EGL14.eglMakeCurrent(dpy, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
        if (surf != EGL14.EGL_NO_SURFACE) EGL14.eglDestroySurface(dpy, surf)
        if (ctx != EGL14.EGL_NO_CONTEXT) EGL14.eglDestroyContext(dpy, ctx)
        EGL14.eglTerminate(dpy)
        dpy = EGL14.EGL_NO_DISPLAY; ctx = EGL14.EGL_NO_CONTEXT; surf = EGL14.EGL_NO_SURFACE
        ready = false
    }

    // --- Zeichnen --------------------------------------------------------------------------

    private fun draw(now: Long): Boolean {
        val W = w.toFloat()
        val H = h.toFloat()
        val mode = when (state.mode) {
            LoaderState.Mode.WAIT -> KnotMesh.WAIT
            LoaderState.Mode.MORPH -> KnotMesh.MORPH
            LoaderState.Mode.DIM -> KnotMesh.DIM
        }
        mesh.build(state.knotS(now), mode, state.morphX(now), state.morphIdx, W, H)
        verts.position(0)
        verts.put(mesh.vArr)
        GLES20.glViewport(0, 0, w, h)
        GLES20.glClearColor(0f, 0f, 0f, 0f)
        GLES20.glClearDepthf(1f)
        GLES20.glEnable(GLES20.GL_DEPTH_TEST)
        GLES20.glDepthFunc(GLES20.GL_LESS)
        GLES20.glDisable(GLES20.GL_CULL_FACE)
        GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT or GLES20.GL_DEPTH_BUFFER_BIT)
        GLES20.glUseProgram(program)
        GLES20.glUniform2f(uScale, 2f / W, -2f / H)
        GLES20.glUniform1f(uZ, 1f / (0.8f * W)) // Tiefe in Pixeln → −1..1 (Knoten samt Dicke reicht etwa ±0,5 Kachelbreiten)
        verts.position(0)
        GLES20.glVertexAttribPointer(aPos, 3, GLES20.GL_FLOAT, false, 24, verts)
        GLES20.glEnableVertexAttribArray(aPos)
        verts.position(3)
        GLES20.glVertexAttribPointer(aCol, 3, GLES20.GL_FLOAT, false, 24, verts)
        GLES20.glEnableVertexAttribArray(aCol)
        indices.position(0)
        GLES20.glDrawElements(GLES20.GL_TRIANGLES, mesh.indices.size, GLES20.GL_UNSIGNED_SHORT, indices)
        if (!EGL14.eglSwapBuffers(dpy, surf)) {
            Log.w(TAG, "eglSwapBuffers: 0x" + Integer.toHexString(EGL14.eglGetError()))
            return false
        }
        return true
    }

    companion object {
        const val VS = """
            attribute vec3 aPos;
            attribute vec3 aCol;
            uniform vec2 uScale;
            uniform float uZ;
            varying vec3 vCol;
            void main() {
                gl_Position = vec4(aPos.x * uScale.x - 1.0, aPos.y * uScale.y + 1.0, aPos.z * uZ, 1.0);
                vCol = aCol;
            }
        """
        const val FS = """
            precision mediump float;
            varying vec3 vCol;
            void main() { gl_FragColor = vec4(vCol, 1.0); }
        """
    }
}

/**
 * Dreiecksnetz des Schlauchs: Ringe um die Mittellinie, runde Kappen an Kopf und Schwanz.
 * Ohne Android-Abhängigkeit (lässt sich auf dem PC prüfen).
 */
internal class KnotMesh {
    /** je Ecke x, y, z (Pixel) und r, g, b */
    val vArr = FloatArray(RINGS * SEG * 6)
    val indices = ShortArray((RINGS - 1) * SEG * 6).also { idx ->
        var n = 0
        for (r in 0 until RINGS - 1) for (k in 0 until SEG) {
            val a = r * SEG + k
            val b = r * SEG + (k + 1) % SEG
            val c = a + SEG
            val d = b + SEG
            idx[n++] = a.toShort(); idx[n++] = c.toShort(); idx[n++] = b.toShort()
            idx[n++] = b.toShort(); idx[n++] = c.toShort(); idx[n++] = d.toShort()
        }
    }
    private val line = FloatArray(Knot.N * 4)

    /** Ringe um die Mittellinie (plus runde Kappen an Kopf und Schwanz) in Pixeln. */
    fun build(s: Float, mode: Int, x: Float, idx: IntArray, W: Float, H: Float) {
        val n = Knot.N
        Knot.centreline(s, line)
        val tg = CroissantShape.TARGET
        val half = CroissantShape.HALF
        // Mittellinie in Pixeln, Morph anwenden
        for (i in 0 until n) {
            var px = line[i * 4]; var py = line[i * 4 + 1]; var pz = line[i * 4 + 2]; var pr = line[i * 4 + 3]
            var zf = 1f
            var r = Knot.colours[i * 3]; var g = Knot.colours[i * 3 + 1]; var b = Knot.colours[i * 3 + 2]
            if (mode == DIM) {
                r = Knot.dimColours[i * 3]; g = Knot.dimColours[i * 3 + 1]; b = Knot.dimColours[i * 3 + 2]
            } else if (mode == MORPH && x > 0f && idx.size == n) {
                val e = Knot.morphWeight(x, i / n.toFloat())
                val j = idx[i]
                px += (tg[j * 2] - px) * e
                py += (tg[j * 2 + 1] - py) * e
                pz *= (1f - e)
                pr += (half - pr) * e
                zf = 1f - e + 0.02f * e
                r += (Knot.logoColours[j * 3] - r) * e
                g += (Knot.logoColours[j * 3 + 1] - g) * e
                b += (Knot.logoColours[j * 3 + 2] - b) * e
            }
            cl[i * 8] = px * W; cl[i * 8 + 1] = py * H; cl[i * 8 + 2] = pz * W; cl[i * 8 + 3] = pr * W
            cl[i * 8 + 4] = zf; cl[i * 8 + 5] = r; cl[i * 8 + 6] = g; cl[i * 8 + 7] = b
        }
        // Tangenten und parallel mitgeführte Normalen
        var nx = 0f; var ny = 0f; var nz = 1f
        var ring = 0
        for (i in 0 until n) {
            val a = if (i == 0) 0 else i - 1
            val c = if (i == n - 1) n - 1 else i + 1
            var tx = cl[c * 8] - cl[a * 8]; var ty = cl[c * 8 + 1] - cl[a * 8 + 1]; var tz = cl[c * 8 + 2] - cl[a * 8 + 2]
            val tl = sqrt(tx * tx + ty * ty + tz * tz).coerceAtLeast(1e-6f)
            tx /= tl; ty /= tl; tz /= tl
            if (i == 0) { // erste Normale: irgendeine Senkrechte
                if (abs(tz) > 0.9f) { nx = 1f; ny = 0f; nz = 0f } else { nx = 0f; ny = 0f; nz = 1f }
            }
            val d = nx * tx + ny * ty + nz * tz
            var mx = nx - d * tx; var my = ny - d * ty; var mz = nz - d * tz
            val ml = sqrt(mx * mx + my * my + mz * mz)
            if (ml > 1e-4f) { mx /= ml; my /= ml; mz /= ml; nx = mx; ny = my; nz = mz }
            tan[i * 3] = tx; tan[i * 3 + 1] = ty; tan[i * 3 + 2] = tz
            nrm[i * 3] = nx; nrm[i * 3 + 1] = ny; nrm[i * 3 + 2] = nz
        }
        // Kopfkappe (Spitze zuerst), Schlauch, Schwanzkappe
        for (k in CAP downTo 1) ring = putRing(ring, 0, -sin(k / CAP.toFloat() * HALF_PI), cos(k / CAP.toFloat() * HALF_PI))
        for (i in 0 until n) ring = putRing(ring, i, 0f, 1f)
        for (k in 1..CAP) ring = putRing(ring, n - 1, sin(k / CAP.toFloat() * HALF_PI), cos(k / CAP.toFloat() * HALF_PI))
    }

    private val cl = FloatArray(Knot.N * 8)
    private val tan = FloatArray(Knot.N * 3)
    private val nrm = FloatArray(Knot.N * 3)

    /** Ein Ring um Punkt [i], entlang der Tangente um [along]·r verschoben, Radius [scale]·r. */
    private fun putRing(ring: Int, i: Int, along: Float, scale: Float): Int {
        val cx = cl[i * 8]; val cy = cl[i * 8 + 1]; val cz = cl[i * 8 + 2]; val r = cl[i * 8 + 3]; val zf = cl[i * 8 + 4]
        val tx = tan[i * 3]; val ty = tan[i * 3 + 1]; val tz = tan[i * 3 + 2]
        val nx = nrm[i * 3]; val ny = nrm[i * 3 + 1]; val nz = nrm[i * 3 + 2]
        // Binormale = T × N
        val bx = ty * nz - tz * ny; val by = tz * nx - tx * nz; val bz = tx * ny - ty * nx
        val ox = cx + tx * along * r; val oy = cy + ty * along * r; val oz = cz + tz * along * r * zf
        val rr = r * scale
        var o = ring * SEG * 6
        for (k in 0 until SEG) {
            val c = COS[k]; val s = SIN[k]
            vArr[o] = ox + rr * (c * nx + s * bx)
            vArr[o + 1] = oy + rr * (c * ny + s * by)
            vArr[o + 2] = oz + rr * (c * nz + s * bz) * zf
            vArr[o + 3] = cl[i * 8 + 5]; vArr[o + 4] = cl[i * 8 + 6]; vArr[o + 5] = cl[i * 8 + 7]
            o += 6
        }
        return ring + 1
    }

    companion object {
        const val WAIT = 0
        const val MORPH = 1
        const val DIM = 2
        const val SEG = 16
        const val CAP = 6
        const val RINGS = Knot.N + 2 * CAP
        val HALF_PI = (PI / 2).toFloat()
        val COS = FloatArray(SEG) { cos(2 * PI * it / SEG).toFloat() }
        val SIN = FloatArray(SEG) { sin(2 * PI * it / SEG).toFloat() }
    }
}
