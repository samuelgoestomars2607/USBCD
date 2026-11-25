package de.samuel.usbcd

import java.io.DataInputStream
import java.io.DataOutputStream

/**
 * Drahtprotokoll – identisch mit host/src/proto.rs.
 * Jede Nachricht: [Typ: u8][Länge: u32 big-endian][Nutzdaten]
 */
object Proto {
    const val VIDEO_PORT = 27183
    const val AUDIO_PORT = 27184

    const val T_HELLO = 0x01
    const val T_CONFIG = 0x02
    const val T_VIDEO = 0x03
    const val T_SETTINGS = 0x04
    const val T_AUTH = 0x05
    const val T_CHALLENGE = 0x06
    const val T_AUTH_OK = 0x07
    const val T_INPUT = 0x08
    const val T_PING = 0x09
    const val T_PONG = 0x0A
    const val T_AFMT = 0x10
    const val T_PCM = 0x11
    /** PC → Tablet: Webcam-Steuerung (JSON) */
    const val T_CAM = 0x12
    /** Tablet → PC: Zustand der Selfie-Kamera (JSON) */
    const val T_CAM_STATE = 0x13
    /** Tablet → PC: [Flags][H.264] der Selfie-Kamera */
    const val T_CAM_DATA = 0x14
    /** Tablet → PC: Lautstärketaste [1 lauter, 2 leiser, 3 stumm] */
    const val T_VOLUME_KEY = 0x15
    /** PC → Tablet: Lautstärke des Tablet-Lautsprechers in Windows (JSON {level, muted}) */
    const val T_VOLUME = 0x16
    /** Tablet → PC: gibt der Dekoder jedes Bild sofort aus? (JSON {prompt}) */
    const val T_DECODER = 0x17
    /** Tablet → PC: bitte ein Schlüsselbild */
    const val T_KEY_REQUEST = 0x18
    /** PC → Tablet: verlustfreies Standbild [Breite u32][Höhe u32][Format u8][zlib-RGBA] */
    const val T_STILL = 0x19
    const val STILL_RGBA_ZLIB = 1

    const val VF_KEY = 1
    const val VF_CONFIG = 2

    const val MAX_FRAME = 64 shl 20

    fun writeFrame(out: DataOutputStream, type: Int, payload: ByteArray) {
        synchronized(out) {
            out.writeByte(type)
            out.writeInt(payload.size)
            out.write(payload)
            out.flush()
        }
    }

    /** Liest den Kopf: liefert (Typ, Länge). */
    fun readHeader(inp: DataInputStream, max: Int = MAX_FRAME): Pair<Int, Int> {
        val type = inp.readUnsignedByte()
        val len = inp.readInt()
        if (len < 0 || len > max) throw java.io.IOException("Nachricht zu groß: $len")
        return type to len
    }

    fun skip(inp: DataInputStream, len: Int) {
        var left = len
        while (left > 0) {
            val n = inp.skipBytes(left)
            if (n <= 0) {
                inp.readByte()
                left -= 1
            } else {
                left -= n
            }
        }
    }
}
