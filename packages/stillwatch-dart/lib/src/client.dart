import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'changes.dart';
import 'pairing.dart';
import 'transport.dart';
import 'wire.dart';

const _cameraSetupTimeout = Duration(seconds: 22);
const _whepOfferTimeout = Duration(seconds: 28);

final class WhepAnswer {
  const WhepAnswer(this.sdp, this.etag);
  final String sdp;
  final String? etag;
}

final class CameraSnapshot {
  const CameraSnapshot(this.jpeg, this.ageAtReceipt);

  final Uint8List jpeg;
  final Duration? ageAtReceipt;
}

typedef _Access = Future<String> Function({String? rejected});

final class StillWatchClient {
  StillWatchClient._(this._transport, this._access, this._expected);

  factory StillWatchClient.paired(
    PinnedCore core, {
    required String seed,
    required Json credentials,
    required SessionWriter save,
    SessionReader? reload,
    SessionLock? lock,
  }) {
    final session = DeviceSession(core, seed, credentials, save, reload, lock);
    return StillWatchClient._(core.transport(), session.access, core.id);
  }

  factory StillWatchClient.loopback(String endpoint, String token) =>
      StillWatchClient._(
        Transport(coreOrigin(endpoint, loopback: true)),
        ({rejected}) async => token,
        null,
      );

  final Transport _transport;
  final _Access _access;
  final String? _expected;
  final Set<Transport> _streams = {};

  Future<CoreInfo> core() async {
    final info = CoreInfo.fromJson(
      await _json('GET', '/bootstrap/info', authorized: false),
    );
    if (_expected != null && info.id != _expected)
      throw const StillWatchException('identity');
    return info;
  }

  Future<Json> health() => _json('GET', '/health/ready');
  Future<Json> status() => _json('GET', '/api/v2/status');
  Future<Json> semanticStatus() => _json('GET', '/api/v2/semantic-status');

  Future<Page<HealthEvent>> healthEvents({
    String? before,
    int limit = 100,
    bool activeOnly = false,
  }) async {
    final json = await _json(
      'GET',
      Uri(
        path: '/api/v2/health-events',
        queryParameters: {
          'limit': '$limit',
          if (before != null) 'before': counter(before),
          if (activeOnly) 'active_only': 'true',
        },
      ).toString(),
    );
    return Page(
      List.unmodifiable(objects(json, 'events').map(HealthEvent.fromJson)),
      json['next'] == null ? null : counter(json['next']),
    );
  }

  Future<Page<Camera>> cameras({String? after, int limit = 100}) async {
    final json = await _json(
      'GET',
      _page('/api/v2/cameras', after == null ? null : uuid(after), limit),
    );
    return Page(
      List.unmodifiable(objects(json, 'cameras').map(Camera.fromJson)),
      json['next'] == null ? null : uuid(json['next']),
    );
  }

  Future<Camera> camera(String id) async =>
      Camera.fromJson(await _json('GET', '/api/v2/cameras/${uuid(id)}'));

  Future<Json> analysis(String id) =>
      _json('GET', '/api/v2/cameras/${uuid(id)}/analysis-status');

  Future<Json> probe(
    Json source, {
    List<Json> masks = const [],
    String? cameraId,
    Json? sampling,
    String? executionProfileId,
  }) => _json(
    'POST',
    '/api/v2/source-probes',
    body: {
      'source': source,
      'privacy_masks': masks,
      if (cameraId != null) 'camera_id': uuid(cameraId),
      if (sampling != null) 'sampling': sampling,
      if (executionProfileId != null)
        'execution_profile_id': executionProfileId,
    },
    timeout: _cameraSetupTimeout,
  );

  Future<List<Json>> captureDevices() => _list('/api/v2/capture-devices');

  Future<List<Json>> discoverOnvif() =>
      _list('/api/v2/onvif/discovery', method: 'POST');

  Future<List<Json>> onvifProfiles(String endpoint, {String? credential}) =>
      _list(
        '/api/v2/onvif/profiles',
        method: 'POST',
        body: {'device_endpoint': endpoint, 'credential_ref': credential},
      );

  Future<Json> onvifStream(
    String endpoint,
    String media,
    String token, {
    String? credential,
  }) => _json(
    'POST',
    '/api/v2/onvif/stream',
    body: {
      'device_endpoint': endpoint,
      'credential_ref': credential,
      'media': media,
      'token': token,
    },
  );

