package com.lich13.sub2ops

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File

@InvokeArg
class InstallApkArgs {
    lateinit var path: String
}

// Invoked from Rust only; validates verified APK inside cacheDir/updates and triggers PackageInstaller.
@TauriPlugin
class AppUpdaterPlugin(private val activity: Activity) : Plugin(activity) {
    private val lock = Any()
    @Volatile private var pendingApkPath: String? = null
    @Volatile private var installStatus: String = "idle"
    @Volatile private var lastError: String = ""

    private fun archivePackageInfo(pm: PackageManager, path: String): PackageInfo? {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.getPackageArchiveInfo(path, PackageManager.PackageInfoFlags.of(0))
        } else {
            @Suppress("DEPRECATION")
            pm.getPackageArchiveInfo(path, 0)
        }
    }

    private fun installedPackageInfo(pm: PackageManager, pkg: String): PackageInfo {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.getPackageInfo(pkg, PackageManager.PackageInfoFlags.of(0))
        } else {
            @Suppress("DEPRECATION")
            pm.getPackageInfo(pkg, 0)
        }
    }

    private fun validateApk(rawPath: String): File {
        require(rawPath.isNotBlank() && rawPath.length <= 4096) { "安装包路径无效" }
        val updatesRoot = File(activity.cacheDir, "updates").canonicalFile
        val canonical = File(rawPath).canonicalFile
        require(
            canonical.parentFile == updatesRoot &&
                canonical.isFile &&
                canonical.length() > 0L &&
                canonical.name.endsWith(".apk")
        ) { "安装包不在受控缓存目录内" }
        val pm = activity.packageManager
        val archive = archivePackageInfo(pm, canonical.path)
            ?: throw IllegalArgumentException("无法解析 APK 安装包")
        require(archive.packageName == activity.packageName) { "安装包应用标识不匹配" }
        val current = installedPackageInfo(pm, activity.packageName)
        require(archive.longVersionCode > current.longVersionCode) { "安装包版本号不高于当前版本" }
        return canonical
    }

    private fun startPackageInstaller(apkFile: File) {
        val uri = FileProvider.getUriForFile(
            activity,
            "${activity.packageName}.fileprovider",
            apkFile
        )
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        activity.startActivity(intent)
    }

    private fun openUnknownSourcesSettings() {
        val primary = Intent(
            Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
            Uri.parse("package:${activity.packageName}")
        ).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        try {
            activity.startActivity(primary)
        } catch (_: ActivityNotFoundException) {
            val fallback = Intent(Settings.ACTION_SECURITY_SETTINGS).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            activity.startActivity(fallback)
        }
    }

    private fun executePendingOnUiThread(): Pair<String, String> {
        val rawPath: String = synchronized(lock) {
            val raw = pendingApkPath ?: return Pair(installStatus, lastError)
            if (!activity.packageManager.canRequestPackageInstalls()) {
                installStatus = "permission_required"
                lastError = ""
                return Pair("permission_required", "")
            }
            pendingApkPath = null
            raw
        }
        return try {
            val verified = validateApk(rawPath)
            startPackageInstaller(verified)
            synchronized(lock) {
                installStatus = "installing"
                lastError = ""
            }
            Pair("installing", "")
        } catch (error: IllegalArgumentException) {
            val msg = error.message ?: "安装包校验失败"
            synchronized(lock) {
                installStatus = "error"
                lastError = msg
            }
            Pair("error", msg)
        } catch (_: Exception) {
            val msg = "无法调起系统安装器"
            synchronized(lock) {
                installStatus = "error"
                lastError = msg
            }
            Pair("error", msg)
        }
    }

    override fun onResume(activity: AppCompatActivity) {
        super.onResume(activity)
        executePendingOnUiThread()
    }

    @Command
    fun installApk(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(InstallApkArgs::class.java)
            val verified = validateApk(args.path)
            if (activity.packageManager.canRequestPackageInstalls()) {
                activity.runOnUiThread {
                    try {
                        startPackageInstaller(verified)
                        synchronized(lock) {
                            pendingApkPath = null
                            installStatus = "installing"
                            lastError = ""
                        }
                        invoke.resolve(JSObject().put("status", "installing"))
                    } catch (_: Exception) {
                        synchronized(lock) {
                            pendingApkPath = null
                            installStatus = "error"
                            lastError = "无法调起系统安装器"
                        }
                        invoke.reject("无法调起系统安装器")
                    }
                }
            } else {
                activity.runOnUiThread {
                    try {
                        synchronized(lock) {
                            pendingApkPath = verified.path
                            installStatus = "permission_required"
                            lastError = ""
                        }
                        openUnknownSourcesSettings()
                        invoke.resolve(JSObject().put("status", "permission_required"))
                    } catch (_: Exception) {
                        synchronized(lock) {
                            pendingApkPath = null
                            installStatus = "error"
                            lastError = "无法打开未知来源应用授权设置"
                        }
                        invoke.reject("无法打开未知来源应用授权设置")
                    }
                }
            }
        } catch (error: IllegalArgumentException) {
            invoke.reject(error.message ?: "安装包校验失败")
        } catch (_: Exception) {
            invoke.reject("准备安装更新失败")
        }
    }

    @Command
    fun resumePending(invoke: Invoke) {
        activity.runOnUiThread {
            val (status, error) = executePendingOnUiThread()
            val res = JSObject().put("status", status)
            if (error.isNotEmpty()) {
                res.put("error", error)
            }
            invoke.resolve(res)
        }
    }

    @Command
    fun clearPending(invoke: Invoke) {
        synchronized(lock) {
            pendingApkPath = null
            installStatus = "idle"
            lastError = ""
        }
        invoke.resolve(JSObject().put("status", "idle"))
    }
}
