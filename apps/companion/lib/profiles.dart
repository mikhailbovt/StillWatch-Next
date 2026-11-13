import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

enum LinkState { connecting, connected, offline, untrusted, revoked }

final class CoreProfile {
  CoreProfile(this.name, this.core, this.seed, this.credentials, this._save) {
    client = StillWatchClient.paired(
      core,
      seed: seed,
      credentials: credentials,
      save: (value) async {
        await _save(this, value);
        credentials = value;
      },
    );
  }

  final String name;
  final PinnedCore core;
  final String seed;
  Json credentials;
  final Future<void> Function(CoreProfile, Json) _save;
  late final StillWatchClient client;

  String get id => core.id;
  String get role => text(object(credentials['device']), 'role');
  String get deviceId => uuid(object(credentials['device'])['id']);
  bool can(String scope) =>
      role == 'owner' ||
      (object(credentials['device'])['scopes'] as List?)?.contains(scope) ==
          true;

  Json serialize([Json? next]) => {
    'name': name,
    'core': core.toJson(),
    'seed': seed,
    'credentials': next ?? credentials,
  };
}

final class ProfileStore {
  const ProfileStore(this._secure);
  final FlutterSecureStorage _secure;
  static const _prefix = 'stillwatch.profile.v1.';

  Future<List<CoreProfile>> load() async {
    final values = await _secure.readAll();
    final profiles = <CoreProfile>[];
    for (final entry in values.entries.where(
      (entry) => entry.key.startsWith(_prefix),
    )) {
      try {
        final value = object(jsonDecode(entry.value));
        final core = PinnedCore.fromJson(object(value['core']));
        if (entry.key != '$_prefix${core.id}') continue;
        profiles.add(
          CoreProfile(
            text(value, 'name'),
            core,
            text(value, 'seed'),
            object(value['credentials']),
            save,
          ),
        );
      } on FormatException {
        // An invalid record does not prevent other Cores from connecting.
      } on StillWatchException {
        // An invalid trust record requires a fresh pairing offer.
      }
    }
    return profiles..sort((a, b) => a.name.compareTo(b.name));
  }

  Future<void> save(CoreProfile profile, Json credentials) => _secure.write(
    key: '$_prefix${profile.id}',
    value: jsonEncode(profile.serialize(credentials)),
  );

  Future<void> remove(CoreProfile profile) =>
      _secure.delete(key: '$_prefix${profile.id}');
}

final class ConsoleState extends ChangeNotifier {
  ConsoleState(this.store);
  final ProfileStore store;

  List<CoreProfile> profiles = const [];
  CoreProfile? selected;
  LinkState link = LinkState.connecting;
  List<Camera> cameras = const [];
  List<Incident> incidents = const [];
  String? historyCursor;
  Json? health;
  String? error;
  DateTime? lastSync;
  bool loading = true;
  bool foreground = true;

  int _generation = 0;
  bool _disposed = false;
  bool _fetching = false;
  StreamSubscription<Change>? _events;
  Timer? _poll;
  Timer? _reconnect;

  bool _current(int generation) => !_disposed && generation == _generation;

  Future<void> load() async {
    try {
      profiles = await store.load();
      if (_disposed) return;
      loading = false;
      await select(profiles.firstOrNull);
    } catch (_) {
      if (_disposed) return;
      error = 'Secure storage is unavailable. Unlock the device key store and retry.';
      loading = false;
      notifyListeners();
    }
  }

  Future<void> enroll(PairingOffer offer, String label, String name) async {
    final previous = profiles
        .where((profile) => profile.id == offer.core.id)
        .firstOrNull;
    if (previous != null && previous.core.pin != offer.core.pin) {
      throw const StillWatchException('identity');
    }
    final enrollment = await offer.enroll(label);
    final profile = CoreProfile(
      name,
      enrollment.core,
      enrollment.seed,
      enrollment.credentials,
      store.save,
    );
    try {
      await store.save(profile, enrollment.credentials);
    } catch (_) {
      profile.client.close();
      throw const StillWatchException('secure_storage');
    }
    if (_disposed) {
      profile.client.close();
      return;
    }
    profiles = [...profiles.where((item) => item.id != profile.id), profile]
      ..sort((a, b) => a.name.compareTo(b.name));
    await select(profile);
    previous?.client.close();
  }