  Future<Json> uploadReplay(Stream<List<int>> video, int length) async {
    if (length <= 0 || length > 128 * 1024 * 1024) {
      throw const StillWatchException('invalid_request');
    }
    final deadline = Deadline(const Duration(seconds: 125));
    final token = await deadline.wait(_access());
    final response = await _transport.open(
      'POST',
      '/api/v2/replay-uploads',
      deadline,
      token: token,
      headers: {'Content-Type': 'application/octet-stream'},
      upload: video,
      uploadLength: length,
    );
    final bytes = await readResponse(response, deadline, 16 * 1024);
    if (!successful(response.statusCode)) {
      throw problem(response.statusCode, bytes);
    }
    return decodeObject(bytes);
  }

  Future<Camera> createCamera(Json config, String operation) async =>
      Camera.fromJson(
        await _json(
          'POST',
          '/api/v2/cameras',
          body: config,
          operation: operation,
          timeout: _cameraSetupTimeout,
        ),
      );

  Future<Camera> patchCamera(
    Camera current,
    Json patch,
    String operation,
  ) async => Camera.fromJson(
    await _json(
      'PATCH',
      '/api/v2/cameras/${current.id}',
      body: patch,
      operation: operation,
      revision: current.revision,
      timeout: _cameraSetupTimeout,
    ),
  );

  Future<void> deleteCamera(Camera current, String operation) async {
    await _request(
      'DELETE',
      '/api/v2/cameras/${current.id}',
      operation: operation,
      revision: current.revision,
    );
  }

  Future<Snapshot> snapshot() async =>
      Snapshot.fromJson(await _json('GET', '/api/v2/snapshot'));

  Future<List<Json>> alerts() async =>
      objects(await _json('GET', '/api/v2/alerts'), 'alerts');

  Future<AlertPage> alertPage({String? after, int limit = 128}) async =>
      AlertPage.fromJson(
        await _json('GET', _page('/api/v2/alerts', after, limit)),
      );

  Future<void> alertSeen(String id, String cycle) async {
    await _request(
      'POST',
      '/api/v2/alerts/${counter(id)}/seen',
      body: utf8.encode(jsonEncode({'attention_cycle': counter(cycle)})),
      contentType: 'application/json',
    );
  }

  Future<void> registerIosPush(String token) async {
    if (token.length < 2 ||
        token.length > 512 ||
        token.length.isOdd ||
        !RegExp(r'^[a-f0-9]+$').hasMatch(token)) {
      throw const StillWatchException('invalid_request');
    }
    await _request(
      'PUT',
      '/api/v2/alerts/ios/registration',
      body: utf8.encode(jsonEncode({'token': token})),
      contentType: 'application/json',
    );
  }

  Future<void> unregisterIosPush() async {
    await _request('DELETE', '/api/v2/alerts/ios/registration');
  }

  Future<Json> iosPushNotice(String notice) =>
      _json('GET', '/api/v2/alerts/ios/notices/${uuid(notice)}');

  Future<Page<Incident>> incidents({String? after, int limit = 100}) async {
    final json = await _json('GET', _page('/api/v2/incidents', after, limit));
    return Page(
      List.unmodifiable(objects(json, 'incidents').map(Incident.fromJson)),
      json['next'] == null ? null : text(json, 'next'),
    );
  }

  Future<Incident> incident(String id) async =>
      Incident.fromJson(await _json('GET', '/api/v2/incidents/${uuid(id)}'));

  Future<Incident> acknowledge(Incident current, String operation) => _command(
    current,
    '/incidents/${current.id}/acknowledgements',
    const {},
    operation,
  );

  Future<Incident> resolve(Incident current, String reason, String operation) =>
      _command(current, '/incidents/${current.id}/resolution', {
        'reason': reason,
      }, operation);

  Future<Incident> feedback(
    Incident current,
    String classification,
    String reason,
    String operation,
  ) => _command(current, '/incidents/${current.id}/feedback', {
    'classification': classification,
    'reason': reason,
    'evidence_version': current.evidenceVersion,
  }, operation);

  Future<Incident> silence(Incident current, int seconds, String operation) =>
      _command(current, '/alerts/${current.id}/silence', {
        'duration_seconds': seconds,
      }, operation);

