import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'profiles.dart';

final class LocalAlerts extends ChangeNotifier {
  LocalAlerts(
    this.console,
    this.storage, {
    this.backgroundWorker = false,
    this.onReview,
    this.onFailure,
    this.observePlatformDelivery,
  }) {
    console.addListener(_connectionChanged);
  }

  final ConsoleState console;
  final FlutterSecureStorage storage;
  final bool backgroundWorker;
  final void Function(CoreProfile, Incident)? onReview;
  final void Function(String)? onFailure;
  final Future<void> Function(CoreProfile)? observePlatformDelivery;
  static const _background = MethodChannel(
    'com.stillwatch.companion/background-alerts',
  );
  final _notifications = FlutterLocalNotificationsPlugin();
  final _active = <String, DeliveredAlert>{};
  static const _preference = 'stillwatch.alerts.foreground';
  static const _blockedAtKey = 'stillwatch.alerts.permission_blocked_at';
  static const _indexKey = 'stillwatch.alerts.delivery.v1';
  static const _urgentChannel = 'stillwatch-urgent';
  static const _maxDelivered = 512;
  bool enabled = false;
  bool available = false;
  String backgroundState = 'stopped';
  bool _startingBackground = false;
  DateTime? permissionBlockedAt;
  bool _busy = false;
  bool _changingPreference = false;
  bool _closed = false;
  bool _initializing = true;
  Future<bool>? _permissionCheck;
  Timer? _poll;
  Timer? _expiration;

