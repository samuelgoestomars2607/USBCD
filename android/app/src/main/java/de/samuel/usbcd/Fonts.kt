package de.samuel.usbcd

import android.content.Context
import android.graphics.Typeface

/** Space Grotesk (variable Schrift, Gewicht 300–700) für alle Texte der App. */
object Fonts {
    private val cache = HashMap<Int, Typeface>()

    fun get(context: Context, weight: Int): Typeface = synchronized(cache) {
        cache.getOrPut(weight) {
            try {
                Typeface.Builder(context.assets, "fonts/SpaceGrotesk.ttf")
                    .setFontVariationSettings("'wght' $weight")
                    .build()
            } catch (e: Exception) {
                Typeface.DEFAULT
            }
        }
    }
}