  Future<Incident> _command(
    Incident current,
    String path,
    Json body,
    String operation,
  ) async => Incident.fromJson(
    await _json(
      'POST',
      '/api/v2$path',
      body: body,
      operation: operation,
      revision: current.revision,
    ),
  );

  Future<Json> evidence(String incidentId, String packetId) => _json(
    'GET',
    '/api/v2/incidents/${uuid(incidentId)}/evidence/${uuid(packetId)}',
  );

  Future<Uint8List> evidenceImage(
    String incidentId,
    String packetId,
    int ordinal,
  ) async => (await _jpeg(
    '/api/v2/incidents/${uuid(incidentId)}/evidence/${uuid(packetId)}/$ordinal',
    512 * 1024,
  )).$2;

  Future<Uint8List> cameraImage(String cameraId) async =>
      (await cameraSnapshot(cameraId)).jpeg;

  Future<CameraSnapshot> cameraSnapshot(String cameraId) async {
    final requestTime = Stopwatch()..start();
    final (headers, jpeg) = await _jpeg(
      '/api/v2/cameras/${uuid(cameraId)}/snapshot.jpg',
      1024 * 1024,
    );
    final milliseconds = int.tryParse(
      headers.value('x-frame-age-ms') ?? '',
      radix: 10,
    );
    return CameraSnapshot(
      jpeg,
      milliseconds == null || milliseconds < 0 || milliseconds > 60 * 1000
          ? null
          : Duration(milliseconds: milliseconds) + requestTime.elapsed,
    );
  }

  Future<(HttpHeaders, Uint8List)> _jpeg(String path, int limit) async {
    final response = await _request('GET', path, limit: limit);
    if (response.$1.contentType?.mimeType != 'image/jpeg' ||
        response.$2.length < 3 ||
        response.$2[0] != 0xff ||
        response.$2[1] != 0xd8 ||
        response.$2[2] != 0xff) {
      throw const StillWatchException('protocol');
    }
    return response;
  }

  Future<List<Json>> devices() => _list('/api/v2/devices');

  Future<Json> issuePairing({
    required String role,
    List<String> cameras = const [],
    List<String> scopes = const [],
    bool proxy = false,
  }) => _json(
    'POST',
    proxy ? '/api/v2/pairing?profile=proxy' : '/api/v2/pairing',
    body: {
      'role': role,
      'camera_ids': cameras.map(uuid).toList(),
      'scopes': scopes,
    },
  );

  Future<void> revokeDevice(String id) async {
    await _request('DELETE', '/api/v2/devices/${uuid(id)}');
  }

  Future<StreamGrant> streamSession(
    String cameraId, {
    String profile = 'preview',
  }) async => StreamGrant.fromJson(
    await _json(
      'POST',
      '/api/v2/stream-sessions',
      body: {'camera_id': uuid(cameraId), 'profile': profile},
    ),
  );

  Future<WhepAnswer> offer(StreamGrant grant, String sdp) async {
    final (headers, bytes) = await _request(
      'POST',
      grant.path,
      body: utf8.encode(sdp),
      contentType: 'application/sdp',
      limit: 48 * 1024,
      timeout: _whepOfferTimeout,
    );
    if (headers.value(HttpHeaders.locationHeader) != grant.path ||
        headers.contentType?.mimeType != 'application/sdp') {
      throw const StillWatchException('protocol');
    }
    return WhepAnswer(
      utf8.decode(bytes),
      headers.value(HttpHeaders.etagHeader),
    );
  }

  Future<void> trickle(StreamGrant grant, String etag, String fragment) async {
    await _request(
      'PATCH',
      grant.path,
      body: utf8.encode(fragment),
      headers: {'If-Match': etag},
      contentType: 'application/trickle-ice-sdpfrag',
    );
  }

  Future<void> closeStream(StreamGrant grant) async {
    await _request('DELETE', grant.path);
  }

  Stream<Change> changes({Cursor? cursor}) async* {
    final transport = _transport.fork();
    _streams.add(transport);
    try {
      yield* changeStream(transport, _events, cursor);
    } finally {
      _streams.remove(transport);
      transport.close();
    }
  }

