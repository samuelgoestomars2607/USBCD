package de.samuel.usbcd

import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/** Messwerte für die Info-Anzeige – alle echt gemessen, nichts geschätzt. */
object Stats {
    val framesShown = AtomicInteger()
    val bytesIn = AtomicLong()
    val decodeNs = AtomicLong()
    val decodeCount = AtomicInteger()
    /** Laufzeit PC → Tablet → PC in µs (misst der PC, schickt sie mit dem nächsten PING) */
    @Volatile var rttUs = 0
    @Volatile var stream = ""

    data class Snapshot(val fps: Float, val mbit: Float, val decodeMs: Float, val usbMs: Float, val stream: String)

    private var lastAt = System.nanoTime()

    /** Werte seit dem letzten Aufruf. */
    @Synchronized
    fun take(): Snapshot {
        val now = System.nanoTime()
        val dt = ((now - lastAt) / 1e9f).coerceAtLeast(0.001f)
        lastAt = now
        val frames = framesShown.getAndSet(0)
        val bytes = bytesIn.getAndSet(0)
        val dn = decodeNs.getAndSet(0)
        val dc = decodeCount.getAndSet(0)
        return Snapshot(
            fps = frames / dt,
            mbit = bytes * 8f / dt / 1e6f,
            decodeMs = if (dc > 0) dn / dc / 1e6f else 0f,
            usbMs = rttUs / 2000f,
            stream = stream,
        )
    }
}
