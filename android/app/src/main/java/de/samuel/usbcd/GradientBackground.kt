package de.samuel.usbcd

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapShader
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.Shader
import android.os.SystemClock
import android.view.View
import kotlin.math.sin
import kotlin.random.Random

/**
 * Hintergrund, solange keine Verbindung besteht: senkrechter Verlauf vom Logo-Orange (#FFC96C, oben)
 * zum Logo-Dunkel (#1F1F1F, unten) mit feinem, ruhendem Korn. Der Übergang wandert langsam auf und ab.
 * Ab der Bildschirmmitte ist es dunkel genug für helle Schrift (Kontrast ≥ 6,8 : 1 bzw. ≥ 5,1 : 1).
 * Deckt das letzte Bild der Übertragung vollständig ab.
 */
class GradientBackground(context: Context) : View(context) {
    private val gradientPaint = Paint()
    private val grainPaint = Paint() // ohne Filter, damit das Korn scharf bleibt
    private val matrix = Matrix()
    private var gradient: LinearGradient? = null
    private val start = SystemClock.uptimeMillis()

    init {
        val grain = Bitmap.createBitmap(grainPixels(), GRAIN_SIZE, GRAIN_SIZE, Bitmap.Config.ARGB_8888)
        val shader = BitmapShader(grain, Shader.TileMode.REPEAT, Shader.TileMode.REPEAT)
        shader.setLocalMatrix(Matrix().apply { setScale(GRAIN_SCALE, GRAIN_SCALE) })
        grainPaint.shader = shader
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        // Etwas höher als der Bildschirm, damit beim Wandern oben/unten keine Kante sichtbar wird
        val span = h * (1f + 2f * DRIFT)
        gradient = LinearGradient(
            0f, -h * DRIFT, 0f, -h * DRIFT + span,
            intArrayOf(ORANGE, MID, DARK, DARK),
            floatArrayOf(0f, 0.30f, 0.60f, 1f),
            Shader.TileMode.CLAMP,
        )
        gradientPaint.shader = gradient
    }

    override fun onDraw(canvas: Canvas) {
        val g = gradient ?: return
        val w = width.toFloat()
        val h = height.toFloat()
        val t = ((SystemClock.uptimeMillis() - start) % CYCLE_MS).toFloat() / CYCLE_MS
        matrix.setTranslate(0f, (sin(t * 2.0 * Math.PI) * h * DRIFT).toFloat())
        g.setLocalMatrix(matrix)
        canvas.drawRect(0f, 0f, w, h, gradientPaint)
        canvas.drawRect(0f, 0f, w, h, grainPaint)
        if (visibility == VISIBLE) postInvalidateOnAnimation()
    }

    override fun onVisibilityChanged(changedView: View, visibility: Int) {
        super.onVisibilityChanged(changedView, visibility)
        if (visibility == VISIBLE) postInvalidateOnAnimation()
    }

    companion object {
        /** Orange aus dem USBCD-Logo */
        private val ORANGE = Color.rgb(0xFF, 0xC9, 0x6C)
        /** Zwischenton (45 % Orange, 55 % Dunkel), damit der Übergang warm bleibt */
        private val MID = Color.rgb(0x84, 0x6C, 0x42)
        /** Dunkel aus dem USBCD-Logo */
        private val DARK = Color.rgb(0x1F, 0x1F, 0x1F)
        private const val CYCLE_MS = 20_000L
        /** Wie weit der Verlauf wandert, als Anteil der Bildschirmhöhe */
        private const val DRIFT = 0.08f
        private const val GRAIN_SIZE = 128
        private const val GRAIN_SCALE = 2f
        private const val GRAIN_ALPHA = 22

        /** Graues Rauschen mit geringer Deckkraft; fester Startwert, damit es bei jedem Start gleich aussieht */
        private fun grainPixels(): IntArray {
            val rnd = Random(0x05BCD)
            return IntArray(GRAIN_SIZE * GRAIN_SIZE) {
                val v = rnd.nextInt(256)
                Color.argb(GRAIN_ALPHA, v, v, v)
            }
        }
    }
}