  Future<void> initialize() async {
    try {
      await _notifications.initialize(
        settings: InitializationSettings(
          android: const AndroidInitializationSettings('ic_notification'),
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
      await _restoreDelivered();
      await _refreshPermissions();
      await refreshBackgroundState();
      try {
        final launch = await _notifications.getNotificationAppLaunchDetails();
        final response = launch?.notificationResponse;
        if (launch?.didNotificationLaunchApp == true && response != null) {
          unawaited(_action(response));
        }
      } catch (_) {
        // Some desktop services do not report activation after launch.
      }
    } catch (_) {
      available = false;
    } finally {
      _initializing = false;
      _connectionChanged();
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
      if (!value &&
          Platform.isAndroid &&
          !backgroundWorker &&
          backgroundState != 'stopped') {
        await setBackground(false);
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
      unawaited(refreshBackgroundState());
    }
  }

  Future<void> refreshBackgroundState() async {
    if (!Platform.isAndroid || backgroundWorker || _closed) return;
    try {
      final state =
          await _background.invokeMethod<String>('state') ?? 'stopped';
      if (_closed || state == backgroundState) return;
      backgroundState = state;
      if (state == 'stopped') {
        _active.clear();
        await _restoreDelivered();
      }
      _connectionChanged();
      if (!_closed) notifyListeners();
    } on PlatformException {
      // The platform monitor has not been attached.
    } on MissingPluginException {
      // This embedding has no background monitor.
    }
  }

  Future<void> setBackground(bool value) async {
    if (!Platform.isAndroid || backgroundWorker || _startingBackground) return;
    _startingBackground = true;
    try {
      if (value) {
        final profile = console.selected;
        if (profile == null || console.link != LinkState.connected) {
          throw const StillWatchException('background_unavailable');
        }
        if (!enabled) await setEnabled(true);
        backgroundState = 'connecting';
        _poll?.cancel();
        _poll = null;
        while (_busy) {
          await Future<void>.delayed(const Duration(milliseconds: 50));
        }
        await _background.invokeMethod<void>('start', {'core': profile.id});
      } else {
        await _background.invokeMethod<void>('stop');
      }
      await refreshBackgroundState();
    } on PlatformException {
      backgroundState = 'stopped';
      throw const StillWatchException('background_unavailable');
    } finally {
      _startingBackground = false;
      _connectionChanged();
      if (!_closed) notifyListeners();
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
    if (_closed || _initializing) return;
    if (!backgroundWorker &&
        (backgroundState != 'stopped' || _startingBackground)) {
      _poll ??= Timer.periodic(const Duration(seconds: 3), (_) {
        if (console.foreground) unawaited(refreshBackgroundState());
      });
      return;
    }
    if (!enabled ||
        !available ||
        !console.foreground ||
        console.link != LinkState.connected ||
        _changingPreference) {
      _poll?.cancel();
      _poll = null;
      if (!_busy && !_changingPreference) unawaited(_sync());
      return;
    }
    if (_poll == null) {
      _poll = Timer.periodic(
        const Duration(seconds: 3),
        (_) => unawaited(_pollNotifications()),
      );
      unawaited(_pollNotifications());
    } else if (!_busy) {
      unawaited(_pollNotifications());
    }
  }

  Future<void> _pollNotifications() async {
    await refreshBackgroundState();
    await _sync();
  }

  Future<void> _sync() async {
    final profile = console.selected;
    if (_busy ||
        _closed ||
        _changingPreference ||
        _initializing ||
        _startingBackground ||
        (!backgroundWorker && backgroundState != 'stopped')) {
      return;
    }
    _busy = true;
    try {
      final profiles = console.profiles.toList();
      final now = DateTime.now().toUtc();
      await _removeWhere(
        (item) =>
            !enabled ||
            (permissionBlockedAt != null && available) ||
            !item.expiresAt.isAfter(now) ||
            (!console.loading && !profiles.any(item.belongsTo)) ||
            (profile != null &&
                item.belongsTo(profile) &&
                (console.link == LinkState.revoked ||
                    console.link == LinkState.untrusted)),
      );
      if (profile == null || !_canNotify(profile)) return;
      if (!await _refreshPermissions()) {
        if (permissionBlockedAt != null) await _removeWhere((_) => true);
        return;
      }
      if (!_canNotify(profile)) return;
      await observePlatformDelivery?.call(profile);
      if (!_canNotify(profile)) return;
      final current = <String>{};
      final tracked = _active.values
          .where((item) => item.belongsTo(profile))
          .map((item) => item.key)
          .toSet();
      String? after;
      String? generation;
      String? head;
      var complete = false;
      final deadline = DateTime.now().add(const Duration(seconds: 105));
      do {
        final page = await profile.client.alertPage(after: after);
        if (!_canNotify(profile)) return;
        final pageGeneration = page.generation ?? console.streamGeneration;
        if (pageGeneration == null ||
            (generation != null && generation != pageGeneration)) {
          return;
        }
        if (head != null && head != page.head) return;
        generation = pageGeneration;
        head = page.head;
        for (final alert in page.alerts) {
          if (!_canNotify(profile)) return;
          if (alert['core_id'] != profile.id ||
              alert['attention'] != 'urgent') {
            continue;
          }
          final entry = DeliveredAlert.fromFeed(profile, generation, alert);
          if (!entry.expiresAt.isAfter(DateTime.now())) continue;
          current.add(entry.key);
          final previous = _active[entry.key];
          if (previous != null) {
            // The action stays bound to the revision that was presented.
            if (previous.presented && alert['client_seen_at'] == null) {
              await _receipt(profile, entry.id, entry.cycle);
            }
            if (previous.presented) continue;
          }
          if (alert['client_seen_at'] != null) continue;
          if (previous == null && _active.length >= _maxDelivered) continue;
          final pending = entry.withHandle(
            previous?.handle ?? _allocateHandle(entry.key),
          );
          await _present(profile, pending);
        }
        // An older server has an unpaged feed. Absence there cannot clear an alert.
        complete =
            page.generation != null && page.head != null && page.next == null;
        if (page.next != null && page.next == after) return;
        after = page.next;
      } while (after != null && DateTime.now().isBefore(deadline));
      if (_canNotify(profile) && complete) {
        final ceiling = BigInt.parse(head!);
        await _removeWhere(
          (item) =>
              tracked.contains(item.key) &&
              !current.contains(item.key) &&
              item.generation == generation &&
              BigInt.parse(item.id) <= ceiling,
        );
      }
      if (_canNotify(profile)) {
        await _removeWhere(
          (item) => item.belongsTo(profile) && item.generation != generation,
        );
      }
    } on StillWatchException catch (error) {
      if (profile != null &&
          (error.status == 401 ||
              error.code == 'credentials' ||
              error.code == 'identity')) {
        await _removeWhere((item) => item.belongsTo(profile));
      }
    } catch (_) {
      // A later poll resumes from a fresh authoritative page.
    } finally {
      _busy = false;
      _scheduleExpiry();
    }
  }

  Future<void> _present(CoreProfile profile, DeliveredAlert pending) async {
    _active[pending.key] = pending;
    await _saveIndex();
    if (!_canNotify(profile)) return;
    final ttl = pending.expiresAt.difference(DateTime.now()).inMilliseconds;
    if (ttl <= 0) return;
    await _notifications.show(
      id: pending.handle,
      title: 'StillWatch alert',
      body: 'Open StillWatch to review an urgent incident.',
      notificationDetails: NotificationDetails(
        android: AndroidNotificationDetails(
          _urgentChannel,
          'Urgent incidents',
          importance: Importance.high,
          priority: Priority.high,
          visibility: NotificationVisibility.private,
          timeoutAfter: ttl,
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
        linux: LinuxNotificationDetails(timeout: LinuxNotificationTimeout(ttl)),
      ),
      payload: jsonEncode(pending.toJson()),
    );
    _active[pending.key] = pending.markPresented();
    await _saveIndex();
    if (_canNotify(profile)) await _receipt(profile, pending.id, pending.cycle);
  }

  bool _canNotify(CoreProfile profile) =>
      enabled &&
      !_closed &&
      !_changingPreference &&
      (backgroundWorker || backgroundState == 'stopped') &&
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
    if (backgroundWorker || response.payload == null) return;
    try {
      final payload = object(jsonDecode(response.payload!));
      final delivery = DeliveredAlert.fromJson(payload);
      final profile = console.profiles.where(delivery.belongsTo).firstOrNull;
      if (profile == null) throw const StillWatchException('credentials');
      if (console.selected != profile) await console.select(profile);
      final page = await profile.client.alertPage(limit: 1);
      final generation = page.generation ?? console.streamGeneration;
      if (generation != delivery.generation) {
        throw const StillWatchException('revision_conflict');
      }
      var incident = await profile.client.incident(delivery.incident);
      if (_closed || console.selected != profile) return;
      console.replaceIncident(incident);
      if (response.actionId == 'ack') {
        if (incident.lifecycle == 'open' &&
            !incident.acknowledged &&
            incident.attentionCycle == delivery.cycle &&
            incident.revision == delivery.revision &&
            delivery.expiresAt.isAfter(DateTime.now()) &&
            console.link == LinkState.connected &&
            console.foreground &&
            profile.can('events_ack')) {
          try {
            await console.queueIncidentAction(
              profile,
              incident,
              generation,
              IncidentAction.acknowledge,
              operationKey:
                  'notification-${delivery.incident}-${delivery.cycle}-${delivery.revision}',
            );
            unawaited(_sync());
            return;
          } on StillWatchException catch (error) {
            if (error.code != 'revision_conflict') rethrow;
            incident = await profile.client.incident(delivery.incident);
          }
        }
        onFailure?.call(
          'Acknowledgement skipped. Review the current incident.',
        );
      }
      if (!_closed && console.selected == profile) {
        onReview?.call(profile, incident);
      }
    } on StillWatchException catch (error) {
      if (!_closed) onFailure?.call(describeFailure(error));
    } catch (_) {
      if (!_closed) onFailure?.call('This notification could not be opened.');
    }
  }

  int _allocateHandle(String key) {
    final bytes = sha256.convert(utf8.encode(key)).bytes;
    var handle =
        (bytes[0] & 0x7f) << 24 | bytes[1] << 16 | bytes[2] << 8 | bytes[3];
    final used = _active.values.map((item) => item.handle).toSet();
    while (used.contains(handle)) {
      handle = (handle + 1) & 0x7fffffff;
    }
    return handle;
  }

  Future<void> _restoreDelivered() async {
    final raw = await storage.read(key: _indexKey);
    if (raw != null && raw.length <= 512 * 1024) {
      final values = jsonDecode(raw);
      if (values is List) {
        for (final value in values.take(_maxDelivered)) {
          try {
            final item = DeliveredAlert.fromJson(object(value));
            _active[item.key] = item;
          } catch (_) {
            // A malformed entry cannot be used for an acknowledgement.
          }
        }
      }
    }
    try {
      final delivered = await _notifications.getActiveNotifications();
      for (final item in delivered) {
        final id = item.id;
        if (id == null) continue;
        final index = _active.values
            .where((entry) => entry.handle == id)
            .firstOrNull;
        if (index != null) {
          _active[index.key] = index.markPresented();
        } else if (item.payload != null) {
          try {
            final entry = DeliveredAlert.fromJson(
              object(jsonDecode(item.payload!)),
            );
            if (entry.handle == id && _active.length < _maxDelivered) {
              _active[entry.key] = entry.markPresented();
            }
          } catch (_) {
            // Platform-owned alerts can have another payload shape.
          }
        }
      }
    } catch (_) {
      // Some notification services do not enumerate past deliveries.
    }
    await _saveIndex();
  }

  Future<void> _saveIndex() => storage.write(
    key: _indexKey,
    value: jsonEncode(_active.values.map((item) => item.toJson()).toList()),
  );

  Future<void> _removeWhere(bool Function(DeliveredAlert) predicate) async {
    var changed = false;
    for (final item in _active.values.where(predicate).toList()) {
      try {
        await _notifications.cancel(id: item.handle);
        _active.remove(item.key);
        changed = true;
      } catch (_) {
        // Retry cancellation after the notification service resumes.
      }
    }
    if (changed) await _saveIndex();
  }

  void _scheduleExpiry() {
    _expiration?.cancel();
    if (_closed || _active.isEmpty) return;
    final earliest = _active.values
        .map((item) => item.expiresAt)
        .reduce((a, b) => a.isBefore(b) ? a : b);
    final delay = earliest.difference(DateTime.now());
    _expiration = Timer(
      delay.isNegative ? const Duration(seconds: 3) : delay,
      () => unawaited(_sync()),
    );
  }

  @override
  void dispose() {
    _closed = true;
    _poll?.cancel();
    _expiration?.cancel();
    console.removeListener(_connectionChanged);
    super.dispose();
  }
}

final class DeliveredAlert {
  const DeliveredAlert(
    this.core,
    this.device,
    this.generation,
    this.incident,
    this.cycle,
    this.revision,
    this.id,
    this.expiresAt,
    this.handle,
    this.presented,
  );

  factory DeliveredAlert.fromFeed(
    CoreProfile profile,
    String generation,
    Json value,
  ) => DeliveredAlert(
    profile.id,
    profile.deviceId,
    generation,
    uuid(value['incident_id']),
    counter(value['attention_cycle']),
    counter(value['revision']),
    counter(value['id']),
    timestamp(value, 'expires_at'),
    0,
    false,
  );

  factory DeliveredAlert.fromJson(Json value) {
    final handle = value['handle'];
    if (handle is! int || handle < 0 || handle > 0x7fffffff) {
      throw const StillWatchException('protocol');
    }
    return DeliveredAlert(
      uuid(value['core']),
      uuid(value['device']),
      uuid(value['generation']),
      uuid(value['incident']),
      counter(value['cycle']),
      counter(value['revision']),
      counter(value['id']),
      timestamp(value, 'expires'),
      handle,
      value['presented'] == true,
    );
  }

  final String core, device, generation, incident, cycle, revision, id;
  final DateTime expiresAt;
  final int handle;
  final bool presented;

  String get key => '$core:$device:$generation:$incident:$cycle';
  bool belongsTo(CoreProfile profile) =>
      core == profile.id && device == profile.deviceId;

  DeliveredAlert withHandle(int value) => DeliveredAlert(
    core,
    device,
    generation,
    incident,
    cycle,
    revision,
    id,
    expiresAt,
    value,
    presented,
  );
  DeliveredAlert markPresented() => DeliveredAlert(
    core,
    device,
    generation,
    incident,
    cycle,
    revision,
    id,
    expiresAt,
    handle,
    true,
  );

  Json toJson() => {
    'core': core,
    'device': device,
    'generation': generation,
    'incident': incident,
    'cycle': cycle,
    'revision': revision,
    'id': id,
    'expires': expiresAt.toIso8601String(),
    'handle': handle,
    'presented': presented,
  };
}
