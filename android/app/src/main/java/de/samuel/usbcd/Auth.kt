package de.samuel.usbcd

import android.content.Intent
import java.io.DataInputStream
import java.io.DataOutputStream
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * Gegenseitige Anmeldung mit dem PC – identisch mit host/src/auth.rs.
 *
 * Der PC startet die App über adb mit einem zufälligen Geheimnis (Extra "usbcd_token").
 * Angenommen wird es nur über den Einstieg ".Connect", den ausschließlich die adb-Shell
 * öffnen darf (geschützt durch android.permission.DUMP, das normale Apps nicht bekommen).
 * Das Geheimnis liegt nur im Arbeitsspeicher.
 *
 *   App → PC  CHALLENGE  nonce_a (32 Byte)
 *   PC → App  AUTH       nonce_p (32) ‖ HMAC-SHA256(token, "USBCD1 pc"  ‖ nonce_a ‖ nonce_p)
 *   App → PC  AUTH_OK    HMAC-SHA256(token, "USBCD1 app" ‖ nonce_a ‖ nonce_p)
 *
 * Prüfwert: token = 32×0x01, nonce_a = 32×0x02, nonce_p = 32×0x03 →
 *   pc  = df4ec950c482ab013447495990e812bfe402327668d75f18be995eecb7b8742f
 *   app = a12a2c51774ab7e8df0b9a8c1eeb91cf29a4d32ca3df526959340d706fa2dd54
 */
object Auth {
    private const val EXTRA = "usbcd_token"
    private const val ENTRY = ".Connect"
    private val random = SecureRandom()

    @Volatile private var token: ByteArray? = null

    /** Übernimmt das Geheimnis, wenn die App über den adb-Einstieg gestartet wurde. */
    fun takeFrom(intent: Intent?) {
        intent ?: return
        val hex = intent.getStringExtra(EXTRA)
        intent.removeExtra(EXTRA)
        val viaAdb = intent.component?.className?.endsWith(ENTRY) == true
        if (!viaAdb || hex == null) return
        parseHex(hex)?.let { token = it }
    }

    fun parseHex(hex: String): ByteArray? {
        if (hex.length != 64 || !hex.all { it in '0'..'9' || it in 'a'..'f' }) return null
        return ByteArray(32) { i -> hex.substring(2 * i, 2 * i + 2).toInt(16).toByte() }
    }

    fun mac(key: ByteArray, label: String, na: ByteArray, np: ByteArray): ByteArray {
        val m = Mac.getInstance("HmacSHA256")
        m.init(SecretKeySpec(key, "HmacSHA256"))
        m.update(label.toByteArray(Charsets.US_ASCII))
        m.update(na)
        m.update(np)
        return m.doFinal()
    }

    /** Server-Seite des Handshakes; true = der PC kennt unser Geheimnis. */
    fun handshake(inp: DataInputStream, out: DataOutputStream): Boolean {
        val na = ByteArray(32).also { random.nextBytes(it) }
        Proto.writeFrame(out, Proto.T_CHALLENGE, na)
        val (type, len) = Proto.readHeader(inp, 4096)
        if (type != Proto.T_AUTH || len != 64) return false
        val p = ByteArray(64)
        inp.readFully(p)
        val key = token ?: return false
        val np = p.copyOfRange(0, 32)
        if (!MessageDigest.isEqual(mac(key, "USBCD1 pc", na, np), p.copyOfRange(32, 64))) return false
        Proto.writeFrame(out, Proto.T_AUTH_OK, mac(key, "USBCD1 app", na, np))
        return true
    }
}
