package com.stillwatch.stillwatch_companion

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import io.flutter.FlutterInjector
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.embedding.engine.dart.DartExecutor
import io.flutter.plugin.common.MethodChannel

class AlarmService : Service() {
    private var engine: FlutterEngine? = null
    private var sessionLocks: SessionLocks? = null
    private val manager by lazy { getSystemService(NotificationManager::class.java) }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        val preferences = getSharedPreferences(PREFERENCES, MODE_PRIVATE)
        val core = intent?.getStringExtra(CORE) ?: preferences.getString(CORE, null)
        if (core == null || !manager.areNotificationsEnabled()) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (engine != null) return START_STICKY
        if (Build.VERSION.SDK_INT >= 26) {
            manager.createNotificationChannel(NotificationChannel(CHANNEL, "StillWatch monitoring", NotificationManager.IMPORTANCE_LOW))
            if (manager.getNotificationChannel(CHANNEL).importance == NotificationManager.IMPORTANCE_NONE) {
                stopSelf()
                return START_NOT_STICKY
            }
        }
        state = "connecting"
        startForeground(STATUS_ID, notification("Connecting to Core"))
        preferences.edit().putString(CORE, core).apply()
        try {
            val loader = FlutterInjector.instance().flutterLoader()
            loader.startInitialization(applicationContext)
            loader.ensureInitializationComplete(applicationContext, null)
            val runtime = FlutterEngine(applicationContext)
            engine = runtime
            sessionLocks = SessionLocks(runtime.dartExecutor.binaryMessenger)
            MethodChannel(runtime.dartExecutor.binaryMessenger, WORK_CHANNEL).setMethodCallHandler { call, result ->
                when (call.method) {
                    "status" -> {
                        when (call.arguments) {
                            "connected" -> { state = "connected"; manager.notify(STATUS_ID, notification("Monitoring for urgent incidents")) }
                            "offline" -> { state = "offline"; manager.notify(STATUS_ID, notification("Core is unreachable. Reconnecting.")) }
                            "revoked" -> { result.success(null); stopSelf(); return@setMethodCallHandler }
                            "permission" -> { result.success(null); stopSelf(); return@setMethodCallHandler }
                            else -> { result.error("invalid_request", "Unknown monitoring status", null); return@setMethodCallHandler }
                        }
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }
            runtime.dartExecutor.executeDartEntrypoint(DartExecutor.DartEntrypoint(loader.findAppBundlePath(), "androidAlarmMain"), listOf(core))
        } catch (_: RuntimeException) {
            stopSelf()
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onDestroy() {
        state = "stopped"
        getSharedPreferences(PREFERENCES, MODE_PRIVATE).edit().remove(CORE).apply()
        sessionLocks?.close()
        sessionLocks = null
        engine?.destroy()
        engine = null
        @Suppress("DEPRECATION")
        stopForeground(true)
        super.onDestroy()
    }

    private fun notification(message: String): Notification {
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(this, 0, Intent(this, AlarmService::class.java).setAction(STOP), PendingIntent.FLAG_IMMUTABLE)
        @Suppress("DEPRECATION")
        val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else Notification.Builder(this)
        @Suppress("DEPRECATION")
        return builder.setSmallIcon(R.drawable.ic_notification).setContentTitle("StillWatch monitoring").setContentText(message)
            .setOnlyAlertOnce(true).setOngoing(true).setContentIntent(open)
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "Stop", stop).build()
    }

    companion object {
        const val CONTROL_CHANNEL = "com.stillwatch.companion/background-alerts"
        const val CORE = "core"
        private const val WORK_CHANNEL = "com.stillwatch.companion/alarm-worker"
        private const val PREFERENCES = "stillwatch.alarm-service"
        private const val CHANNEL = "stillwatch-monitoring"
        private const val STOP = "com.stillwatch.companion.STOP_ALARM"
        private const val STATUS_ID = 0x7ffffffe
        var state = "stopped"
            private set
    }
}
