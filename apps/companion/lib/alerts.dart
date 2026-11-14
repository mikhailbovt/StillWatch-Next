import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'profiles.dart';

final class LocalAlerts extends ChangeNotifier {
  LocalAlerts(this.console, this.storage) {
    console.addListener(_connectionChanged);
  }

  final ConsoleState console;
  final FlutterSecureStorage storage;
  final _notifications = FlutterLocalNotificationsPlugin();
  final _active = <String, int>{};
  static const _preference = 'stillwatch.alerts.foreground';
  static const _blockedAtKey = 'stillwatch.alerts.permission_blocked_at';
  static const _urgentChannel = 'stillwatch-urgent';
  bool enabled = false;
  bool available = false;
  DateTime? permissionBlockedAt;
  bool _busy = false;
  bool _changingPreference = false;
  bool _closed = false;
  Future<bool>? _permissionCheck;
  Timer? _poll;

  Future<void> initialize() async {
    try {
      await _notifications.initialize(
        settings: InitializationSettings(
          android: const AndroidInitializationSettings('@mipmap/ic_launcher'),
          iOS: DarwinInitializationSettings(
            requestAlertPermission: false,
            requestBadgePermission: false,
            requestSoundPermission: false,
            notificationCategories: [
              DarwinNotificationCategory(
                'stillwatch-alert',
                actions: [
                  DarwinNotificationAction.plain(
                    'ack',
                    'Acknowledge',
                    options: const {
                      DarwinNotificationActionOption.authenticationRequired,
                      DarwinNotificationActionOption.foreground,
                    },
                  ),
                ],
              ),
            ],
          ),
          linux: const LinuxInitializationSettings(
            defaultActionName: 'Open StillWatch',
          ),
          windows: const WindowsInitializationSettings(
            appName: 'StillWatch',
            appUserModelId: 'StillWatch.Companion',
            guid: 'b2670827-583d-458b-a3ee-0823efa53048',
          ),
        ),
        onDidReceiveNotificationResponse: (response) =>
            unawaited(_action(response)),
      );
      available = true;
      enabled = await storage.read(key: _preference) == 'yes';
      permissionBlockedAt = DateTime.tryParse(
        await storage.read(key: _blockedAtKey) ?? '',
      )?.toUtc();
      await _refreshPermissions();
      if (enabled) _connectionChanged();
      final launch = await _notifications.getNotificationAppLaunchDetails();
      final response = launch?.notificationResponse;
      if (launch?.didNotificationLaunchApp == true && response != null) {
        unawaited(_action(response));
      }
    } catch (_) {
      available = false;
    }
    if (!_closed) notifyListeners();
  }

  Future<void> setEnabled(bool value) async {
    if (_changingPreference) return;
    if (!available) throw const StillWatchException('notification_unavailable');
    _changingPreference = true;
    try {
      try {
        await _permissionCheck;
      } catch (_) {
        // Retry the system check after changing the preference.
      }
      if (value) {
        final allowed = switch (Platform.operatingSystem) {
          'android' =>
            await _notifications
                .resolvePlatformSpecificImplementation<
                  AndroidFlutterLocalNotificationsPlugin
                >()
                ?.requestNotificationsPermission(),
          'ios' =>
            await _notifications
                .resolvePlatformSpecificImplementation<
                  IOSFlutterLocalNotificationsPlugin
                >()
                ?.requestPermissions(alert: true, sound: true, badge: false),
          _ => true,
        };
        if (allowed != true) {
          await _recordPermission(false, requested: true);
          throw const StillWatchException('notification_permission');
        }
      }
      await storage.write(key: _preference, value: value ? 'yes' : 'no');
      enabled = value;
      await _refreshPermissions();
    } finally {
      _changingPreference = false;
      _connectionChanged();
      if (!_closed) notifyListeners();
    }
  }

  Future<bool> _refreshPermissions() async {
    if (_permissionCheck case final pending?) {
      return pending.onError((_, _) => false);
    }
    final check = _checkPermissions();
    _permissionCheck = check;
    try {
      return await check;
    } catch (_) {
      // The notification service can be unavailable during app transitions.
      return false;
    } finally {
      _permissionCheck = null;
    }
  }

  void refreshPermissionsOnResume() {
    if (available && !_closed && !_changingPreference) {
      unawaited(_refreshPermissions());
    }
  }

  Future<bool> _checkPermissions() async {
    final android = _notifications
        .resolvePlatformSpecificImplementation<
          AndroidFlutterLocalNotificationsPlugin
        >();
    final ios = _notifications
        .resolvePlatformSpecificImplementation<
          IOSFlutterLocalNotificationsPlugin
        >();
    final bool allowed;
    if (android != null) {
      final enabled = await android.areNotificationsEnabled() == true;
      final channels = enabled ? await android.getNotificationChannels() : null;
      allowed =
          enabled &&
          !(channels?.any(
                (channel) =>
                    channel.id == _urgentChannel &&
                    channel.importance == Importance.none,
              ) ??
              false);
    } else if (ios != null) {
      final settings = await ios.checkPermissions();
      allowed =
          settings?.isEnabled == true || settings?.isProvisionalEnabled == true;
    } else {
      allowed = true;
    }
    await _recordPermission(allowed);
    return allowed;
  }

