package de.samuel.usbcd

import android.media.MediaCodecList
import android.os.Build
import android.view.Display
import org.json.JSONObject
import kotlin.math.max
import kotlin.math.min

/** Ein Hardware-Decoder und was er bei nativer Auflösung schafft. */
data class DecoderChoice(
    val mime: String,
    val name: String,
    val hardware: Boolean,
    val maxFps: Double,
)

/** Was ein Decoder bei einer bestimmten Größe schafft (Liste für den PC). */
data class DecoderMode(
    val codec: String,
    val name: String,
    val hardware: Boolean,
    val width: Int,
    val height: Int,
    val fps: Double,
)

object DisplayInfo {
    const val MIME_HEVC = "video/hevc"
    const val MIME_AVC = "video/avc"

    /** Höchste Auflösung, bei Gleichstand die höchste Bildrate. */
    fun bestMode(display: Display): Display.Mode =
        display.supportedModes.maxWith(
            compareBy<Display.Mode>({ it.physicalWidth.toLong() * it.physicalHeight }, { it.refreshRate })
        )

    /** Querformat-Größe (Breite ≥ Höhe) eines Modus. */
    fun landscapeSize(mode: Display.Mode): Pair<Int, Int> {
        val w = max(mode.physicalWidth, mode.physicalHeight)
        val h = min(mode.physicalWidth, mode.physicalHeight)
        return w to h
    }

    private fun isHardware(info: android.media.MediaCodecInfo): Boolean =
        if (Build.VERSION.SDK_INT >= 29) {
            info.isHardwareAccelerated
        } else {
            val n = info.name.lowercase()
            !(n.startsWith("omx.google.") || n.startsWith("c2.android.") || n.contains(".sw."))
        }

    /** Besten Decoder für mime bei w×h suchen (Hardware bevorzugt, dann höchste Bildrate). */
    fun findDecoder(mime: String, w: Int, h: Int): DecoderChoice? {
        var best: DecoderChoice? = null
        for (info in MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos) {
            if (info.isEncoder) continue
            if (info.supportedTypes.none { it.equals(mime, ignoreCase = true) }) continue
            val vc = try {
                info.getCapabilitiesForType(mime).videoCapabilities
            } catch (e: Exception) {
                null
            } ?: continue
            if (!vc.isSizeSupported(w, h)) continue
            val fps = try {
                vc.getSupportedFrameRatesFor(w, h).upper
            } catch (e: Exception) {
                0.0
            }
            val c = DecoderChoice(mime, info.name, isHardware(info), fps)
            val b = best
            if (b == null ||
                (c.hardware && !b.hardware) ||
                (c.hardware == b.hardware && c.maxFps > b.maxFps)
            ) {
                best = c
            }
        }
        return best
    }

    /**
     * Was jeder Decoder (Hardware und Software, HEVC und H.264) im Seitenverhältnis des
     * Displays schafft: die größte unterstützte Größe und ein paar kleinere Stufen, jeweils
     * mit Bildrate. Der PC wählt daraus und weicht im Betrieb aus, wenn eine Stufe scheitert.
     * Hardware: theoretische Bildrate (wie bisher); Software: gemessene, falls vorhanden –
     * die theoretische ist bei Software-Decodern viel zu optimistisch.
     */
    fun decoderModes(w: Int, h: Int): List<DecoderMode> {
        val out = ArrayList<DecoderMode>()
        for ((mime, key) in listOf(MIME_HEVC to "hevc", MIME_AVC to "avc")) {
            data class Cand(val name: String, val hw: Boolean, val sizes: List<Triple<Int, Int, Double>>)
            val cands = ArrayList<Cand>()
            for (info in MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos) {
                if (info.isEncoder) continue
                if (Build.VERSION.SDK_INT >= 29 && info.isAlias) continue
                if (info.supportedTypes.none { it.equals(mime, ignoreCase = true) }) continue
                val vc = try {
                    info.getCapabilitiesForType(mime).videoCapabilities
                } catch (e: Exception) {
                    null
                } ?: continue
                val hw = isHardware(info)
                val sizes = candidateSizes(vc, w, h).map { (cw, ch) -> Triple(cw, ch, rate(vc, cw, ch, hw)) }
                if (sizes.isNotEmpty()) cands.add(Cand(info.name, hw, sizes))
            }
            // je Codec höchstens zwei Hardware- und einen Software-Decoder (die mit der größten Stufe)
            // wie bisher: bei gleicher Größe der mit der höchsten Bildrate zuerst
            val byArea = compareByDescending<Cand> { it.sizes.first().first.toLong() * it.sizes.first().second }
                .thenByDescending { it.sizes.first().third }
            val chosen = cands.filter { it.hw }.sortedWith(byArea).take(2) +
                cands.filter { !it.hw }.sortedWith(byArea).take(1)
            for (c in chosen) {
                for ((cw, ch, fps) in c.sizes) out.add(DecoderMode(key, c.name, c.hw, cw, ch, fps))
            }
        }
        return out
    }