  Future<void> forget(CoreProfile profile) async {
    await store.remove(profile);
    profiles = profiles
        .where((item) => item.id != profile.id)
        .toList(growable: false);
    if (selected == profile) await select(profiles.firstOrNull);
    profile.client.close();
    if (!_disposed) notifyListeners();
  }

  Future<void> select(CoreProfile? profile) async {
    final generation = ++_generation;
    _cancelTimers();
    selected = profile;
    cameras = const [];
    incidents = const [];
    historyCursor = null;
    health = null;
    error = null;
    lastSync = null;
    link = LinkState.connecting;
    notifyListeners();
    await _stopEvents();
    if (profile != null && foreground && _current(generation)) {
      await _connect(profile, generation);
    }
  }

  Future<void> setForeground(bool value) async {
    if (_disposed || foreground == value) return;
    foreground = value;
    final generation = ++_generation;
    _cancelTimers();
    notifyListeners();
    await _stopEvents();
    final profile = selected;
    if (value && profile != null && _current(generation)) {
      await _connect(profile, generation);
    }
  }

  Future<void> reconnect() async {
    final profile = selected;
    if (profile == null || !foreground) return;
    final generation = ++_generation;
    _cancelTimers();
    await _stopEvents();
    if (_current(generation)) await _connect(profile, generation);
  }

  Future<void> _connect(CoreProfile profile, int generation) async {
    link = LinkState.connecting;
    notifyListeners();
    try {
      await profile.client.core();
      final snapshot = await profile.client.snapshot();
      final results = await Future.wait<Object>([
        profile.client.cameras(),
        profile.client.incidents(),
        profile.client.health().onError<StillWatchException>((error, _) {
          if (error.status == 503) return {'state': 'degraded'};
          throw error;
        }),
      ]);
      if (!_current(generation)) return;
      cameras = (results[0] as Page<Camera>).items;
      final history = results[1] as Page<Incident>;
      final merged = <String, Incident>{
        for (final incident in snapshot.incidents) incident.id: incident,
      };
      for (final item in history.items) {
        final existing = merged[item.id];
        if (existing == null || _newer(item, existing)) merged[item.id] = item;
      }
      incidents = _sort(merged.values);
      historyCursor = history.next;
      health = results[2] as Json;
      lastSync = DateTime.now();
      link = LinkState.connected;
      error = null;
      _events = profile.client
          .changes(cursor: snapshot.cursor)
          .listen(
            (change) {
              if (!_current(generation)) return;
              final incident = change.incident;
              if (incident != null) replaceIncident(incident);
            },
            onError: (Object error) => _linkFailure(error, generation),
            onDone: () {
              if (_current(generation) && link == LinkState.connected) {
                _linkFailure(
                  const StillWatchException('transport'),
                  generation,
                );
              }
            },
          );
      _poll = Timer.periodic(
        const Duration(seconds: 4),
        (_) => unawaited(refreshCameras()),
      );
      notifyListeners();
    } catch (failure) {
      _linkFailure(failure, generation);
    }
  }

  Future<void> refreshCameras({String? after}) async {
    final profile = selected;
    if (profile == null ||
        !foreground ||
        _fetching ||
        link != LinkState.connected) {
      return;
    }
    _fetching = true;
    final generation = _generation;
    try {
      final visible = <Camera>[];
      String? cursor = after;
      do {
        final page = await profile.client.cameras(after: cursor);
        if (!_current(generation)) return;
        visible.addAll(page.items);
        cursor = page.next;
      } while (cursor != null && visible.length < 500);
      cameras = List.unmodifiable(visible);
      lastSync = DateTime.now();
      notifyListeners();
    } catch (failure) {
      _linkFailure(failure, generation);
    } finally {
      _fetching = false;
    }
  }

