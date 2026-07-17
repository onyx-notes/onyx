// Onyx secret storage for Android.
//
// Plain entries: one AndroidKeyStore AES-GCM master key ("onyx.secrets.
// master", hardware-backed where available) encrypts each value; the
// iv||ciphertext blobs live in a private SharedPreferences file. This is
// the modern replacement for the deprecated androidx.security.crypto.
//
// Protected entries: one keystore key *per entry* created with
// setUserAuthenticationRequired(true) and invalidated on biometric
// enrollment change. Both storing and reading run the cipher through a
// BiometricPrompt CryptoObject, so the OS enforces "a currently enrolled
// biometric was presented" — the app never sees a bypass path.

package app.onyx.plugins.secrets

import android.app.Activity
import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class KeyArgs {
    lateinit var key: String
}

@InvokeArg
class SetArgs {
    lateinit var key: String
    lateinit var value: String
}

@InvokeArg
class ProtectedGetArgs {
    lateinit var key: String
    var reason: String = ""
}

@InvokeArg
class ProtectedSetArgs {
    lateinit var key: String
    lateinit var value: String
    var reason: String = ""
}

@TauriPlugin
class SecretsPlugin(private val activity: Activity) : Plugin(activity) {
    companion object {
        private const val KEYSTORE = "AndroidKeyStore"
        private const val MASTER_ALIAS = "onyx.secrets.master"
        private const val BIO_ALIAS_PREFIX = "onyx.secrets.bio."
        private const val PREFS = "onyx.secrets"
        private const val BIO_PREFIX = "bio."
        private const val GCM_TAG_BITS = 128
    }

    private val prefs: SharedPreferences by lazy {
        activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    }

    private fun keyStore(): KeyStore =
        KeyStore.getInstance(KEYSTORE).apply { load(null) }

    // ----- plain entries -------------------------------------------------