  Future<void> _recordPermission(bool allowed, {bool requested = false}) async {
    if (_closed) return;
    final previous = permissionBlockedAt;
    if (allowed) {
      permissionBlockedAt = null;
    } else if (enabled || requested) {
      permissionBlockedAt ??= DateTime.now().toUtc();
    }
    if (previous != permissionBlockedAt) {
      notifyListeners();
      try {
        final blocked = permissionBlockedAt;
        if (blocked == null) {
          await storage.delete(key: _blockedAtKey);
        } else {
          await storage.write(
            key: _blockedAtKey,
            value: blocked.toIso8601String(),
          );
        }
      } catch (_) {
        // Keep the current device permission visible if secure storage is unavailable.
      }
    }
  }

  void _connectionChanged() {
    if (_closed) return;
    if (!enabled ||
        !available ||
        !console.foreground ||
        console.link != LinkState.connected ||
        _changingPreference) {
      _poll?.cancel();
      _poll = null;
      if (_active.isNotEmpty) unawaited(_removeAll());
      return;
    }
    if (_poll == null) {
      _poll = Timer.periodic(
        const Duration(seconds: 3),
        (_) => unawaited(_sync()),
      );
      unawaited(_sync());
    }
  }

  Future<void> _sync() async {
    final profile = console.selected;
    if (profile == null || _busy || _closed || _changingPreference) return;
    _busy = true;
    try {
      if (!await _refreshPermissions()) {
        if (_active.isNotEmpty) await _removeAll();
        return;
      }
      if (!_canNotify(profile)) return;
      final feed = await profile.client.alerts();
      final current = <String>{};
      for (final alert in feed) {
        if (!_canNotify(profile)) return;
        final id = counter(alert['id']);
        final cycle = counter(alert['attention_cycle']);
        if (alert['core_id'] != profile.id ||
            alert['attention'] != 'urgent' ||
            !timestamp(alert, 'expires_at').isAfter(DateTime.now())) {
          continue;
        }
        final key = '${profile.id}:$id:$cycle';
        current.add(key);
        if (alert['client_seen_at'] != null) {
          continue;
        }
        if (_active.containsKey(key)) {
          await _receipt(profile, id, cycle);
          continue;
        }
        final handle = key.hashCode & 0x7fffffff;
        final payload = jsonEncode({
          'core': profile.id,
          'incident': uuid(alert['incident_id']),
          'cycle': cycle,
          'expires': alert['expires_at'],
          'revision': counter(alert['revision']),
        });
        await _notifications.show(
          id: handle,
          title: 'StillWatch alert',
          body: 'Open StillWatch to review an urgent incident.',
          notificationDetails: NotificationDetails(
            android: AndroidNotificationDetails(
              _urgentChannel,
              'Urgent incidents',
              importance: Importance.high,
              priority: Priority.high,
              visibility: NotificationVisibility.private,
              actions: [
                if (profile.can('events_ack'))
                  const AndroidNotificationAction(
                    'ack',
                    'Acknowledge',
                    showsUserInterface: true,
                    cancelNotification: false,
                  ),
              ],
            ),
            iOS: DarwinNotificationDetails(
              categoryIdentifier: profile.can('events_ack')
                  ? 'stillwatch-alert'
                  : null,
            ),
          ),
          payload: payload,
        );
        if (!_canNotify(profile)) {
          await _notifications.cancel(id: handle);
          return;
        }
        _active[key] = handle;
        await _receipt(profile, id, cycle);
      }
      for (final key
          in _active.keys.where((key) => !current.contains(key)).toList()) {
        await _notifications.cancel(id: _active.remove(key)!);
      }
    } catch (_) {
      // The next poll revalidates the alert feed against Core.
    } finally {
      _busy = false;
    }
  }

  bool _canNotify(CoreProfile profile) =>
      enabled &&
      !_closed &&
      !_changingPreference &&
      console.foreground &&
      console.selected == profile &&
      console.link == LinkState.connected;

  Future<void> _receipt(CoreProfile profile, String id, String cycle) async {
    try {
      await profile.client.alertSeen(id, cycle);
    } catch (_) {
      // Retry the receipt for the already displayed notification on the next poll.
    }
  }

  Future<void> _action(NotificationResponse response) async {
    if (response.actionId != 'ack' || response.payload == null) return;
    try {
      final payload = object(jsonDecode(response.payload!));
      final profile = console.profiles
          .where((item) => item.id == payload['core'])
          .firstOrNull;
      if (profile == null ||
          !profile.can('events_ack') ||
          !timestamp(payload, 'expires').isAfter(DateTime.now())) {
        return;
      }
      final incident = await profile.client.incident(uuid(payload['incident']));
      if (incident.lifecycle != 'open' ||
          incident.acknowledged ||
          incident.attentionCycle != payload['cycle'] ||
          incident.revision != payload['revision']) {
        return;
      }
      final next = await profile.client.acknowledge(incident, operationId());
      if (!_closed && console.selected == profile) {
        console.replaceIncident(next);
      }
    } catch (_) {
      // The authoritative timeline retains any unacknowledged incident.
    }
  }

  Future<void> _removeAll() async {
    final handles = _active.values.toList();
    _active.clear();
    for (final id in handles) {
      try {
        await _notifications.cancel(id: id);
      } catch (_) {
        // The system notification service may have stopped during suspension.
      }
    }
  }

  @override
  void dispose() {
    _closed = true;
    _poll?.cancel();
    console.removeListener(_connectionChanged);
    unawaited(_removeAll());
    super.dispose();
  }
}
