package com.stillwatch.stillwatch_companion

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import com.google.zxing.client.android.Intents
import com.journeyapps.barcodescanner.CaptureActivity
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var pendingScan: MethodChannel.Result? = null
    private var sessionLocks: SessionLocks? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        sessionLocks = SessionLocks(flutterEngine.dartExecutor.binaryMessenger)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, AlarmService.CONTROL_CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "start" -> {
                        val intent = Intent(this, AlarmService::class.java)
                        val profile = call.argument<String>("core")
                        if (profile == null || !PROFILE.matches(profile)) {
                            result.error("invalid_request", "Invalid Core profile", null)
                            return@setMethodCallHandler
                        }
                        intent.putExtra(AlarmService.CORE, profile)
                        try {
                            if (android.os.Build.VERSION.SDK_INT >= 26) startForegroundService(intent)
                            else startService(intent)
                            result.success(null)
                        } catch (_: RuntimeException) {
                            result.error("background_unavailable", "Android could not start monitoring", null)
                        }
                    }
                    "stop" -> {
                        stopService(Intent(this, AlarmService::class.java))
                        result.success(null)
                    }
                    "state" -> result.success(AlarmService.state)
                    else -> result.notImplemented()
                }
            }
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "com.stillwatch.companion/pairing")
            .setMethodCallHandler { call, result ->
                when {
                    call.method != "scanQr" -> result.notImplemented()
                    pendingScan != null -> result.error("busy", "Pairing scanner is already open", null)
                    else -> {
                        pendingScan = result
                        val intent = Intent(this, CaptureActivity::class.java).apply {
                            action = Intents.Scan.ACTION
                            putExtra(Intents.Scan.FORMATS, "QR_CODE")
                            putExtra(Intents.Scan.BEEP_ENABLED, false)
                            putExtra(Intents.Scan.BARCODE_IMAGE_ENABLED, false)
                            putExtra(Intents.Scan.SHOW_MISSING_CAMERA_PERMISSION_DIALOG, false)
                        }
                        @Suppress("DEPRECATION")
                        startActivityForResult(intent, QR_SCAN)
                    }
                }
            }
    }

    override fun cleanUpFlutterEngine(flutterEngine: FlutterEngine) {
        sessionLocks?.close()
        sessionLocks = null
        super.cleanUpFlutterEngine(flutterEngine)
    }

    @Deprecated("FlutterActivity does not own an ActivityResultRegistry")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != QR_SCAN) return
        val result = pendingScan ?: return
        pendingScan = null
        if (resultCode != RESULT_OK && checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            result.error("camera_permission", "Camera permission is unavailable", null)
        } else {
            result.success(data?.getStringExtra(Intents.Scan.RESULT)?.takeIf { resultCode == RESULT_OK && it.length <= 8192 })
        }
    }

    private companion object {
        const val QR_SCAN = 4802
        val PROFILE = Regex("[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}")
    }
}
