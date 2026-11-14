import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

enum LinkState { connecting, connected, offline, untrusted, revoked }

enum IncidentAction {
  acknowledge('Acknowledge'),
  silence('Silence for 5 min'),
  falseAlarm('False alarm'),
  resolve('Resolve');

  const IncidentAction(this.label);
  final String label;
}

final class PendingIncidentAction {
  PendingIncidentAction._(
    this.action,
    this._reviewed,
    this._streamGeneration,
    this._reason,
    String? key,
  ) : _key = key ?? operationId();

  static const lifetime = Duration(minutes: 2);
  final IncidentAction action;
  final Incident _reviewed;
  final String _streamGeneration;
  final String _reason;
  final String _key;
  final DateTime _expires = DateTime.now().add(lifetime);
  final Stopwatch _age = Stopwatch()..start();
  late final Timer _timer;
  bool _sending = false;
  bool get sending => _sending;
  bool get _expired =>
      _age.elapsed >= lifetime || !_expires.isAfter(DateTime.now());

  bool _matches(Incident current, String? generation) =>
      generation == _streamGeneration &&
      current.id == _reviewed.id &&
      current.cameraId == _reviewed.cameraId &&
      current.revision == _reviewed.revision &&
      current.attentionCycle == _reviewed.attentionCycle &&
      current.evidenceVersion == _reviewed.evidenceVersion &&
      current.packetId == _reviewed.packetId &&
      current.lifecycle == 'open' &&
      (action != IncidentAction.acknowledge || !current.acknowledged) &&
      (action != IncidentAction.falseAlarm || current.packetId != null);

  Future<Incident> _apply(StillWatchClient client) => switch (action) {
    IncidentAction.acknowledge => client.acknowledge(_reviewed, _key),
    IncidentAction.silence => client.silence(_reviewed, 300, _key),
    IncidentAction.falseAlarm => client.feedback(
      _reviewed,
      'false_alarm',
      _reason,
      _key,
    ),
    IncidentAction.resolve => client.resolve(_reviewed, _reason, _key),
  };
}

