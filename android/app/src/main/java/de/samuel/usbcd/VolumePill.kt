package de.samuel.usbcd

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.RectF
import android.view.View

/**
 * Kleine Anzeige der Windows-Lautstärke des Tablet-Lautsprechers, nachdem eine
 * Lautstärketaste gedrückt wurde (Farben aus dem USBCD-Logo).
 */
class VolumePill(context: Context) : View(context) {
    private val density = resources.displayMetrics.density
    private val bg = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.argb(235, 0x1F, 0x1F, 0x1F) }
    private val track = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.rgb(0x3A, 0x3A, 0x3A) }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.rgb(0xFF, 0xC9, 0x6C) }
    private val label = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.rgb(0xF1, 0xF1, 0xF1)
        textSize = android.util.TypedValue.applyDimension(
            android.util.TypedValue.COMPLEX_UNIT_SP, 16f, resources.displayMetrics
        )
        typeface = Fonts.get(context, 500)
    }
    private val rect = RectF()
    private var level = 0
    private var muted = false

    fun show(level: Int, muted: Boolean) {
        this.level = level.coerceIn(0, 100)
        this.muted = muted
        contentDescription = text()
        invalidate()
    }

    private fun text(): String =
        if (muted) context.getString(R.string.volume_muted) else context.getString(R.string.volume_level, level)

    override fun onDraw(canvas: Canvas) {
        val w = width.toFloat()
        val h = height.toFloat()
        rect.set(0f, 0f, w, h)
        canvas.drawRoundRect(rect, h / 2f, h / 2f, bg)

        val pad = 24f * density
        val barH = 6f * density
        val barY = h * 0.68f
        val textY = h * 0.42f - (label.descent() + label.ascent()) / 2f
        canvas.drawText(text(), pad, textY, label)

        rect.set(pad, barY - barH / 2f, w - pad, barY + barH / 2f)
        canvas.drawRoundRect(rect, barH / 2f, barH / 2f, track)
        if (!muted && level > 0) {
            rect.right = pad + (w - 2f * pad) * level / 100f
            canvas.drawRoundRect(rect, barH / 2f, barH / 2f, fill)
        }
    }
}