  Future<void> loadOlder() async {
    final profile = selected;
    final cursor = historyCursor;
    if (profile == null || cursor == null || link != LinkState.connected) {
      return;
    }
    final generation = _generation;
    final page = await profile.client.incidents(after: cursor);
    if (!_current(generation)) return;
    final merged = <String, Incident>{
      for (final item in incidents) item.id: item,
    };
    for (final item in page.items) {
      final existing = merged[item.id];
      if (existing == null || _newer(item, existing)) merged[item.id] = item;
    }
    incidents = _sort(merged.values);
    historyCursor = page.next;
    notifyListeners();
  }

  void replaceIncident(Incident incident) {
    final prior = incidents.where((item) => item.id == incident.id).firstOrNull;
    if (prior != null && !_newer(incident, prior)) return;
    incidents = _sort([
      ...incidents.where((item) => item.id != incident.id),
      incident,
    ]);
    notifyListeners();
  }

  void _linkFailure(Object failure, int generation) {
    if (!_current(generation)) return;
    final code = failure is StillWatchException ? failure.code : 'transport';
    final status = failure is StillWatchException ? failure.status : null;
    link = switch (code) {
      'identity' || 'endpoint' => LinkState.untrusted,
      'unauthorized' || 'credentials' => LinkState.revoked,
      _ => status == 401 ? LinkState.revoked : LinkState.offline,
    };
    error = describeFailure(failure);
    _cancelTimers();
    unawaited(_stopEvents());
    if (link == LinkState.revoked || link == LinkState.untrusted) {
      cameras = const [];
      incidents = const [];
      historyCursor = null;
    }
    if (link == LinkState.offline && foreground) {
      _reconnect = Timer(
        Duration(seconds: code == 'cursor_expired' ? 0 : 4),
        () => unawaited(reconnect()),
      );
    }
    notifyListeners();
  }

  Future<void> _stopEvents() async {
    final events = _events;
    _events = null;
    await events?.cancel();
  }

  void _cancelTimers() {
    _poll?.cancel();
    _reconnect?.cancel();
  }

  @override
  void dispose() {
    _disposed = true;
    _cancelTimers();
    unawaited(_stopEvents());
    for (final profile in profiles) {
      profile.client.close();
    }
    super.dispose();
  }
}

bool _newer(Incident next, Incident previous) =>
    BigInt.parse(next.revision) > BigInt.parse(previous.revision);

List<Incident> _sort(Iterable<Incident> value) =>
    value.toList()..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));

String describeFailure(Object error) {
  if (error is! StillWatchException) {
    return 'The operation could not finish. Try again.';
  }
  return switch (error.code) {
    'endpoint' || 'identity' => 'The Core identity could not be verified. Check the fingerprint with its owner.',
    'unauthorized' || 'credentials' =>
      'This device session expired or was revoked. Pair with a new code.',
    'forbidden' => 'This action is restricted by the Core access policy.',
    'revision_conflict' =>
      'The Core has a newer revision. Refresh and review it before retrying.',
    'transport' || 'deadline' =>
      'The Core is unreachable. Check the network or VPN and try again.',
    'capacity_exceeded' => 'The Core reached capacity for this operation.',
    'provider_unavailable' =>
      'The requested service is temporarily unavailable.',
    'secure_storage' => 'The device key store could not save the session.',
    'notification_permission' =>
      'Allow notifications in system settings, then try again.',
    'notification_unavailable' =>
      'The system notification service is unavailable.',
    'invalid_request' || 'protocol' =>
      'The data could not be validated. Check the fields and try again.',
    'not_found' => 'The resource is unavailable to this device.',
    'cursor_expired' => 'The Core has a new event history. Refreshing.',
    _ =>
      'Core rejected the request${error.status == null ? '' : ' (HTTP ${error.status})'}.',
  };
}
