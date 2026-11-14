import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'profiles.dart';

final class IosPush extends ChangeNotifier {
  IosPush(this.console, this.storage, this.onReview, this.onFailure);

  final ConsoleState console;
  final FlutterSecureStorage storage;
  final void Function(CoreProfile, Incident) onReview;
  final void Function(String) onFailure;
  static const _channel = MethodChannel('com.stillwatch.companion/apns');

  CoreProfile? _profile;
  LinkState? _link;
  bool? _permission;
  (String, String)? _registered;
  DateTime? _registeredAt;
  String? _clearedForPermission;
  Timer? _timer;
  bool _closed = false;
  Completer<void>? _syncDone;
  bool _changing = false;
  bool _opening = false;
  final _confirmedNotices = <(String, String)>{};
  DateTime? _lastPresentationCheck;

  bool enabled = false;
  String state = 'off';

  String _key(CoreProfile profile) =>
      'stillwatch.ios-push.${profile.id}.${profile.deviceId}';

  void initialize() {
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'interaction') unawaited(_open());
      if (call.method == 'registrationChanged') unawaited(_sync());
    });
    console.addListener(_connectionChanged);
    _timer = Timer.periodic(const Duration(seconds: 20), (_) {
      if (_permission == false || state == 'unavailable') _permission = null;
      unawaited(_sync());
    });
    _connectionChanged();
    unawaited(_open());
  }

  void resume() {
    _permission = null;
    _registeredAt = null;
    _lastPresentationCheck = null;
    unawaited(_sync());
    unawaited(_open());
  }

  void _connectionChanged() {
    final profile = console.selected;
    if (profile == _profile && console.link == _link) return;
    _profile = profile;
    _link = console.link;
    _lastPresentationCheck = null;
    unawaited(_sync());
    if (console.link == LinkState.connected) unawaited(_open());
  }

  Future<void> observeDelivered(CoreProfile profile) async {
    if (_closed || !console.foreground || console.selected != profile) return;
    final now = DateTime.now();
    final previous = _lastPresentationCheck;
    if (previous != null &&
        now.difference(previous) < const Duration(seconds: 9)) {
      return;
    }
    _lastPresentationCheck = now;
    final notices = await _channel.invokeListMethod<String>('deliveredNotices');
    if (notices == null || _closed || console.selected != profile) return;
    _confirmedNotices.removeWhere((entry) => !notices.contains(entry.$2));
    final budget = Stopwatch()..start();
    for (final notice in notices.take(32)) {
      if (_closed ||
          console.selected != profile ||
          _confirmedNotices.length >= 128 ||
          budget.elapsed >= const Duration(seconds: 1)) {
        return;
      }
      final key = (profile.id, notice);
      if (_confirmedNotices.contains(key)) continue;
      try {
        final response = await profile.client
            .iosPushNotice(notice)
            .timeout(const Duration(seconds: 1));
        final alert = object(response['alert']);
        if (alert['core_id'] != profile.id ||
            response['generation'] != console.streamGeneration) {
          continue;
        }
        await profile.client
            .alertSeen(counter(alert['id']), counter(alert['attention_cycle']))
            .timeout(const Duration(seconds: 1));
        _confirmedNotices.add(key);
      } on StillWatchException catch (error) {
        if (error.status == 404) continue;
        return;
      } on TimeoutException {
        return;
      }
    }
  }

  Future<void> setEnabled(CoreProfile profile, bool value) async {
    if (_closed || _changing || profile != console.selected) return;
    _changing = true;
    try {
      await _syncDone?.future;
      if (value) {
        if (console.link != LinkState.connected ||
            console.coreInfo?.iosPush != true) {
          throw const StillWatchException('notification_unavailable');
        }
        _permission =
            await _channel.invokeMethod<bool>('register', true) == true;
        if (_permission != true) {
          throw const StillWatchException('notification_permission');
        }
      }
      await storage.write(key: _key(profile), value: value ? 'yes' : 'clear');
      _registered = null;
      if (!_closed && console.selected == profile) {
        enabled = value;
        state = value ? 'connecting' : 'disable_pending';
        notifyListeners();
      }
    } finally {
      _changing = false;
    }
    await _sync();
  }

  Future<void> forget(CoreProfile profile) async {
    if (_changing) throw const StillWatchException('notification_unavailable');
    _changing = true;
    try {
      await _syncDone?.future;
      final key = _key(profile);
      if (await storage.read(key: key) == null) return;
      await storage.write(key: key, value: 'clear');
      try {
        await profile.client.unregisterIosPush();
      } on StillWatchException catch (error) {
        if (error.status != 401 && error.status != 404) rethrow;
      }
      await storage.delete(key: key);
    } finally {
      _changing = false;
    }
  }

  Future<void> _sync() async {
    final profile = console.selected;
    if (_closed ||
        _changing ||
        _syncDone != null ||
        !console.foreground ||
        profile == null) {
      return;
    }
    final done = _syncDone = Completer<void>();
    final key = _key(profile);
    var next = 'off';
    var optedIn = false;
    try {
      final setting = await storage.read(key: key);
      optedIn = setting == 'yes';
      if (_closed || _changing || console.selected != profile) return;
      if (setting == 'clear') {
        next = 'disable_pending';
        if (console.link == LinkState.connected) {
          await profile.client.unregisterIosPush();
          await storage.delete(key: key);
          next = 'off';
        }
      } else if (optedIn) {
        next = 'connecting';
        if (console.link != LinkState.connected) return;
        if (console.coreInfo?.iosPush != true) {
          next = 'unavailable';
          return;
        }
        _permission ??=
            await _channel.invokeMethod<bool>('register', false) == true;
        if (_permission != true) {
          next = 'permission';
          if (_clearedForPermission != key) {
            await profile.client.unregisterIosPush();
            _clearedForPermission = key;
            _registered = null;
          }
          return;
        }
        _clearedForPermission = null;
        final registration = await _channel.invokeMapMethod<String, Object?>(
          'token',
        );
        if (registration?['failed'] == true) {
          next = 'unavailable';
          return;
        }
        final token = registration?['token'];
        if (token is! String ||
            _closed ||
            _changing ||
            console.selected != profile) {
          return;
        }
        if (_registered != (key, token) ||
            _registeredAt == null ||
            DateTime.now().difference(_registeredAt!) >=
                const Duration(minutes: 15)) {
          await profile.client.registerIosPush(token);
          _registered = (key, token);
          _registeredAt = DateTime.now();
        }
        next = 'registered';
      }
    } catch (_) {
      if (next != 'disable_pending') next = 'unavailable';
    } finally {
      _syncDone = null;
      done.complete();
      if (!_closed &&
          !_changing &&
          console.selected == profile &&
          (enabled != optedIn || state != next)) {
        enabled = optedIn;
        state = next;
        notifyListeners();
      }
    }
  }

  Future<void> _open() async {
    if (_closed || _opening || !console.foreground) return;
    _opening = true;
    try {
      final notices = await _channel.invokeListMethod<String>(
        'takeInteractions',
      );
      for (final notice in (notices ?? const <String>[]).take(8)) {
        if (_closed) return;
        var opened = false;
        final selected = console.selected;
        final profiles = [
          ?selected,
          ...console.profiles.where((profile) => profile != selected),
        ];
        for (final profile in profiles) {
          try {
            final response = await profile.client.iosPushNotice(notice);
            final alert = object(response['alert']);
            if (alert['core_id'] != profile.id ||
                alert['attention'] != 'urgent') {
              continue;
            }
            final generation = uuid(response['generation']);
            final incident = await profile.client.incident(
              uuid(alert['incident_id']),
            );
            if (incident.attentionCycle != counter(alert['attention_cycle'])) {
              continue;
            }
            if (_closed) return;
            if (console.selected != profile) await console.select(profile);
            if (_closed ||
                console.selected != profile ||
                console.streamGeneration != generation) {
              continue;
            }
            await profile.client.alertSeen(
              counter(alert['id']),
              incident.attentionCycle,
            );
            if (_closed || console.selected != profile) return;
            console.replaceIncident(incident);
            onReview(profile, incident);
            opened = true;
            break;
          } on StillWatchException {
            // The opaque notice identifies its paired device only at its Core.
          }
        }
        if (!_closed && !opened) {
          onFailure(
            'The alert is unavailable. Check the Core connection and incident history.',
          );
        }
      }
    } catch (_) {
      if (!_closed) onFailure('The iOS notification could not be opened.');
    } finally {
      _opening = false;
    }
  }

  @override
  void dispose() {
    _closed = true;
    _timer?.cancel();
    console.removeListener(_connectionChanged);
    _channel.setMethodCallHandler(null);
    super.dispose();
  }
}