  Future<HttpClientResponse> _events(
    Transport transport,
    Cursor? cursor,
  ) async {
    final deadline = Deadline();
    final token = await deadline.wait(_access());
    final headers = {
      'Accept': 'text/event-stream',
      if (cursor != null) 'Last-Event-ID': cursor.toString(),
    };
    final response = await transport.open(
      'GET',
      '/api/v2/changes',
      deadline,
      token: token,
      headers: headers,
    );
    if (response.statusCode != 401) return response;
    await response.listen(null).cancel();
    final renewed = await deadline.wait(_access(rejected: token));
    if (renewed == token)
      throw const StillWatchException('unauthorized', status: 401);
    return transport.open(
      'GET',
      '/api/v2/changes',
      deadline,
      token: renewed,
      headers: headers,
    );
  }

  Future<List<Json>> _list(
    String path, {
    String method = 'GET',
    Json? body,
  }) async {
    final (_, bytes) = await _request(
      method,
      path,
      body: body == null ? null : utf8.encode(jsonEncode(body)),
      contentType: 'application/json',
    );
    try {
      final value = jsonDecode(utf8.decode(bytes));
      if (value is List) return List.unmodifiable(value.map(object));
    } on FormatException {
      throw const StillWatchException('protocol');
    }
    throw const StillWatchException('protocol');
  }

  Future<Json> _json(
    String method,
    String path, {
    Json? body,
    String? operation,
    String? revision,
    bool authorized = true,
    Duration timeout = totalDeadline,
  }) async {
    final (_, bytes) = await _request(
      method,
      path,
      body: body == null ? null : utf8.encode(jsonEncode(body)),
      contentType: 'application/json',
      operation: operation,
      revision: revision,
      authorized: authorized,
      timeout: timeout,
    );
    return decodeObject(bytes);
  }

  Future<(HttpHeaders, Uint8List)> _request(
    String method,
    String path, {
    List<int>? body,
    String? contentType,
    Map<String, String> headers = const {},
    String? operation,
    String? revision,
    bool authorized = true,
    int limit = maxJsonBytes,
    Duration timeout = totalDeadline,
  }) async {
    final deadline = Deadline(timeout);
    var token = authorized ? await deadline.wait(_access()) : null;
    final repeatable = method == 'GET' || operation != null;
    if (operation != null &&
        !RegExp(r'^[A-Za-z0-9_.-]{1,128}$').hasMatch(operation)) {
      throw const StillWatchException('invalid_request');
    }
    final requestHeaders = {
      ...headers,
      if (body != null && contentType != null) 'Content-Type': contentType,
      if (operation != null) 'Idempotency-Key': operation,
      if (revision != null) 'If-Match': '"${counter(revision)}"',
    };
    var refreshed = false;
    for (var attempt = 0; attempt < 3; attempt++) {
      final HttpClientResponse response;
      try {
        response = await _transport.open(
          method,
          path,
          deadline,
          token: token,
          headers: requestHeaders,
          body: body,
        );
      } on StillWatchException catch (error) {
        if (!repeatable || error.code != 'transport' || attempt != 0) rethrow;
        await deadline.wait(
          Future<void>.delayed(const Duration(milliseconds: 150)),
        );
        continue;
      }
      if (successful(response.statusCode)) {
        return (
          response.headers,
          await readResponse(response, deadline, limit),
        );
      }
      final delay = retryAfter(response, attempt);
      final error = problem(
        response.statusCode,
        await readResponse(response, deadline, 16 * 1024),
      );
      if (repeatable &&
          token != null &&
          error.status == 401 &&
          !refreshed &&
          attempt < 2) {
        final replacement = await deadline.wait(_access(rejected: token));
        if (replacement == token) throw error;
        token = replacement;
        refreshed = true;
        continue;
      }
      if (!repeatable ||
          attempt == 2 ||
          !error.canReconnect ||
          delay >= deadline.remaining)
        throw error;
      await deadline.wait(Future<void>.delayed(delay));
    }
    throw const StillWatchException('deadline');
  }

  void close() {
    _transport.close();
    for (final stream in _streams) {
      stream.close();
    }
    _streams.clear();
  }
}

String _page(String path, String? cursor, int limit) => Uri(
  path: path,
  queryParameters: {'limit': '$limit', if (cursor != null) 'after': cursor},
).toString();