final class CoreProfile {
  CoreProfile(
    this.name,
    this.core,
    this.seed,
    this.credentials,
    this._save, {
    StillWatchClient? client,
    SessionReader? reload,
    SessionLock? lock,
  }) {
    this.client =
        client ??
        StillWatchClient.paired(
          core,
          seed: seed,
          credentials: credentials,
          reload: reload,
          lock: lock,
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
  static const _locks = MethodChannel('com.stillwatch.companion/session-lock');

  Future<T> _exclusive<T>(String core, Future<T> Function() run) async {
    if (!Platform.isAndroid) return run();
    final handle = await _locks.invokeMethod<int>('acquire', core);
    if (handle == null) throw const StillWatchException('transport');
    try {
      return await run();
    } finally {
      await _locks.invokeMethod<void>('release', handle);
    }
  }

  CoreProfile _profile(
    String name,
    PinnedCore core,
    String seed,
    Json credentials,
  ) => CoreProfile(
    name,
    core,
    seed,
    credentials,
    save,
    reload: !Platform.isAndroid
        ? null
        : () async {
            final raw = await _secure.read(key: '$_prefix${core.id}');
            return raw == null
                ? null
                : object(object(jsonDecode(raw))['credentials']);
          },
    lock: !Platform.isAndroid ? null : (run) => _exclusive(core.id, run),
  );

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
          _profile(
            text(value, 'name'),
            core,
            text(value, 'seed'),
            object(value['credentials']),
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

  Future<void> remove(CoreProfile profile) => _exclusive(
    profile.id,
    () => _secure.delete(key: '$_prefix${profile.id}'),
  );
}

final class ConsoleState extends ChangeNotifier {
  ConsoleState(this.store);
  final ProfileStore store;

  List<CoreProfile> profiles = const [];
  CoreProfile? selected;
  CoreInfo? coreInfo;
  LinkState link = LinkState.connecting;
  List<Camera> cameras = const [];
  List<Incident> incidents = const [];
  bool hasOlderOpenIncidents = false;
  String? historyCursor;
  Json? health;
  String? error;
  DateTime? lastSync;
  String? _streamGeneration;
  String? get streamGeneration => _streamGeneration;
  bool loading = true;
  bool foreground = true;

  final _pendingActions = <(CoreProfile, String), PendingIncidentAction>{};
  int _generation = 0;
  bool _disposed = false;
  bool _fetching = false;
  StreamSubscription<Change>? _events;
  Timer? _poll;
  Timer? _reconnect;

  bool _current(int generation) => !_disposed && generation == _generation;

  Future<void> load({String? preferredCore}) async {
    try {
      profiles = await store.load();
      if (_disposed) return;
      loading = false;
      await select(
        preferredCore == null
            ? profiles.firstOrNull
            : profiles
                  .where((profile) => profile.id == preferredCore)
                  .firstOrNull,
      );
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
    final profile = store._profile(
      name,
      enrollment.core,
      enrollment.seed,
      enrollment.credentials,
    );
    try {
      await store._exclusive(
        profile.id,
        () => store.save(profile, enrollment.credentials),
      );
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
    if (previous != null) _clearPendingActions(previous);
    await select(profile);
    previous?.client.close();
  }

  Future<void> forget(CoreProfile profile) async {
    await store.remove(profile);
    _clearPendingActions(profile);
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
    coreInfo = null;
    cameras = const [];
    incidents = const [];
    hasOlderOpenIncidents = false;
    historyCursor = null;
    health = null;
    error = null;
    lastSync = null;
    _streamGeneration = null;
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
    if (value && selected != null && link == LinkState.connected) {
      link = LinkState.connecting;
    }
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
    if (link == LinkState.connected) {
      link = LinkState.connecting;
      notifyListeners();
    }
    await _stopEvents();
    if (_current(generation)) await _connect(profile, generation);
  }

  Future<void> _connect(CoreProfile profile, int generation) async {
    link = LinkState.connecting;
    notifyListeners();
    try {
      final info = await profile.client.core();
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
      coreInfo = info;
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
      hasOlderOpenIncidents = snapshot.hasOlderOpenIncidents;
      historyCursor = history.next;
      _streamGeneration = snapshot.cursor.generation;
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
        const Duration(seconds: 2),
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
    if (page.next == null) hasOlderOpenIncidents = false;
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

  bool canViewCachedIncidents(CoreProfile profile) =>
      selected == profile &&
      link != LinkState.revoked &&
      link != LinkState.untrusted;

  bool _actionOnline(CoreProfile profile) =>
      selected == profile && foreground && link == LinkState.connected;

  PendingIncidentAction? pendingAction(CoreProfile profile, String id) =>
      _pendingActions[(profile, id)];

  Future<void> queueIncidentAction(
    CoreProfile profile,
    Incident reviewed,
    String? reviewedGeneration,
    IncidentAction action, {
    String reason = '',
    String? operationKey,
  }) async {
    if (_disposed ||
        !foreground ||
        !canViewCachedIncidents(profile) ||
        !profile.can('events_ack')) {
      throw const StillWatchException('forbidden');
    }
    final slot = (profile, reviewed.id);
    if (_pendingActions.containsKey(slot)) {
      throw const StillWatchException('incident_action_pending');
    }
    if (_pendingActions.length >= 32) {
      throw const StillWatchException('incident_action_capacity');
    }
    if (reviewedGeneration == null || reviewedGeneration != streamGeneration) {
      throw const StillWatchException('revision_conflict');
    }
    final online = _actionOnline(profile);
    final pending = PendingIncidentAction._(
      action,
      reviewed,
      reviewedGeneration,
      reason,
      operationKey,
    );
    final latest = incidents
        .where((item) => item.id == reviewed.id)
        .firstOrNull;
    if (!pending._matches(latest ?? reviewed, streamGeneration)) {
      throw const StillWatchException('revision_conflict');
    }
    pending._timer = Timer(PendingIncidentAction.lifetime, () {
      if (!_disposed && !pending.sending) {
        discardIncidentAction(profile, reviewed.id);
      }
    });
    _pendingActions[slot] = pending;
    notifyListeners();
    if (online) await sendPendingAction(profile, reviewed.id);
  }

  Future<void> sendPendingAction(CoreProfile profile, String id) async {
    final slot = (profile, id);
    final pending = _pendingActions[slot];
    if (_disposed ||
        pending == null ||
        pending.sending ||
        !_actionOnline(profile)) {
      return;
    }
    final generation = _generation;
    bool current() =>
        _current(generation) &&
        _actionOnline(profile) &&
        _pendingActions[slot] == pending;
    void clear() {
      if (_pendingActions[slot] == pending) _removePendingAction(slot);
    }

    pending._sending = true;
    notifyListeners();
    var refreshOnConflict = true;
    try {
      if (!current()) return;
      if (!profile.can('events_ack')) {
        throw const StillWatchException('forbidden');
      }
      if (pending._expired) {
        throw const StillWatchException('incident_action_expired');
      }
      final snapshot = await profile.client.snapshot();
      if (!current()) return;
      final latestGeneration = snapshot.cursor.generation;
      if (latestGeneration != streamGeneration) {
        throw const StillWatchException('revision_conflict');
      }
      // The snapshot's generation and revisions were read from one transaction.
      // Older rows require a guarded detail read when the snapshot was bounded.
      var latest = snapshot.incidents
          .where((item) => item.id == id)
          .firstOrNull;
      if (latest == null && snapshot.hasOlderOpenIncidents) {
        try {
          latest = await profile.client.incident(id);
        } on StillWatchException catch (failure) {
          if (failure.status != 404) rethrow;
        }
      }
      if (!current()) return;
      if (latest == null) throw const StillWatchException('revision_conflict');
      replaceIncident(latest);
      if (!current()) return;
      if (pending._expired) {
        throw const StillWatchException('incident_action_expired');
      }
      if (!pending._matches(latest, latestGeneration)) {
        refreshOnConflict = false;
        throw const StillWatchException('revision_conflict');
      }
      final updated = await pending._apply(profile.client);
      clear();
      if (_current(generation) && selected == profile) replaceIncident(updated);
    } on StillWatchException catch (failure) {
      if (!failure.canReconnect) clear();
      if (!_current(generation)) return;
      switch (failure.code) {
        case 'transport' ||
            'deadline' ||
            'unauthorized' ||
            'credentials' ||
            'endpoint' ||
            'identity':
          _linkFailure(failure, generation);
        case 'revision_conflict' when refreshOnConflict:
          unawaited(reconnect());
        default:
          if (failure.status == 401) _linkFailure(failure, generation);
      }
      rethrow;
    } finally {
      pending._sending = false;
      if (pending._expired) clear();
      if (!_disposed) notifyListeners();
    }
  }

  void discardIncidentAction(CoreProfile profile, String id) {
    if (_disposed || pendingAction(profile, id)?.sending == true) return;
    _removePendingAction((profile, id));
    notifyListeners();
  }

  void _removePendingAction((CoreProfile, String) slot) {
    _pendingActions.remove(slot)?._timer.cancel();
  }

  void _clearPendingActions(CoreProfile profile) {
    for (final slot
        in _pendingActions.keys.where((slot) => slot.$1 == profile).toList()) {
      _removePendingAction(slot);
    }
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
      final profile = selected;
      if (profile != null) _clearPendingActions(profile);
      cameras = const [];
      incidents = const [];
      historyCursor = null;
      _streamGeneration = null;
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
    for (final pending in _pendingActions.values) {
      pending._timer.cancel();
    }
    _pendingActions.clear();
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
    'incident_action_pending' => 'This incident already has a pending action.',
    'incident_action_capacity' =>
      'Send or discard a pending action to make room.',
    'incident_action_expired' =>
      'The pending action expired. Review the incident again.',
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
