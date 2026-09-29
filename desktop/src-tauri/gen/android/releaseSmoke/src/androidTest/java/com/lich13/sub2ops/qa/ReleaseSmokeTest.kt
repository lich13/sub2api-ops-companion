package com.lich13.sub2ops.qa

import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import java.net.ServerSocket
import java.net.SocketException
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

/** Runs in a separate package: the release APK is not modified or instrumented. */
@RunWith(AndroidJUnit4::class)
class ReleaseSmokeTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val device = UiDevice.getInstance(instrumentation)
    private val app = "com.lich13.sub2ops"
    private fun launch() { device.executeShellCommand("am start -W -n $app/.MainActivity") }
    private fun await(message: String, check: () -> Boolean) {
        val until = SystemClock.elapsedRealtime() + 15000
        while (SystemClock.elapsedRealtime() < until) {
            if (check()) return
            SystemClock.sleep(100)
        }
        fail(message)
    }
    private fun connected() {
        assertTrue("Release app did not connect", device.wait(Until.hasObject(By.text("已连接")), 15000))
        assertTrue("Account data missing", device.wait(Until.hasObject(By.text("Codex · 主力")), 15000))
    }

    @Test fun connectAndBackground() {
        FixtureServer().use { server ->
            launch()
            assertTrue(device.wait(Until.hasObject(By.text("验证并连接")), 15000))
            val inputs = device.findObjects(By.clazz("android.widget.EditText"))
            assertEquals("Connection form inputs", 2, inputs.size)
            inputs[0].text = "http://127.0.0.1:18181"
            inputs[1].text = "release-smoke-key"
            device.findObject(By.text("验证并连接")).click()
            connected()
            for (page in listOf("模型", "事件", "自动化", "设置", "账号")) {
                device.findObject(By.text(page)).click()
                device.waitForIdle()
            }
            val before = server.snapshots.get()
            await("Release foreground refresh missing") { server.snapshots.get() >= before + 2 }
            device.pressHome()
            SystemClock.sleep(1400)
            val paused = server.requests.get()
            SystemClock.sleep(3200)
            assertEquals("Release kept polling in background", paused, server.requests.get())
            launch()
            connected()
            await("Release did not refresh after resume") { server.requests.get() > paused }
            device.executeShellCommand("am force-stop $app")
            launch()
            connected()
            assertEquals("Read-only smoke issued writes", 0, server.writes.get())
            assertTrue(server.correctKey)
        }
    }

    @Test fun restoreAndDisconnect() {
        FixtureServer().use { server ->
            launch()
            connected()
            device.findObject(By.text("设置")).click()
            val disconnect = By.text("断开连接并删除本机 Key")
            for (i in 0..8) {
                if (device.hasObject(disconnect)) break
                device.swipe(device.displayWidth / 2, device.displayHeight * 3 / 4,
                    device.displayWidth / 2, device.displayHeight / 3, 24)
            }
            assertTrue("Disconnect control missing", device.hasObject(disconnect))
            device.findObject(disconnect).click()
            assertTrue(device.wait(Until.hasObject(By.text("验证并连接")), 15000))
            device.executeShellCommand("am force-stop $app")
            launch()
            assertTrue("Disconnect did not survive restart", device.wait(Until.hasObject(By.text("验证并连接")), 15000))
            assertFalse(device.hasObject(By.text("已连接")))
            assertEquals(0, server.writes.get())
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
                    val input = client.getInputStream().bufferedReader()
                    val request = input.readLine() ?: return@use
                    val headers = mutableListOf<String>()
                    while (true) { val line = input.readLine() ?: break; if (line.isEmpty()) break; headers.add(line) }
                    requests.incrementAndGet()
                    if (!request.startsWith("GET ")) writes.incrementAndGet()
                    if (headers.none { it.equals("x-api-key: release-smoke-key", true) }) correctKey = false
                    val path = request.split(' ')[1].removePrefix("/api/desktop/v1")
                    val body = when (path) {
                        "/capabilities" -> "{\"api_version\":1}"
                        "/snapshot" -> { snapshots.incrementAndGet(); fixture("snapshot") }
                        "/config" -> fixture("config")
                        "/model-groups" -> "{\"groups\":[]}"
                        "/quota-refresh" -> "{\"status\":\"idle\",\"items\":[],\"total\":0,\"completed\":0}"
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
