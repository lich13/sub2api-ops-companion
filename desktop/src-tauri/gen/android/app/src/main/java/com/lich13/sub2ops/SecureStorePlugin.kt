package com.lich13.sub2ops

import android.app.Activity
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import android.util.Base64
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONObject

@InvokeArg
class CredentialArgs {
    lateinit var base: String
    var key: String? = null
}

// Invoked from Rust only; no WebView command permissions expose this plugin.
@TauriPlugin
class SecureStorePlugin(private val activity: Activity) : Plugin(activity) {
    private val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    private fun identity(base: String): String {
        require(base.isNotEmpty() && base.length <= 4096)
        return MessageDigest.getInstance("SHA-256").digest(base.toByteArray(Charsets.UTF_8))
            .joinToString("") { "%02x".format(it) }
    }
    private fun file(id: String) = AtomicFile(File(activity.noBackupFilesDir, "credential-$id.json"))
    private fun alias(id: String) = "sub2ops.$id"
    private fun secret(id: String, create: Boolean): SecretKey {
        (store.getKey(alias(id), null) as? SecretKey)?.let { return it }
        check(create)
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias(id), KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256).setRandomizedEncryptionRequired(true).build())
        }.generateKey()
    }
    @Command
    fun read(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(CredentialArgs::class.java)
            val id = identity(args.base)
            val saved = file(id)
            if (!saved.baseFile.exists()) { invoke.resolve(JSObject()); return }
            val data = JSONObject(saved.openRead().use { it.readBytes().toString(Charsets.UTF_8) })
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, secret(id, false), GCMParameterSpec(128, Base64.decode(data.getString("iv"), Base64.NO_WRAP)))
            cipher.updateAAD(args.base.toByteArray(Charsets.UTF_8))
            val key = cipher.doFinal(Base64.decode(data.getString("ciphertext"), Base64.NO_WRAP)).toString(Charsets.UTF_8)
            invoke.resolve(JSObject().put("key", key))
        } catch (_: Exception) { invoke.reject("安全存储读取失败") }
    }
    @Command
    fun write(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(CredentialArgs::class.java)
            val key = requireNotNull(args.key)
            require(key.isNotBlank() && key.length <= 4096)
            val id = identity(args.base)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, secret(id, true))
            cipher.updateAAD(args.base.toByteArray(Charsets.UTF_8))
            val data = JSONObject().put("iv", Base64.encodeToString(cipher.iv, Base64.NO_WRAP))
                .put("ciphertext", Base64.encodeToString(cipher.doFinal(key.toByteArray(Charsets.UTF_8)), Base64.NO_WRAP))
            val target = file(id)
            val stream = target.startWrite()
            try { stream.write(data.toString().toByteArray(Charsets.UTF_8)); target.finishWrite(stream) }
            catch (error: Exception) { target.failWrite(stream); throw error }
            invoke.resolve()
        } catch (_: Exception) { invoke.reject("安全存储写入失败") }
    }
    @Command
    fun delete(invoke: Invoke) {
        try {
            val id = identity(invoke.parseArgs(CredentialArgs::class.java).base)
            file(id).delete()
            check(!file(id).baseFile.exists())
            store.deleteEntry(alias(id))
            invoke.resolve()
        } catch (_: Exception) { invoke.reject("安全存储删除失败") }
    }
    @Command
    fun background(invoke: Invoke) {
        activity.runOnUiThread { activity.moveTaskToBack(true); invoke.resolve() }
    }
}
