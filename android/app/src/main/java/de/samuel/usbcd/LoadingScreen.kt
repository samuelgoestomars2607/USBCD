package de.samuel.usbcd

import android.content.Context
import android.os.Handler
import android.os.SystemClock
import android.view.Display
import android.view.Gravity
import android.view.View
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView

/**
 * Ladebildschirm über dem Videobild: Knoten als Loader mit Schein dahinter, Status, Hinweis
 * und unten eine Fußzeile mit Name und Display-Daten. Beim Verbinden wird der Knoten zum
 * Croissant; ausgeblendet wird erst, wenn das Logo steht.
 *
 * [onShow] / [onHide]: die Activity zeigt bzw. verbirgt das Videobild (Touch, Standbild).
 */
class LoadingScreen(
    private val context: Context,
    private val mode: Display.Mode,
    private val ui: Handler,
    private val onShow: () -> Unit,
    private val onHide: () -> Unit,
) {
    private val loader = LoaderState()
    private lateinit var statusView: TextView
    private lateinit var detailView: TextView
    private lateinit var footerView: TextView
    /** Erstes Videobild ist da, Ausblenden wartet nur noch auf das Ende des Morphs */
    private var hidePending = false

    val view: FrameLayout = build()

    private fun dp(v: Int) = (v * context.resources.displayMetrics.density).toInt()

    private fun text(weight: Int, sizeSp: Float, color: Int) = TextView(context).apply {
        typeface = Fonts.get(context, weight)
        textSize = sizeSp
        setTextColor(color)
        gravity = Gravity.CENTER
        includeFontPadding = false
    }

    private fun build(): FrameLayout {
        val primary = context.getColor(R.color.text_primary)
        val secondary = context.getColor(R.color.text_secondary)
        val muted = context.getColor(R.color.text_muted)
        val overlay = FrameLayout(context)

        val knotBox = FrameLayout(context)
        val knotView = KnotView(context, loader)
        val logoView = LogoRevealView(context, loader)
        knotBox.addView(knotView, FrameLayout.LayoutParams(-1, -1))
        knotBox.addView(logoView, FrameLayout.LayoutParams(-1, -1))
        val backdrop = LoaderBackdrop(context, loader, knotBox)
        overlay.addView(backdrop, FrameLayout.LayoutParams(-1, -1))
        loader.onChange {
            knotView.kick()
            backdrop.invalidate()
            logoView.invalidate()
        }

        val column = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
        }
        column.addView(knotBox, LinearLayout.LayoutParams(dp(250), dp(250)))
        statusView = text(500, 28f, primary)
        column.addView(statusView, LinearLayout.LayoutParams(-2, -2).apply { topMargin = dp(40) })
        detailView = text(400, 16f, secondary).apply { setLineSpacing(0f, 1.4f) }
        column.addView(detailView, LinearLayout.LayoutParams(-2, -2).apply { topMargin = dp(16) })
        overlay.addView(column, FrameLayout.LayoutParams(-2, -2, Gravity.CENTER).apply {
            leftMargin = dp(32); rightMargin = dp(32); bottomMargin = dp(32)
        })

        val footer = LinearLayout(context).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        footer.addView(text(700, 12f, muted).apply {
            text = context.getString(R.string.app_name)
            letterSpacing = 0.32f
        })
        footerView = text(400, 12f, muted)
        footer.addView(footerView, LinearLayout.LayoutParams(-2, -2).apply { leftMargin = dp(8) })
        overlay.addView(footer, FrameLayout.LayoutParams(-2, -2, Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL).apply {
            bottomMargin = dp(36)
        })
        val (w, h) = DisplayInfo.landscapeSize(mode)
        footerView.text = "·   " + context.getString(R.string.display_info, w, h, "%.0f".format(mode.refreshRate))
        return overlay
    }

    private fun show() {
        onShow()
        hidePending = false
        view.animate().cancel()
        view.alpha = 1f
        view.visibility = View.VISIBLE
    }

    /** Erstes Videobild: ausblenden – läuft gerade der Morph zum Croissant, erst wenn er fertig ist. */
    fun hide() {
        if (loader.mode == LoaderState.Mode.MORPH && !loader.morphDone(SystemClock.uptimeMillis())) {
            hidePending = true
            return
        }
        onHide()
        hidePending = false
        view.animate().alpha(0f).setDuration(250).withEndAction { view.visibility = View.GONE }.start()
    }

    private val morphFinished = Runnable { if (hidePending) hide() }

    fun showWaiting() {
        show()
        loader.toWait()
        statusView.text = context.getString(R.string.waiting)
        detailView.text = context.getString(R.string.waiting_detail)
    }

    fun showBlocked() {
        show()
        loader.toDim()
        statusView.text = context.getString(R.string.port_busy)
        detailView.text = context.getString(R.string.port_busy_detail)
    }

    /** Kein Decoder dieses Tablets kann das Bild anzeigen – klar sagen statt schwarzem Bild. */
    fun showUnsupported() {
        show()
        loader.toDim()
        statusView.text = context.getString(R.string.unsupported)
        detailView.text = context.getString(R.string.unsupported_detail)
    }

    fun showConnecting(info: String) {
        show()
        loader.toMorph()
        ui.removeCallbacks(morphFinished)
        ui.postDelayed(morphFinished, LoaderState.DONE_MS + 20)
        statusView.text = context.getString(R.string.connected)
        detailView.text = context.getString(R.string.starting, info)
    }
}
