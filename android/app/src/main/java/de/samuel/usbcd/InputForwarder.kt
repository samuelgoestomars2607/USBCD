package de.samuel.usbcd

import android.view.MotionEvent
import java.nio.ByteBuffer
import java.util.concurrent.LinkedBlockingQueue
import kotlin.math.atan
import kotlin.math.cos
import kotlin.math.sin
import kotlin.math.tan

/**
 * Schickt Touch und S Pen an den PC (Format: host/src/input.rs).
 * Gesendet wird in einem eigenen Thread – nie im UI-Thread.
 */
class InputForwarder(private val send: (Int, ByteArray) -> Boolean) {
    @Volatile var enabled = true
    private val queue = LinkedBlockingQueue<ByteArray>(2048)

    init {
        Thread({
            while (true) {
                val b = queue.take()
                send(Proto.T_INPUT, b)
            }
        }, "input-out").apply { isDaemon = true }.start()
    }

    private fun post(b: ByteArray) {
        if (!queue.offer(b)) { // PC hängt hinterher: Ältestes verwerfen
            queue.poll()
            queue.offer(b)
        }
    }

    private fun norm(v: Float, size: Int): Int = ((v / size.coerceAtLeast(1)) * 65535f).toInt().coerceIn(0, 65535)

    /** true = Ereignis wurde an den PC weitergegeben. */
    fun handle(ev: MotionEvent, w: Int, h: Int): Boolean {
        if (!enabled || w <= 0 || h <= 0) return false
        val stylus = (0 until ev.pointerCount).firstOrNull {
            val t = ev.getToolType(it)
            t == MotionEvent.TOOL_TYPE_STYLUS || t == MotionEvent.TOOL_TYPE_ERASER
        }
        if (stylus != null) {
            pen(ev, stylus, w, h)
            return true
        }
        val fingers = (0 until ev.pointerCount).any { ev.getToolType(it) == MotionEvent.TOOL_TYPE_FINGER }
        if (fingers && ev.actionMasked !in HOVER) {
            touch(ev, w, h)
            return true
        }
        return false
    }

    // --- Stift ---

    private fun pen(ev: MotionEvent, i: Int, w: Int, h: Int) {
        val (inRange, contact) = when (ev.actionMasked) {
            MotionEvent.ACTION_HOVER_ENTER, MotionEvent.ACTION_HOVER_MOVE -> true to false
            MotionEvent.ACTION_HOVER_EXIT -> false to false
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_MOVE, MotionEvent.ACTION_POINTER_DOWN -> true to true
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> true to false
            MotionEvent.ACTION_CANCEL -> false to false
            else -> return
        }
        var flags = 0
        if (inRange) flags = flags or 1
        if (contact) flags = flags or 2
        if (ev.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0) flags = flags or 4
        if (ev.getToolType(i) == MotionEvent.TOOL_TYPE_ERASER) flags = flags or 8
        // Zwischenwerte (der Stift tastet schneller ab, als Ereignisse kommen) für glatte Linien
        if (contact && ev.actionMasked == MotionEvent.ACTION_MOVE) {
            for (k in 0 until ev.historySize) {
                post(penBytes(flags, ev.getHistoricalX(i, k), ev.getHistoricalY(i, k),
                    ev.getHistoricalPressure(i, k),
                    ev.getHistoricalAxisValue(MotionEvent.AXIS_TILT, i, k),
                    ev.getHistoricalAxisValue(MotionEvent.AXIS_ORIENTATION, i, k), w, h))
            }
        }
        val pressure = if (contact) ev.getPressure(i) else 0f
        post(penBytes(flags, ev.getX(i), ev.getY(i), pressure,
            ev.getAxisValue(MotionEvent.AXIS_TILT, i), ev.getAxisValue(MotionEvent.AXIS_ORIENTATION, i), w, h))
    }

    private fun penBytes(flags: Int, x: Float, y: Float, pressure: Float, tilt: Float, orient: Float, w: Int, h: Int): ByteArray {
        // Android: Neigung + Richtung → Windows: Neigung nach rechts (X) bzw. unten (Y), Grad
        val t = tilt.coerceIn(0f, 1.5f)
        val tx = Math.toDegrees(atan(tan(t) * sin(orient)).toDouble()).toInt().coerceIn(-90, 90)
        val ty = Math.toDegrees(atan(-tan(t) * cos(orient)).toDouble()).toInt().coerceIn(-90, 90)
        return ByteBuffer.allocate(10).apply {
            put(1)
            put(flags.toByte())
            putShort(norm(x, w).toShort())
            putShort(norm(y, h).toShort())
            putShort((pressure.coerceIn(0f, 1f) * 1024f).toInt().toShort())
            put(tx.toByte())
            put(ty.toByte())
        }.array()
    }

    // --- Finger ---

    private fun touch(ev: MotionEvent, w: Int, h: Int) {
        val action = ev.actionMasked
        val lifted = if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_POINTER_UP) ev.actionIndex else -1
        val canceled = action == MotionEvent.ACTION_CANCEL
        val ids = (0 until ev.pointerCount).filter {
            ev.getToolType(it) == MotionEvent.TOOL_TYPE_FINGER && ev.getPointerId(it) < MAX_TOUCH
        }
        val b = ByteBuffer.allocate(2 + ids.size * 6)
        b.put(2)
        b.put(ids.size.toByte())
        for (i in ids) {
            var f = 0
            if (!canceled && i != lifted) f = f or 1
            if (canceled) f = f or 2
            b.put(ev.getPointerId(i).toByte())
            b.put(f.toByte())
            b.putShort(norm(ev.getX(i), w).toShort())
            b.putShort(norm(ev.getY(i), h).toShort())
        }
        post(b.array())
    }

    companion object {
        private const val MAX_TOUCH = 10
        private val HOVER = setOf(
            MotionEvent.ACTION_HOVER_ENTER, MotionEvent.ACTION_HOVER_MOVE, MotionEvent.ACTION_HOVER_EXIT
        )
    }
}
