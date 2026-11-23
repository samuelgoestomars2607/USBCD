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
        return JSONObject()
            .put("proto", 1)
            .put("model", "${Build.MANUFACTURER} ${Build.MODEL}")
            .put("name", name)
            .put("width", w)
            .put("height", h)
            .put("refresh", mode.refreshRate.toDouble())
            .put("density_dpi", densityDpi)
            .put("codecs", codecs)
    }
}
