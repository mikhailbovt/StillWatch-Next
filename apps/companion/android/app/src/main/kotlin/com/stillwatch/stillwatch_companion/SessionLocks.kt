package com.stillwatch.stillwatch_companion

import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel

/** The service and Activity both use the app's main looper and secure credential store. */
class SessionLocks(messenger: BinaryMessenger) {
    private val channel = MethodChannel(messenger, "com.stillwatch.companion/session-lock")

    init {
        channel.setMethodCallHandler { call, result ->
            when (call.method) {
                "acquire" -> {
                    val core = call.arguments as? String
                    if (core == null || core.length != 36) {
                        result.error("invalid_request", "Invalid Core session key", null)
                    } else if (held[core] != null) {
                        if (waiting.size >= 32) result.error("busy", "Session queue is full", null)
                        else waiting.add(Waiter(core, this, result))
                    } else {
                        grant(core, result)
                    }
                }
                "release" -> {
                    val handle = call.arguments as? Int
                    val core = held.entries.firstOrNull { it.value.owner === this && it.value.handle == handle }?.key
                    if (core != null) release(core)
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
    }

    fun close() {
        channel.setMethodCallHandler(null)
        waiting.removeAll { it.owner === this }
        held.filterValues { it.owner === this }.keys.toList().forEach(::release)
    }

    private fun grant(core: String, result: MethodChannel.Result) {
        val handle = ++sequence
        held[core] = Lease(this, handle)
        result.success(handle)
    }

    private fun release(core: String) {
        held.remove(core)
        val next = waiting.indexOfFirst { it.core == core }
        if (next >= 0) {
            val waiter = waiting.removeAt(next)
            waiter.owner.grant(core, waiter.result)
        }
    }

    private data class Lease(val owner: SessionLocks, val handle: Int)
    private data class Waiter(val core: String, val owner: SessionLocks, val result: MethodChannel.Result)

    private companion object {
        var sequence = 0
        val held = mutableMapOf<String, Lease>()
        val waiting = mutableListOf<Waiter>()
    }
}
