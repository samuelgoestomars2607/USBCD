package de.samuel.usbcd

import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import kotlin.math.min

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
