package de.samuel.usbcd

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.util.Log

/**
 * Spielt den PC-Ton über die Tablet-Lautsprecher – mit kleinem Puffer, damit
 * Ton und Bild synchron bleiben. Läuft der Puffer zu voll (Takt-Drift),
 * werden einzelne Häppchen verworfen.
 */
class AudioPlayer(@Suppress("unused") private val context: Context) {
    private var track: AudioTrack? = null
    private var rate = 48000
    private var channels = 2
    private var written = 0L

    fun configure(sampleRate: Int, ch: Int) {
        release()
        rate = sampleRate
        channels = if (ch >= 2) 2 else 1
        val mask = if (channels == 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val minBuf = AudioTrack.getMinBufferSize(rate, mask, AudioFormat.ENCODING_PCM_16BIT)
        val target = rate * channels * 2 * 40 / 1000 // ~40 ms
        val t = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(rate)
                    .setChannelMask(mask)
                    .build()
            )
            .setBufferSizeInBytes(maxOf(minBuf, target))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
            .build()
        t.play()
        track = t
        written = 0

        // Die Tablet-Lautstärke bleibt, wie der Nutzer sie eingestellt hat
        // (wichtig z. B. mit Kopfhörer am Tablet).
        Log.i(TAG, "Audio: $rate Hz, $channels Kanäle")
    }

    fun write(data: ByteArray, len: Int) {
        val t = track ?: return
        val frames = len / (2 * channels)
        // playbackHeadPosition läuft nach 2^32 Frames (~25 h bei 48 kHz) über – modulo rechnen,
        // sonst würde nach einem Tag Dauerbetrieb jeder Ton verworfen
        val played = t.playbackHeadPosition.toLong() and 0xFFFFFFFFL
        val queued = (written - played) and 0xFFFFFFFFL
        if (queued > rate * 120L / 1000) return // > 120 ms im Puffer → verwerfen
        val n = t.write(data, 0, len, AudioTrack.WRITE_BLOCKING)
        if (n > 0) written += frames
    }

    fun release() {
        track?.let {
            try {
                it.pause()
                it.flush()
                it.release()
            } catch (_: Exception) {
            }
        }
        track = null
    }

    companion object {
        private const val TAG = "USBCD"
    }
}
