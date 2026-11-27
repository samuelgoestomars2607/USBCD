package de.samuel.usbcd

import android.content.Context
import android.graphics.SurfaceTexture
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLSurface
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import android.view.Choreographer
import android.view.TextureView
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import java.nio.ShortBuffer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

private const val TAG = "USBCD-Loader"

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
