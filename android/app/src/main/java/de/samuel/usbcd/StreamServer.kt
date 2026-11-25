package de.samuel.usbcd

import android.util.Log
import org.json.JSONObject
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket

/**
 * Lauscht auf 127.0.0.1 – der PC erreicht diese Ports über USB per `adb forward`.
 * Port 27183: Display-Infos senden, Video empfangen. Port 27184: Ton empfangen.
 */
class StreamServer(
    private val hello: JSONObject,
    private val video: VideoDecoder,
    private val audio: AudioPlayer,
    private val onState: (connected: Boolean, info: String) -> Unit,
    private val onSettings: (JSONObject) -> Unit,
    /** true: eine andere App belegt den USBCD-Anschluss */
    private val onBlocked: (Boolean) -> Unit,
    /** Webcam-Steuerung vom PC; null = Verbindung beendet */
    private val onCamera: (JSONObject?) -> Unit,
    /** Neue Windows-Lautstärke nach einer Lautstärketaste */
    private val onVolume: (JSONObject) -> Unit,
    /** Verlustfreies Standbild vom PC (Daten, Nummer des letzten Videobilds davor) */
    private val onStill: (ByteArray, Long) -> Unit = { _, _ -> },
    /** Standbild entfernen (neues Videobild, neue Einstellungen, Verbindung beendet) */
    private val onStillHide: () -> Unit = {},
) {
    /** Zähler der Videobilder: ein Standbild gilt nur, solange kein neueres kam */
    @Volatile var videoGen = 0L
        private set
    /** liegt gerade ein Standbild über dem Video? */
    val stillVisible = java.util.concurrent.atomic.AtomicBoolean(false)

    private fun hideStill() {
        if (stillVisible.getAndSet(false)) onStillHide()
    }

    @Volatile private var running = true
    /** Rückkanal der aktuellen Video-Verbindung (Touch/Stift, PONG) */
    @Volatile private var videoOut: DataOutputStream? = null
    private val servers = mutableListOf<ServerSocket>()
    private val threads = mutableListOf<Thread>()

    fun start() {
        threads += Thread({ serve(Proto.VIDEO_PORT, ::handleVideo) }, "video-server")
        threads += Thread({ serve(Proto.AUDIO_PORT, ::handleAudio) }, "audio-server")
        threads.forEach { it.start() }
    }

    /** Nachricht an den PC; false, wenn gerade keine Verbindung besteht. */
    fun send(type: Int, payload: ByteArray): Boolean {
        val out = videoOut ?: return false
        return try {
            Proto.writeFrame(out, type, payload)
            true
        } catch (e: Exception) {
            false
        }
    }

    fun stop() {
        running = false
        synchronized(servers) { servers.forEach { runCatching { it.close() } } }
        video.stop()
        audio.release()
    }

    private fun serve(port: Int, handler: (Socket, DataInputStream, DataOutputStream) -> Unit) {
        var failures = 0
        while (running) {
            val ss = try {
                ServerSocket().apply {
                    reuseAddress = true
                    bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), port), 1)
                }
            } catch (e: Exception) {
                Log.w(TAG, "Port $port belegt, neuer Versuch", e)
                if (++failures == 6) onBlocked(true)
                Thread.sleep(500)
                continue
            }
            if (failures >= 6) onBlocked(false)
            failures = 0
            synchronized(servers) { servers += ss }
            try {
                while (running) {
                    val s = ss.accept()
                    try {
                        s.tcpNoDelay = true
                        s.soTimeout = 5000
                        val inp = DataInputStream(BufferedInputStream(s.getInputStream(), 1 shl 20))
                        val out = DataOutputStream(BufferedOutputStream(s.getOutputStream()))
                        // erst anmelden – ohne gültige Anmeldung wird nichts gesendet oder angenommen
                        if (Auth.handshake(inp, out)) {
                            s.soTimeout = 0
                            handler(s, inp, out)
                        } else {
                            Log.w(TAG, "Port $port: Anmeldung fehlgeschlagen")
                        }
                    } catch (e: Exception) {
                        Log.i(TAG, "Verbindung $port beendet: ${e.message}")
                    } finally {
                        runCatching { s.close() }
                    }
                }
            } catch (e: Exception) {
                if (running) Log.w(TAG, "Server $port", e)
            } finally {
                runCatching { ss.close() }
                synchronized(servers) { servers -= ss }
            }
        }
    }

    @Suppress("UNUSED_PARAMETER")
    private fun handleVideo(s: Socket, inp: DataInputStream, out: DataOutputStream) {
        // Sobald das Bild läuft, schickt der PC jede Sekunde ein PING. Kommt danach 6 s lang
        // nichts (PC heruntergefahren, abgestürzt), Verbindung beenden statt das letzte Bild
        // stehen zu lassen. Vorher nicht: das Einschalten des Monitors dauert mitunter länger.
        var watchdog = false
        Proto.writeFrame(out, Proto.T_HELLO, hello.toString().toByteArray())
        videoOut = out
        try {
            while (running) {
                val (type, len) = Proto.readHeader(inp)
                when (type) {
                    Proto.T_CONFIG -> {
                        val buf = ByteArray(len)
                        inp.readFully(buf)
                        val c = JSONObject(String(buf))
                        val codec = c.getString("codec")
                        val w = c.getInt("width")
                        val h = c.getInt("height")
                        val fps = c.getInt("fps")
                        // gleiche Einstellungen (nur neue Bildqualität): Bild läuft einfach weiter
                        // erstes Verbinden dieses Tablets: Bild erst nach der Dekoder-Prüfung zeigen
                        val check = c.optBoolean("check", false)
                        hideStill()
                        if (video.configure(codec, w, h, fps, check)) {
                            Stats.stream = "${codec.uppercase()} · ${w}×${h} · $fps Hz"
                            onState(true, Stats.stream)
                        }
                    }
                    Proto.T_VIDEO -> {
                        // neues Bild: ein liegendes Standbild ist ab jetzt veraltet
                        videoGen++
                        hideStill()
                        video.feed(inp, len)
                    }
                    Proto.T_STILL -> {
                        val buf = ByteArray(len)
                        inp.readFully(buf)
                        Stats.bytesIn.addAndGet(len.toLong() + 5)
                        onStill(buf, videoGen)
                    }
                    Proto.T_PING -> {
                        val buf = ByteArray(len)
                        inp.readFully(buf)
                        if (!watchdog) {
                            watchdog = true
                            s.soTimeout = 6000
                        }
                        if (len >= 12) {
                            Stats.rttUs = java.nio.ByteBuffer.wrap(buf, 8, 4).int
                        }
                        if (len >= 8) Proto.writeFrame(out, Proto.T_PONG, buf.copyOf(8))
                    }
                    Proto.T_SETTINGS -> {
                        val buf = ByteArray(len)
                        inp.readFully(buf)
                        onSettings(JSONObject(String(buf)))
                    }
                    Proto.T_CAM -> {
                        val buf = ByteArray(len)
                        inp.readFully(buf)
                        onCamera(JSONObject(String(buf)))
                    }
                    Proto.T_VOLUME -> {
                        val buf = ByteArray(len)
                        inp.readFully(buf)
                        onVolume(JSONObject(String(buf)))
                    }
                    else -> Proto.skip(inp, len)
                }
            }
        } finally {
            videoOut = null
            hideStill()
            onCamera(null)
            video.stop()
            onState(false, "")
        }
    }

    @Suppress("UNUSED_PARAMETER")
    private fun handleAudio(s: Socket, inp: DataInputStream, out: DataOutputStream) {
        var buf = ByteArray(64 shl 10)
        try {
            while (running) {
                val (type, len) = Proto.readHeader(inp)
                when (type) {
                    Proto.T_AFMT -> {
                        val b = ByteArray(len)
                        inp.readFully(b)
                        val f = JSONObject(String(b))
                        audio.configure(f.getInt("rate"), f.getInt("channels"))
                    }
                    Proto.T_PCM -> {
                        if (buf.size < len) buf = ByteArray(len)
                        inp.readFully(buf, 0, len)
                        audio.write(buf, len)
                    }
                    else -> Proto.skip(inp, len)
                }
            }
        } finally {
            audio.release()
        }
    }

    companion object {
        private const val TAG = "USBCD"
    }
}
