package com.lich13.sub2ops

import android.os.SystemClock
import android.app.Activity
import android.app.Instrumentation
import android.content.Intent
import android.graphics.Bitmap
import android.view.View
import android.view.ViewGroup
import android.view.MotionEvent
import android.webkit.WebView
import androidx.core.content.FileProvider
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.lifecycle.Lifecycle
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.espresso.Espresso.pressBack
import androidx.test.espresso.web.sugar.Web.onWebView
import androidx.test.espresso.web.webdriver.DriverAtoms.findElement
import androidx.test.espresso.web.webdriver.DriverAtoms.webClick
import androidx.test.espresso.web.webdriver.Locator
import java.io.File
import java.net.ServerSocket
import java.net.SocketException
import java.security.KeyStore
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

/** Run each method in a separate instrumentation process, with an APK upgrade between them. */
@RunWith(AndroidJUnit4::class)
class AndroidRuntimeTest {
    private lateinit var activity: MainActivity
    private lateinit var web: WebView
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val base = "http://127.0.0.1:18181"
    private val testKey = "android-instrumentation-key"

    private fun await(message: String, timeout: Long = 15000, check: () -> Boolean) {
        val end = SystemClock.elapsedRealtime() + timeout
        while (SystemClock.elapsedRealtime() < end) {
            if (check()) return
            SystemClock.sleep(100)
        }
        fail(message)
    }

    private fun findWeb(view: View): WebView? {
        if (view is WebView) return view
        if (view is ViewGroup) for (i in 0 until view.childCount) findWeb(view.getChildAt(i))?.let { return it }
        return null
    }

    private fun open(): ActivityScenario<MainActivity> {
        val scenario = ActivityScenario.launch(MainActivity::class.java)
        scenario.onActivity { activity = it }
        await("Native WebView did not start") {
            instrumentation.runOnMainSync { findWeb(activity.window.decorView)?.let { web = it } }
            ::web.isInitialized
        }
        await("Rust bridge or React did not start") {
            eval("Boolean(window.__TAURI_INTERNALS__ && document.querySelector('.app'))") == "true"
        }
        await("Credential initialization did not complete") { !invoke("get_state").getBoolean("initializing") }
        return scenario
    }

    private fun eval(script: String): String {
        var result = "null"
        val latch = CountDownLatch(1)
        instrumentation.runOnMainSync { web.evaluateJavascript(script) { result = it; latch.countDown() } }
        assertTrue("WebView evaluation timeout", latch.await(5, TimeUnit.SECONDS))
        return result
    }

    private fun invoke(command: String, args: JSONObject = JSONObject()): JSONObject {
        eval("window.__qaResult=null; window.__TAURI_INTERNALS__.invoke(${JSONObject.quote(command)}, $args).then(value=>window.__qaResult={ok:true,value:value??null}).catch(error=>window.__qaResult={ok:false,error:String(error)})")
        await("Native command timed out: $command") { eval("window.__qaResult!==null") == "true" }
        val wrapper = JSONObject(eval("window.__qaResult"))
        assertTrue("Native command failed: $command ${wrapper.optString("error")}", wrapper.getBoolean("ok"))
        return wrapper.optJSONObject("value") ?: JSONObject()
    }

