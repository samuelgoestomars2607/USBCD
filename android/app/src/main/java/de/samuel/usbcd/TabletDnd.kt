package de.samuel.usbcd

import android.app.Activity
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.provider.Settings
import android.util.Log
import android.widget.Toast

/**
 * „Nicht stören“, solange das Tablet als Monitor läuft (Schalter im USBCD-Menü am PC).
 *
 * - Schaltet nur ein, wenn „Nicht stören“ vorher aus war – hat man es selbst schon an,
 *   bleibt alles, wie es ist.
 * - Schaltet beim Trennen nur wieder aus, wenn noch der von USBCD gesetzte Zustand gilt
 *   (hat man es zwischendurch selbst geändert, bleibt die eigene Wahl).
 * - Was stumm bzw. ausgeblendet wird, bestimmen die „Nicht stören“-Einstellungen des Tablets
 *   (Ausnahmen wie Wecker gelten weiter).
 * - Die nötige Erlaubnis wird nur einmal erfragt, nachdem der Schalter am PC eingeschaltet wurde.
 */
class TabletDnd(private val activity: Activity) {
    private val nm = activity.getSystemService(NotificationManager::class.java)
    private val prefs = activity.getSharedPreferences("usbcd", Context.MODE_PRIVATE)
    /** Schalter am PC */
    private var wanted = false
    /** Monitorbetrieb läuft */
    private var active = false

    private fun granted(): Boolean = nm?.isNotificationPolicyAccessGranted == true

    /** Einstellung vom PC. */
    fun setWanted(on: Boolean) {
        if (on && !wanted && !granted() && !prefs.getBoolean(KEY_ASKED, false)) {
            prefs.edit().putBoolean(KEY_ASKED, true).apply()
            askPermission()
        }
        if (!on) prefs.edit().putBoolean(KEY_ASKED, false).apply() // beim nächsten Einschalten wieder fragen
        wanted = on
        update()
    }

    /** Verbindung zum PC steht bzw. ist beendet. */
    fun setActive(on: Boolean) {
        active = on
        update()
    }

    /** Nach der Rückkehr aus den Einstellungen (Erlaubnis evtl. erteilt). */
    fun onResume() = update()

    /** Beim Start (Rest nach Absturz) und beim Beenden: eigenen Zustand zurücknehmen. */
    fun release() {
        if (!prefs.getBoolean(KEY_SET, false)) return
        try {
            if (granted() && nm?.currentInterruptionFilter == NotificationManager.INTERRUPTION_FILTER_PRIORITY) {
                nm.setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_ALL)
                Log.i(TAG, "Nicht stören: wieder aus")
            }
        } catch (e: Exception) {
            Log.w(TAG, "Nicht stören zurücksetzen", e)
        }
        prefs.edit().putBoolean(KEY_SET, false).apply()
    }

    private fun update() {
        if (wanted && active) engage() else release()
    }

    private fun engage() {
        if (prefs.getBoolean(KEY_SET, false) || !granted()) return
        try {
            // selbst schon eingeschaltet (oder anderer Modus): nicht anfassen
            if (nm?.currentInterruptionFilter != NotificationManager.INTERRUPTION_FILTER_ALL) return
            prefs.edit().putBoolean(KEY_SET, true).commit()
            nm.setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_PRIORITY)
            Log.i(TAG, "Nicht stören: an")
        } catch (e: Exception) {
            prefs.edit().putBoolean(KEY_SET, false).apply()
            Log.w(TAG, "Nicht stören einschalten", e)
        }
    }

    private fun askPermission() {
        try {
            Toast.makeText(activity, R.string.dnd_permission, Toast.LENGTH_LONG).show()
            activity.startActivity(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS))
        } catch (e: Exception) {
            Log.w(TAG, "Einstellungen für „Nicht stören“ nicht verfügbar", e)
        }
    }

    companion object {
        private const val TAG = "USBCD"
        private const val KEY_SET = "dnd_set_by_usbcd"
        private const val KEY_ASKED = "dnd_permission_asked"
    }
}
