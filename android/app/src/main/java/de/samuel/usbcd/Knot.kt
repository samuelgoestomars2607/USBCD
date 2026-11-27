package de.samuel.usbcd

import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.pow
import kotlin.math.sin
import kotlin.math.sqrt

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
