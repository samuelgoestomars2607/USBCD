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
import android.graphics.drawable.Drawable
import android.os.SystemClock
import android.view.View
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.exp
import kotlin.math.pow

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
