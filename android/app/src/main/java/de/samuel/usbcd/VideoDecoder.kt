package de.samuel.usbcd

import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.util.Log
import android.view.Surface

/**
 * Hardware-Dekodierung direkt auf die SurfaceView – ohne Puffer/Verzögerung:
 * jedes fertige Bild wird sofort angezeigt.
 *
 * Prüft nebenbei, ob der Dekoder jedes Bild sofort ausgibt oder eins festhält, bis das
 * nächste kommt, und meldet das dem PC (DECODER). Nur bei sofortiger Ausgabe sendet der
 * PC Bilder „nur bei Änderungen“; sonst bliebe das letzte Bild hängen. Wartet der
 * Dekoder auf ein Schlüsselbild, fordert er eins an (KEY_REQUEST).
 */
class VideoDecoder(
    private val decoders: Map<String, DecoderChoice?>,
    private val onFirstFrame: () -> Unit,
) {
    private data class Config(val mime: String, val width: Int, val height: Int, val fps: Int)

    private val lock = Any()
    private var surface: Surface? = null
    private var config: Config? = null
    private var codec: MediaCodec? = null
    private var outThread: Thread? = null
    private var lastCsd: ByteArray? = null
    @Volatile private var needKey = true
    private var scratch = ByteArray(1 shl 20)
    /** Übergabe an den Decoder je Bild: Zeitpunkt und laufende Nummer */
    private class Queued(val at: Long, val seq: Long)
    private val queuedAt = object : LinkedHashMap<Long, Queued>(64, 0.75f, false) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Long, Queued>?) = size > 64
    }
    /** Nummer des zuletzt übergebenen Bildes */
    @Volatile private var lastSeq = 0L
    /**
     * Dekoder-Prüfung (nur beim ersten Verbinden, der PC schickt dabei alle 200 ms ein Bild):
     * Bilder gemessen / davon in < 100 ms ausgegeben. Hält der Dekoder Bilder fest, kommt
     * jedes erst mit dem nächsten heraus (~200 ms) – eindeutig unterscheidbar.
     */
    private var probeSkip = 0
    private var probeCount = 0
    private var probePrompt = 0
    /** im Betrieb: Ausgaben seit dem letzten Schlüsselbild-Neueinstieg */
    @Volatile private var outSinceResync = 0
    /** „hängt“ schon gemeldet (je Dekoder einmal) */
    private var stuckReported = false
    private var lastKeyRequest = 0L

    /** Meldungen an den PC (Typ, Nutzdaten); läuft nie im UI- oder Lese-Thread */
    var reporter: ((Int, ByteArray) -> Unit)? = null
    private val reports = java.util.concurrent.Executors.newSingleThreadExecutor()

    private fun report(type: Int, payload: ByteArray) {
        val r = reporter ?: return
        try {
            reports.execute { r(type, payload) }
        } catch (_: Exception) {
        }
    }

    /** Schlüsselbild anfordern (höchstens 1× je Sekunde). */
    private fun requestKey() {
        val now = System.nanoTime()
        if (now - lastKeyRequest < 1_000_000_000L) return
        lastKeyRequest = now
        report(Proto.T_KEY_REQUEST, ByteArray(0))
    }

    private fun reportPrompt(prompt: Boolean) {
        synchronized(reveal) {
            if (reveal.checking) {
                reveal.checking = false
                // PC beendet die Prüfung mit einem Encoder-Neustart (passende Übertragungsart):
                // Verbindungsbildschirm bis zum ersten Bild danach stehen lassen
                reveal.awaitRestart = true
                reveal.holdUntil = System.nanoTime() + 3_000_000_000L
            }
        }
        Log.i(TAG, "Dekoder gibt Bilder ${if (prompt) "sofort" else "verzögert"} aus")
        report(Proto.T_DECODER, "{\"prompt\":$prompt}".toByteArray())
        maybeReveal()
    }

    /**
     * Wann das Bild erscheint (Verbindungsbildschirm ausblenden). Beim ersten Verbinden eines
     * Tablets prüft die App erst den Dekoder; schaltet der PC danach um, gäbe es einen kurzen
     * Ruckler – der Verbindungsbildschirm bleibt deshalb bis nach der Umstellung stehen
     * (höchstens ein paar Sekunden).
     */
    private class Reveal {
        /** erstes Bild dieses Dekoders ausgegeben */
        var haveFrame = false
        /** Verbindungsbildschirm schon ausgeblendet */
        var shown = false
        /** Prüfung läuft (Anweisung des PCs beim ersten Verbinden) */
        var checking = false
        /** Prüfung bestanden, Encoder-Neustart am PC steht aus */
        var awaitRestart = false
        /** nach dem Neustart: erst ab diesem Bild (Schlüsselbild) zeigen; 0 = keins */
        var keySeq = 0L
        var waitKey = false
        /** längstens bis hierhin warten (nanoTime); 0 = nicht warten */
        var holdUntil = 0L
    }
    private val reveal = Reveal()

    /** Bild zeigen, sobald nichts mehr dagegen spricht (oder die Wartezeit um ist). */
    private fun maybeReveal() {
        val show = synchronized(reveal) {
            val now = System.nanoTime()
            val waiting = reveal.holdUntil != 0L && now < reveal.holdUntil
            if (!waiting) {
                reveal.checking = false
                reveal.awaitRestart = false
                reveal.waitKey = false
                reveal.holdUntil = 0L
            }
            if (!reveal.shown && reveal.haveFrame && !waiting) {
                reveal.shown = true
                true
            } else {
                false
            }
        }
        if (show) onFirstFrame()
    }

    fun setSurface(s: Surface?) = synchronized(lock) {
        surface = s
        recreate()
        // z. B. zurück in die App: neuer Dekoder braucht ein Schlüsselbild – bei
        // „nur bei Änderungen“ käme bei stillem Bild sonst keins
        if (codec != null) requestKey()
    }

    /**
     * true, wenn sich Codec/Auflösung/Bildrate geändert haben (sonst läuft der Decoder weiter).
     * `check`: erstes Verbinden – Bild erst nach der Dekoder-Prüfung zeigen.
     */
    fun configure(codecName: String, width: Int, height: Int, fps: Int, check: Boolean = false): Boolean = synchronized(lock) {
        val mime = if (codecName == "hevc") DisplayInfo.MIME_HEVC else DisplayInfo.MIME_AVC
        val next = Config(mime, width, height, fps)
        lastCsd = null
        if (next == config && codec != null) {
            // nur der Encoder am PC wurde neu gestartet (z. B. andere Bildqualität):
            // Decoder behalten, ab dem nächsten Schlüsselbild weiter – kein Flackern
            needKey = true
            synchronized(reveal) {
                if (reveal.awaitRestart) {
                    // die Umstellung nach der Prüfung: ab dem ersten Bild danach zeigen
                    reveal.awaitRestart = false
                    reveal.waitKey = true
                    reveal.keySeq = 0L
                    reveal.holdUntil = System.nanoTime() + 3_000_000_000L
                }
            }
            return false
        }
        config = next
        synchronized(reveal) {
            reveal.shown = false
            reveal.haveFrame = false
            reveal.awaitRestart = false
            reveal.waitKey = false
            reveal.checking = check
            // Prüfung dauert ~3 s (180 Bilder); spätestens nach 8 s trotzdem zeigen
            reveal.holdUntil = if (check) System.nanoTime() + 8_000_000_000L else 0L
        }
        recreate()
        true
    }

    fun stop() = synchronized(lock) {
        config = null
        releaseCodec()
    }

    /** Liest `len` Bytes Nutzdaten einer VIDEO-Nachricht vom Strom und dekodiert sie. */
    fun feed(inp: java.io.DataInputStream, len: Int) {
        val flags = inp.readUnsignedByte()
        val pts = inp.readLong()
        val n = len - 9
        if (n <= 0) return
        if (scratch.size < n) scratch = ByteArray(n + (n shr 2))
        inp.readFully(scratch, 0, n)
        Stats.bytesIn.addAndGet(len.toLong() + 5)

        synchronized(lock) {
            if (flags and Proto.VF_CONFIG != 0) {
                lastCsd = scratch.copyOf(n)
                if (codec != null && !needKey) queue(scratch, n, pts, MediaCodec.BUFFER_FLAG_CODEC_CONFIG)
                return
            }
            val c = codec ?: return
            val key = flags and Proto.VF_KEY != 0
            if (needKey) {
                if (!key) {
                    requestKey()
                    return // nach (Neu-)Start erst ab Schlüsselbild
                }
                lastCsd?.let { queue(it, it.size, pts, MediaCodec.BUFFER_FLAG_CODEC_CONFIG) }
                needKey = false
                outSinceResync = 0
                synchronized(reveal) {
                    // dieses Schlüsselbild bekommt die nächste Nummer
                    if (reveal.waitKey && reveal.keySeq == 0L) reveal.keySeq = lastSeq + 1
                }
            }
            if (!queue(scratch, n, pts, if (key) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)) {
                // Decoder kommt nicht hinterher → bis zum nächsten Schlüsselbild verwerfen
                needKey = true
                requestKey()
                Log.w(TAG, "Eingabepuffer voll – warte auf Schlüsselbild (${c.name})")
            }
        }
    }

    private fun queue(data: ByteArray, n: Int, pts: Long, flags: Int): Boolean {
        val c = codec ?: return false
        return try {
            var idx = -1
            for (i in 0 until 10) { // max. ~100 ms warten
                idx = c.dequeueInputBuffer(10_000)
                if (idx >= 0) break
            }
            if (idx < 0) return false
            val buf = c.getInputBuffer(idx) ?: return false
            if (buf.capacity() < n) {
                Log.w(TAG, "Bild zu groß für Eingabepuffer: $n > ${buf.capacity()}")
                c.queueInputBuffer(idx, 0, 0, pts, 0)
                return false
            }
            buf.clear()
            buf.put(data, 0, n)
            if (flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG == 0) {
                synchronized(queuedAt) {
                    val seq = lastSeq + 1
                    queuedAt[pts] = Queued(System.nanoTime(), seq)
                    lastSeq = seq
                }
            }
            c.queueInputBuffer(idx, 0, n, pts, flags)
            true
        } catch (e: Exception) {
            Log.e(TAG, "queueInputBuffer", e)
            needKey = true
            false
        }
    }

    private fun recreate() {
        releaseCodec()
        val cfg = config ?: return
        val s = surface ?: return
        if (!s.isValid) return
        val key = if (cfg.mime == DisplayInfo.MIME_HEVC) "hevc" else "avc"
        val name = decoders[key]?.name

        val c = try {
            if (name != null) MediaCodec.createByCodecName(name) else MediaCodec.createDecoderByType(cfg.mime)
        } catch (e: Exception) {
            Log.e(TAG, "Decoder $name nicht erstellbar, nehme Standard", e)
            MediaCodec.createDecoderByType(cfg.mime)
        }
        // Erst mit allen Latenz-Optionen versuchen, sonst schlicht.
        val configured = try {
            c.configure(format(cfg, extended = true), s, null, 0)
            true
        } catch (e: Exception) {
            Log.w(TAG, "configure mit Latenz-Optionen fehlgeschlagen, versuche einfach", e)
            false
        }
        if (!configured) {
            c.reset()
            c.configure(format(cfg, extended = false), s, null, 0)
        }
        // kleinere Auflösung vom PC (gleiches Seitenverhältnis) auf den ganzen Bildschirm ziehen –
        // das Hochrechnen übernimmt die Hardware des Tablets
        c.setVideoScalingMode(MediaCodec.VIDEO_SCALING_MODE_SCALE_TO_FIT)
        c.start()
        codec = c
        needKey = true
        synchronized(queuedAt) {
            queuedAt.clear()
            probeSkip = 0
            probeCount = 0
            probePrompt = 0
        }
        outSinceResync = 0
        stuckReported = false
        Log.i(TAG, "Decoder ${c.name} läuft: ${cfg.width}x${cfg.height} @ ${cfg.fps}")

        val t = Thread({ drain(c) }, "video-out")
        t.priority = Thread.MAX_PRIORITY
        t.start()
        outThread = t
    }

    private fun format(cfg: Config, extended: Boolean): MediaFormat {
        val f = MediaFormat.createVideoFormat(cfg.mime, cfg.width, cfg.height)
        f.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, maxOf(cfg.width * cfg.height, 2 shl 20))
        if (Build.VERSION.SDK_INT >= 30) f.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
        if (extended) {
            f.setInteger(MediaFormat.KEY_PRIORITY, 0) // Echtzeit
            f.setInteger(MediaFormat.KEY_OPERATING_RATE, cfg.fps)
            // herstellerspezifische Niedriglatenz-Schalter (werden ignoriert, wenn unbekannt)
            f.setInteger("vendor.qti-ext-dec-low-latency.enable", 1)
            f.setInteger("vendor.qti-ext-dec-picture-order.enable", 1)
            f.setInteger("vendor.rtc-ext-dec-low-latency.enable", 1)
            f.setInteger("vdec-lowlatency", 1)
        }
        return f
    }

    private fun drain(c: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        synchronized(reveal) {
            reveal.haveFrame = false
            reveal.shown = false
        }
        while (true) {
            val idx = try {
                c.dequeueOutputBuffer(info, 10_000)
            } catch (e: Exception) {
                break // Decoder wurde freigegeben
            }
            if (idx >= 0) {
                val q = synchronized(queuedAt) { queuedAt.remove(info.presentationTimeUs) }
                if (q != null) {
                    Stats.decodeNs.addAndGet(System.nanoTime() - q.at)
                    Stats.decodeCount.incrementAndGet()
                    checkPrompt(q)
                }
                outSinceResync++
                Stats.framesShown.incrementAndGet()
                try {
                    c.releaseOutputBuffer(idx, true) // sofort anzeigen
                } catch (e: Exception) {
                    break
                }
                synchronized(reveal) {
                    if (!reveal.waitKey) {
                        reveal.haveFrame = true
                    } else if (reveal.keySeq != 0L && q != null && q.seq >= reveal.keySeq) {
                        // erstes Bild nach der Umstellung: jetzt zeigen
                        reveal.waitKey = false
                        reveal.keySeq = 0L
                        reveal.holdUntil = 0L
                        reveal.haveFrame = true
                    }
                }
                maybeReveal()
            }
            if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) break
            if (idx < 0) {
                checkStuck()
                maybeReveal() // Wartezeit abgelaufen?
            }
        }
    }

    /** Nur während der Prüfung: Zeit vom Übergeben bis zur Ausgabe; Urteil nach 10 Bildern. */
    private fun checkPrompt(q: Queued) {
        if (!synchronized(reveal) { reveal.checking }) return
        val verdict: Boolean? = synchronized(queuedAt) {
            // die ersten Bilder (Anlaufen des Dekoders) nicht werten
            if (probeSkip < 3) {
                probeSkip++
                return@synchronized null
            }
            probeCount++
            if (System.nanoTime() - q.at < 100_000_000L) probePrompt++
            if (probeCount < 10) null else probePrompt >= 9
        }
        if (verdict != null) reportPrompt(verdict)
    }

    /**
     * Sicherung im Betrieb: Das neueste Bild steckt seit > 250 ms im Dekoder, obwohl er seit
     * dem letzten Schlüsselbild schon Bilder ausgegeben hat und nichts nachkam → er hält es
     * fest. Dann zurück zum gleichmäßigen Bildstrom (meldet je Dekoder einmal).
     */
    private fun checkStuck() {
        if (stuckReported || needKey || outSinceResync == 0) return
        if (synchronized(reveal) { reveal.checking }) return
        val stuck = synchronized(queuedAt) {
            val newest = queuedAt.values.lastOrNull() ?: return
            newest.seq == lastSeq && System.nanoTime() - newest.at > 250_000_000L
        }
        if (stuck) {
            stuckReported = true
            synchronized(queuedAt) { queuedAt.clear() }
            Log.w(TAG, "Bild hängt im Dekoder")
            report(Proto.T_DECODER, "{\"prompt\":false}".toByteArray())
        }
    }

    private fun releaseCodec() {
        val c = codec ?: return
        codec = null
        try {
            c.stop()
        } catch (_: Exception) {
        }
        try {
            c.release()
        } catch (_: Exception) {
        }
        outThread?.join(500)
        outThread = null
    }

    companion object {
        private const val TAG = "USBCD"
    }
}