    private fun lcm8(a: Int): Int {
        val x = maxOf(1, a)
        var g = 8
        var b = x
        while (b != 0) { val t = g % b; g = b; b = t }
        return 8 / g * x
    }

    /** Größte unterstützte Größe im Seitenverhältnis w:h, dazu kleinere Stufen (80/64/50 %). */
    private fun candidateSizes(vc: android.media.MediaCodecInfo.VideoCapabilities, w: Int, h: Int): List<Pair<Int, Int>> {
        val aw = lcm8(vc.widthAlignment)
        val ah = lcm8(vc.heightAlignment)
        fun sizeFor(hh: Int): Pair<Int, Int> {
            val ch = hh / ah * ah
            val cw = ((ch.toLong() * w / h).toInt()) / aw * aw
            return cw to ch
        }
        fun ok(s: Pair<Int, Int>) = s.first >= 64 && s.second >= 64 &&
            try { vc.isSizeSupported(s.first, s.second) } catch (e: Exception) { false }
        // größte Stufe: native Größe, sonst schrittweise kleiner bis etwa 360 Zeilen
        var fit: Pair<Int, Int>? = if (ok(w to h)) w to h else null
        if (fit == null) {
            var hh = h / ah * ah
            while (hh >= 360) {
                val s = sizeFor(hh)
                if (ok(s)) { fit = s; break }
                hh -= ah
            }
        }
        val top = fit ?: return emptyList()
        val list = arrayListOf(top)
        for (f in listOf(0.8, 0.64, 0.5)) {
            val s = sizeFor((h * f).toInt())
            if (s.first.toLong() * s.second < top.first.toLong() * top.second && ok(s) && s !in list) list.add(s)
        }
        return list
    }

    private fun rate(vc: android.media.MediaCodecInfo.VideoCapabilities, w: Int, h: Int, hw: Boolean): Double {
        val supported = try { vc.getSupportedFrameRatesFor(w, h).upper } catch (e: Exception) { 0.0 }
        if (hw) return supported
        val measured = try { vc.getAchievableFrameRatesFor(w, h)?.upper } catch (e: Exception) { null }
        return if (measured != null && measured > 0) minOf(measured, if (supported > 0) supported else measured) else supported
    }

    /** Gerätename wie in den Einstellungen (z. B. „Galaxy Tab S6 Lite“), sonst Modell. */
    fun deviceName(context: android.content.Context): String {
        val n = try {
            android.provider.Settings.Global.getString(context.contentResolver, android.provider.Settings.Global.DEVICE_NAME)
        } catch (e: Exception) {
            null
        }
        return if (n.isNullOrBlank()) "${Build.MANUFACTURER} ${Build.MODEL}" else n
    }

    fun helloJson(
        mode: Display.Mode,
        densityDpi: Int,
        decoders: Map<String, DecoderChoice?>,
        name: String,
        modes: List<DecoderMode>,
        appVersion: Long,
    ): JSONObject {
        val (w, h) = landscapeSize(mode)
        val codecs = JSONObject()
        for ((key, d) in decoders) {
            codecs.put(
                key,
                JSONObject()
                    .put("supported", d != null)
                    .put("hardware", d?.hardware ?: false)
                    .put("name", d?.name ?: "")
                    .put("max_fps", d?.maxFps ?: 0.0)
            )
        }
        val list = org.json.JSONArray()
        for (m in modes) {
            list.put(
                JSONObject()
                    .put("codec", m.codec)
                    .put("decoder", m.name)
                    .put("hw", m.hardware)
                    .put("width", m.width)
                    .put("height", m.height)
                    .put("fps", m.fps)
            )
        }
        return JSONObject()
            .put("proto", 2)
            .put("model", "${Build.MANUFACTURER} ${Build.MODEL}")
            .put("name", name)
            .put("width", w)
            .put("height", h)
            .put("refresh", mode.refreshRate.toDouble())
            .put("density_dpi", densityDpi)
            .put("codecs", codecs)
            .put("modes", list)
            .put("app_version", appVersion)
            // ändert sich mit jedem Android-Update: dann prüft der PC wieder von der besten Stufe
            .put("build", Build.FINGERPRINT.take(200))
    }
}