    private fun tapWeb(selector: String) {
        val point = JSONObject(eval("(() => { const r=document.querySelector(${JSONObject.quote(selector)}).getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2,width:innerWidth}; })()"))
        val position = IntArray(2)
        instrumentation.runOnMainSync { web.getLocationOnScreen(position) }
        val scale = web.width / point.getDouble("width")
        val x = (position[0] + point.getDouble("x") * scale).toFloat()
        val y = (position[1] + point.getDouble("y") * scale).toFloat()
        val down = SystemClock.uptimeMillis()
        for (action in listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP)) {
            val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, x, y, 0)
            assertTrue(instrumentation.uiAutomation.injectInputEvent(event, true))
            event.recycle()
        }
    }

    @Test fun persistAndLifecycle() {
        FixtureServer().use { server ->
            val scenario = open()
            invoke("connect", JSONObject().put("baseUrl", base).put("apiKey", testKey))
            await("Snapshot was not received") { invoke("get_state").optBoolean("online") }
            assertEquals("android", invoke("get_state").getString("platform"))
            assertEquals("6", eval("document.querySelectorAll('.mobile-account').length"))
            assertEquals("5", eval("document.querySelectorAll('.bottom-nav button').length"))
            for (index in 0..4) {
                eval("document.querySelectorAll('.bottom-nav button')[$index].click()")
                await("Mobile page navigation failed") { eval("document.querySelectorAll('.bottom-nav button')[$index].classList.contains('active')") == "true" }
                assertEquals("false", eval("document.documentElement.scrollWidth > innerWidth"))
            }
            eval("document.querySelectorAll('.bottom-nav button')[0].click()")
            eval("Array.from(document.querySelectorAll('button')).find(b=>b.textContent==='筛选').click()")
            await("Filters did not open") { eval("Boolean(document.querySelector('.filter-options.open'))") == "true" }
            instrumentation.runOnMainSync { activity.onBackPressedDispatcher.onBackPressed() }
            await("Android Back did not close filters") { eval("!document.querySelector('.filter-options.open')") == "true" }

            eval("window.__qaDenied=null; window.__TAURI_INTERNALS__.invoke('plugin:secure-store|read',{base:${JSONObject.quote(base)}}).then(()=>window.__qaDenied=false).catch(()=>window.__qaDenied=true)")
            await("Private credential command leaked to WebView") { eval("window.__qaDenied") == "true" }
            val secrets = activity.noBackupFilesDir.listFiles()?.filter { it.name.startsWith("credential-") } ?: emptyList()
            assertEquals(1, secrets.size)
            assertFalse(secrets.single().readText().contains(testKey))
            assertTrue(JSONObject(secrets.single().readText()).has("ciphertext"))
            val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            val aliases = store.aliases().toList().filter { it.startsWith("sub2ops.") }
            assertEquals(1, aliases.size)
            assertNull("AES key must not be exportable", store.getKey(aliases.single(), null).encoded)
            assertEquals("false", eval("JSON.stringify(localStorage).includes(${JSONObject.quote(testKey)}) || JSON.stringify(sessionStorage).includes(${JSONObject.quote(testKey)})"))

            val before = server.snapshots.get()
            await("Foreground two-second refresh missing", 5500) { server.snapshots.get() >= before + 2 }
            scenario.moveToState(Lifecycle.State.CREATED)
            SystemClock.sleep(1400)
            val paused = server.requests.get()
            SystemClock.sleep(3200)
            assertEquals("Background continued polling", paused, server.requests.get())
            scenario.moveToState(Lifecycle.State.RESUMED)
            await("Foreground did not refresh immediately", 3000) { server.requests.get() > paused }
            assertEquals("Read-only navigation issued a write", 0, server.writes.get())
            assertTrue(server.correctKey)
            val position = IntArray(2)
            instrumentation.runOnMainSync { web.getLocationOnScreen(position) }
            val bars = ViewCompat.getRootWindowInsets(web)!!.getInsets(WindowInsetsCompat.Type.systemBars())
            assertTrue("WebView overlaps status bar", position[1] >= bars.top)
            assertTrue("WebView overlaps navigation bar", position[1] + web.height <= web.rootView.height - bars.bottom)
            // Keep encrypted credentials for the separate process / upgrade test.
        }
    }

    @Test fun restartUpgradeAndDisconnect() {
        FixtureServer().use {
            open()
            await("Credentials were not restored after restart/upgrade") { invoke("get_state").optBoolean("online") }
            invoke("disconnect")
            assertFalse(invoke("get_state").getBoolean("connected"))
            assertTrue(activity.noBackupFilesDir.listFiles()?.none { it.name.startsWith("credential-") } ?: true)
            val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            assertTrue(store.aliases().toList().none { it.startsWith("sub2ops.") })
        }
    }

    @Test fun keyboardAndMediaPicker() {
        FixtureServer().use { server ->
            open()
            invoke("connect", JSONObject().put("baseUrl", base).put("apiKey", testKey))
            await("Accounts not ready") { eval("document.querySelectorAll('.mobile-account').length") == "6" }
            File(activity.cacheDir, "qa-accounts.png").outputStream().use {
                instrumentation.uiAutomation.takeScreenshot().compress(Bitmap.CompressFormat.PNG, 100, it)
            }
            // A real touch establishes the WebView input connection; a JS click does not.
            tapWeb(".search input")
            await("Soft keyboard did not open") { ViewCompat.getRootWindowInsets(web)?.isVisible(WindowInsetsCompat.Type.ime()) == true }
            assertEquals("false", eval("document.documentElement.scrollWidth > innerWidth"))
            pressBack()
            await("Back did not dismiss keyboard") { ViewCompat.getRootWindowInsets(web)?.isVisible(WindowInsetsCompat.Type.ime()) == false }
            eval("document.querySelector('.mobile-account .icon-button').click()")
            eval("Array.from(document.querySelectorAll('.mobile-action-sheet button')).find(b=>b.textContent.includes('测试连接')).click()")
            await("Test model list not ready") { eval("document.querySelectorAll('.test-controls select')[1]?.options.length") == "3" }
            eval("const model=document.querySelectorAll('.test-controls select')[1]; model.value='gpt-image-1'; model.dispatchEvent(new Event('change',{bubbles:true}));")
            await("Image input not rendered") { eval("Boolean(document.querySelector('input[type=file]'))") == "true" }
            val source = File(activity.cacheDir, "qa-input.png")
            source.outputStream().use { Bitmap.createBitmap(2, 2, Bitmap.Config.ARGB_8888).compress(Bitmap.CompressFormat.PNG, 100, it) }
            val uri = FileProvider.getUriForFile(activity, "com.lich13.sub2ops.fileprovider", source)
            val opened = AtomicInteger()
            val monitor = object : Instrumentation.ActivityMonitor() {
                override fun onStartActivity(intent: Intent): Instrumentation.ActivityResult? {
                    if (intent.action in listOf(Intent.ACTION_GET_CONTENT, Intent.ACTION_OPEN_DOCUMENT, Intent.ACTION_CHOOSER)) {
                        opened.incrementAndGet()
                        return Instrumentation.ActivityResult(Activity.RESULT_OK, Intent().setData(uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION))
                    }
                    return null
                }
            }
            instrumentation.addMonitor(monitor)
            try {
                onWebView().withElement(findElement(Locator.CSS_SELECTOR, "input[type=file]")).perform(webClick())
                await("Android file picker result was not read") { eval("document.querySelector('.test-upload').textContent.includes('qa-input.png')") == "true" }
                assertEquals(1, opened.get())
                assertEquals("Selecting media must not send a model request", 0, server.writes.get())
            } finally { instrumentation.removeMonitor(monitor); source.delete() }
            invoke("disconnect")
        }
    }

    private inner class FixtureServer : AutoCloseable {
        private val socket = ServerSocket(18181, 10, java.net.InetAddress.getByName("127.0.0.1"))
        val requests = AtomicInteger()
        val snapshots = AtomicInteger()
        val writes = AtomicInteger()
        @Volatile var correctKey = true
        private val thread = Thread {
            while (!socket.isClosed) try {
                socket.accept().use { client ->
                    client.soTimeout = 5000
                    val reader = client.getInputStream().bufferedReader()
                    val request = reader.readLine() ?: return@use
                    val headers = mutableListOf<String>()
                    while (true) { val line = reader.readLine() ?: break; if (line.isEmpty()) break; headers.add(line) }
                    requests.incrementAndGet()
                    if (!request.startsWith("GET ")) writes.incrementAndGet()
                    if (headers.none { it.equals("x-api-key: $testKey", true) }) correctKey = false
                    val path = request.split(' ')[1].removePrefix("/api/desktop/v1")
                    val body = when {
                        path == "/capabilities" -> "{\"api_version\":1}"
                        path == "/snapshot" -> { snapshots.incrementAndGet(); fixture("snapshot") }
                        path == "/config" -> fixture("config")
                        path == "/model-groups" -> "{\"groups\":[]}"
                        path == "/quota-refresh" -> "{\"id\":\"idle\",\"status\":\"idle\",\"items\":[],\"total\":0,\"completed\":0}"
                        path.endsWith("/models") -> "[{\"id\":\"gpt-6-sol\",\"type\":\"text\"},{\"id\":\"gpt-image-1\",\"type\":\"image\"}]"
                        else -> "{\"items\":[],\"next_cursor\":null}"
                    }.toByteArray()
                    client.getOutputStream().write("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${body.size}\r\nConnection: close\r\n\r\n".toByteArray() + body)
                }
            } catch (_: SocketException) { break }
        }.apply { isDaemon = true; start() }
        private fun fixture(name: String) = instrumentation.context.assets.open("$name.json").bufferedReader().use { it.readText() }
        override fun close() { socket.close(); thread.join(1000) }
    }
}