    private fun masterKey(): SecretKey {
        val store = keyStore()
        (store.getKey(MASTER_ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                MASTER_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build()
        )
        return generator.generateKey()
    }

    private fun encrypt(key: SecretKey, plaintext: ByteArray): String {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val sealed = cipher.doFinal(plaintext)
        return Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + ":" +
            Base64.encodeToString(sealed, Base64.NO_WRAP)
    }

    private fun decrypt(key: SecretKey, stored: String): ByteArray? {
        val parts = stored.split(":")
        if (parts.size != 2) return null
        val iv = Base64.decode(parts[0], Base64.NO_WRAP)
        val sealed = Base64.decode(parts[1], Base64.NO_WRAP)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, iv))
        return cipher.doFinal(sealed)
    }

    @Command
    fun available(invoke: Invoke) {
        val biometric = BiometricManager.from(activity)
            .canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG) ==
            BiometricManager.BIOMETRIC_SUCCESS
        val result = JSObject()
        result.put("secure", true)
        result.put("biometric", biometric)
        invoke.resolve(result)
    }

    @Command
    fun set(invoke: Invoke) {
        val args = invoke.parseArgs(SetArgs::class.java)
        try {
            val sealed = encrypt(masterKey(), args.value.toByteArray(Charsets.UTF_8))
            prefs.edit().putString(args.key, sealed).apply()
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("failed to store secret: ${e.message}")
        }
    }

    @Command
    fun get(invoke: Invoke) {
        val args = invoke.parseArgs(KeyArgs::class.java)
        val result = JSObject()
        try {
            val stored = prefs.getString(args.key, null)
            val plain = stored?.let { decrypt(masterKey(), it) }
            result.put("value", plain?.toString(Charsets.UTF_8))
        } catch (e: Exception) {
            // Corrupt blob or wiped keystore: treat as absent, not fatal.
            result.put("value", null)
        }
        invoke.resolve(result)
    }

    @Command
    fun delete(invoke: Invoke) {
        val args = invoke.parseArgs(KeyArgs::class.java)
        prefs.edit().remove(args.key).apply()
        invoke.resolve()
    }

    // ----- biometric-bound entries ---------------------------------------

    private fun bioAlias(key: String) = BIO_ALIAS_PREFIX + key

    private fun createBioKey(alias: String): SecretKey {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        val builder = KeyGenParameterSpec.Builder(
            alias,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setUserAuthenticationRequired(true)
            // Enrollment change (new finger/face) invalidates the key: a
            // freshly enrolled biometric must not unlock old vault keys.
            .setInvalidatedByBiometricEnrollment(true)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            builder.setUserAuthenticationParameters(
                0,
                KeyProperties.AUTH_BIOMETRIC_STRONG
            )
        } else {
            @Suppress("DEPRECATION")
            builder.setUserAuthenticationValidityDurationSeconds(-1)
        }
        generator.init(builder.build())
        return generator.generateKey()
    }

    /** Run `cipher` through a biometric prompt, then `onDone(cipher)`. */
    private fun promptFor(
        invoke: Invoke,
        title: String,
        cipher: Cipher,
        onDone: (Cipher) -> Unit,
    ) {
        val fragmentActivity = activity as? FragmentActivity
        if (fragmentActivity == null) {
            invoke.reject("biometric prompt needs a FragmentActivity host")
            return
        }
        ContextCompat.getMainExecutor(activity).execute {
            val prompt = BiometricPrompt(
                fragmentActivity,
                ContextCompat.getMainExecutor(activity),
                object : BiometricPrompt.AuthenticationCallback() {
                    override fun onAuthenticationSucceeded(
                        result: BiometricPrompt.AuthenticationResult
                    ) {
                        val authorized = result.cryptoObject?.cipher
                        if (authorized == null) {
                            invoke.reject("biometric prompt returned no cipher")
                            return
                        }
                        try {
                            onDone(authorized)
                        } catch (e: Exception) {
                            invoke.reject("crypto failure after biometric: ${e.message}")
                        }
                    }

                    override fun onAuthenticationError(code: Int, message: CharSequence) {
                        invoke.reject("biometric: $message")
                    }
                }
            )
            val info = BiometricPrompt.PromptInfo.Builder()
                .setTitle(title)
                .setNegativeButtonText(activity.getString(android.R.string.cancel))
                .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                .build()
            prompt.authenticate(info, BiometricPrompt.CryptoObject(cipher))
        }
    }

    @Command
    fun setProtected(invoke: Invoke) {
        val args = invoke.parseArgs(ProtectedSetArgs::class.java)
        try {
            val alias = bioAlias(args.key)
            // Replace any previous key: old entries die with it.
            keyStore().deleteEntry(alias)
            val key = createBioKey(alias)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, key)
            promptFor(invoke, args.reason.ifEmpty { "Onyx" }, cipher) { authorized ->
                val sealed = authorized.doFinal(args.value.toByteArray(Charsets.UTF_8))
                val stored =
                    Base64.encodeToString(authorized.iv, Base64.NO_WRAP) + ":" +
                        Base64.encodeToString(sealed, Base64.NO_WRAP)
                prefs.edit().putString(BIO_PREFIX + args.key, stored).apply()
                invoke.resolve()
            }
        } catch (e: Exception) {
            invoke.reject("failed to enroll protected secret: ${e.message}")
        }
    }

    @Command
    fun getProtected(invoke: Invoke) {
        val args = invoke.parseArgs(ProtectedGetArgs::class.java)
        val stored = prefs.getString(BIO_PREFIX + args.key, null)
        if (stored == null) {
            val result = JSObject()
            result.put("value", null)
            invoke.resolve(result)
            return
        }
        try {
            val parts = stored.split(":")
            val iv = Base64.decode(parts[0], Base64.NO_WRAP)
            val sealed = Base64.decode(parts[1], Base64.NO_WRAP)
            val key = keyStore().getKey(bioAlias(args.key), null) as? SecretKey
                ?: throw IllegalStateException("keystore entry missing")
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, iv))
            promptFor(invoke, args.reason.ifEmpty { "Onyx" }, cipher) { authorized ->
                val plain = authorized.doFinal(sealed)
                val result = JSObject()
                result.put("value", plain.toString(Charsets.UTF_8))
                invoke.resolve(result)
            }
        } catch (e: KeyPermanentlyInvalidatedException) {
            // Biometric enrollment changed: the entry is gone by design.
            prefs.edit().remove(BIO_PREFIX + args.key).apply()
            keyStore().deleteEntry(bioAlias(args.key))
            invoke.reject("biometric enrollment changed; secret invalidated")
        } catch (e: Exception) {
            invoke.reject("failed to read protected secret: ${e.message}")
        }
    }

    @Command
    fun deleteProtected(invoke: Invoke) {
        val args = invoke.parseArgs(KeyArgs::class.java)
        prefs.edit().remove(BIO_PREFIX + args.key).apply()
        try {
            keyStore().deleteEntry(bioAlias(args.key))
        } catch (_: Exception) {
            // Missing alias is fine.
        }
        invoke.resolve()
    }
}
