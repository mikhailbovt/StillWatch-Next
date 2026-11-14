import 'dart:convert';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';
import 'package:cryptography/cryptography.dart';

import 'transport.dart';
import 'wire.dart';

typedef SessionWriter = Future<void> Function(Json credentials);
typedef SessionReader = Future<Json?> Function();
typedef SessionLock = Future<String> Function(Future<String> Function());

final _algorithm = Ed25519();

final class PinnedCore {
  PinnedCore.fromJson(Json value)
    : origin = coreOrigin(text(value, 'endpoint')),
      id = uuid(value['core_instance_id']),
      pin = text(value, 'tls_certificate_sha256'),
      _encoded = text(value, 'tls_certificate_der'),
      _certificate = _decodeDer(text(value, 'tls_certificate_der')) {
    if (sha256.convert(_certificate).toString() != pin) {
      throw const StillWatchException('endpoint');
    }
  }

  final Uri origin;
  final String id;
  final String pin;
  final String _encoded;
  final Uint8List _certificate;

  Json toJson() => {
    'endpoint': origin.toString(),
    'core_instance_id': id,
    'tls_certificate_sha256': pin,
    'tls_certificate_der': _encoded,
  };

  Transport transport() => Transport(origin, der: _certificate, pin: pin);
}

final class PairingOffer {
  PairingOffer.fromJson(Json value)
    : core = PinnedCore.fromJson(value),
      _challenge = uuid(value['challenge']),
      _secret = text(value, 'secret'),
      expires = timestamp(value, 'expires_at') {
    if (object(value['api'])['major'] != 2 || !validToken(_secret)) {
      throw const StillWatchException('credentials');
    }
  }

  final PinnedCore core;
  final String _challenge;
  final String _secret;
  final DateTime expires;

  Future<Enrollment> enroll(String label) async {
    final name = label.trim();
    if (name.isEmpty ||
        name.runes.length > 80 ||
        RegExp(r'[\x00-\x1f\x7f]').hasMatch(name) ||
        !expires.isAfter(DateTime.now())) {
      throw const StillWatchException('credentials');
    }
    final key = await _algorithm.newKeyPair();
    final transport = core.transport();
    try {
      final info = CoreInfo.fromJson(
        await _json(transport, 'GET', '/bootstrap/info'),
      );
      if (info.id != core.id) throw const StillWatchException('identity');
      final signature = await _algorithm.sign([
        ...ascii.encode('stillwatch/pair/v2\x00'),
        ...uuidBytes(core.id),
        ...uuidBytes(_challenge),
        ...ascii.encode(_secret),
        ...utf8.encode(name),
      ], keyPair: key);
      final credentials =
          await _json(transport, 'POST', '/api/v2/pairing/finish', {
            'challenge': _challenge,
            'secret': _secret,
            'label': name,
            'public_key': hex((await key.extractPublicKey()).bytes),
            'signature': hex(signature.bytes),
          });
      _validateSession(credentials);
      return Enrollment(
        core,
        base64Url.encode(await key.extractPrivateKeyBytes()),
        credentials,
      );
    } finally {
      transport.close();
    }
  }
}

final class Enrollment {
  Enrollment(this.core, this.seed, Json credentials)
    : credentials = Map.unmodifiable(credentials);
  final PinnedCore core;
  final String seed;
  final Json credentials;
}

final class DeviceSession {
  DeviceSession(
    this.core,
    String seed,
    Json credentials,
    this._save,
    this._reload,
    this._lock,
  ) : _credentials = Map.unmodifiable(credentials),
      _key = _algorithm.newKeyPairFromSeed(_decodeSeed(seed)) {
    _validateSession(credentials);
    if (credentials['pending_refresh'] case final saved?) {
      _pending = Map.unmodifiable(object(saved));
      if (_pending!['replay_supported'] != true) {
        _terminal = true;
      }
    }
  }

  final PinnedCore core;
  final Future<SimpleKeyPair> _key;
  final SessionWriter _save;
  final SessionReader? _reload;
  final SessionLock? _lock;
  Json _credentials;
  Json? _pending;
  Future<void>? _refreshing;
  bool _dirty = false;
  bool _terminal = false;

  Future<String> access({String? rejected}) async {
    final running = _refreshing;
    if (running != null) {
      await running;
      return access(rejected: rejected);
    }
    if (_terminal) throw const StillWatchException('credentials');
    final accessToken = text(_credentials, 'access_token');
    final expires = timestamp(_credentials, 'access_expires_at');
    final expired = !expires.isAfter(
      DateTime.now().add(const Duration(seconds: 15)),
    );
    if (expired || rejected == accessToken || _dirty || _pending != null) {
      final flight = _renew(rejected);
      _refreshing = flight;
      try {
        await flight;
      } finally {
        _refreshing = null;
      }
    }
    return text(_credentials, 'access_token');
  }

  Future<void> _renew(String? rejected) async {
    Future<String> run() async {
      final reader = _reload;
      if (reader != null) {
        final saved = await reader();
        if (saved == null ||
            object(saved['device'])['id'] !=
                object(_credentials['device'])['id']) {
          _terminal = true;
          throw const StillWatchException('credentials');
        }
        _validateSession(saved);
        if (!_dirty &&
            (saved['refresh_token'] != _credentials['refresh_token'] ||
                saved['pending_refresh'] != null)) {
          _credentials = Map.unmodifiable(saved);
          final pending = saved['pending_refresh'];
          _pending = pending == null ? null : Map.unmodifiable(object(pending));
          if (_pending != null && _pending!['replay_supported'] != true) {
            _terminal = true;
            throw const StillWatchException('credentials');
          }
        }
      }
      final expires = !timestamp(
        _credentials,
        'access_expires_at',
      ).isAfter(DateTime.now().add(const Duration(seconds: 15)));
      await _update(
        expires || rejected == _credentials['access_token'] || _pending != null,
      );
      return text(_credentials, 'access_token');
    }

    if (_lock case final lock?) {
      await lock(run);
    } else {
      await run();
    }
  }

  Future<void> _update(bool refresh) async {
    await _saveReplacement();
    if (refresh) {
      if (!timestamp(
        _credentials,
        'refresh_expires_at',
      ).isAfter(DateTime.now())) {
        _terminal = true;
        throw const StillWatchException('credentials');
      }
      final device = uuid(object(_credentials['device'])['id']);
      final refreshToken = text(_credentials, 'refresh_token');
      final transport = core.transport();
      try {
        final info = await _json(transport, 'GET', '/bootstrap/info');
        if (info['instance_id'] != core.id ||
            object(info['version'])['major'] != 2) {
          throw const StillWatchException('identity');
        }
        if (_pending == null) {
          final nonce = operationId();
          final signature = await _refreshSignature(
            device,
            nonce,
            refreshToken,
          );
          _pending = {
            'replay_supported': info['refresh_replay'] == true,
            'request': {
              'device_id': device,
              'refresh_token': refreshToken,
              'nonce': nonce,
              'signature': signature,
            },
          };
        }
        final pending = _pending!;
        if (pending['replay_supported'] == true &&
            info['refresh_replay'] != true) {
          throw const StillWatchException('credentials');
        }
        final request = object(pending['request']);
        if (request['device_id'] != device ||
            request['refresh_token'] != refreshToken ||
            request['signature'] !=
                await _refreshSignature(
                  device,
                  uuid(request['nonce']),
                  refreshToken,
                )) {
          _terminal = true;
          throw const StillWatchException('credentials');
        }
        await _save({..._credentials, 'pending_refresh': pending});
        _terminal = pending['replay_supported'] != true;
        final credentials = await _json(
          transport,
          'POST',
          '/api/v2/device-session/refresh',
          request,
        );
        try {
          _validateSession(credentials);
          if (object(credentials['device'])['id'] != device) {
            throw const StillWatchException('identity');
          }
        } on StillWatchException {
          _terminal = true;
          rethrow;
        }
        _credentials = Map.unmodifiable(credentials);
        _pending = null;
        _dirty = true;
        _terminal = false;
      } on StillWatchException catch (error) {
        if (error.status == 401 || error.status == 403) {
          _terminal = true;
          throw const StillWatchException('credentials');
        }
        rethrow;
      } finally {
        transport.close();
      }
    }
    await _saveReplacement();
  }

  Future<String> _refreshSignature(
    String device,
    String nonce,
    String token,
  ) async {
    final signature = await _algorithm.sign([
      ...ascii.encode('stillwatch/refresh/v2\x00'),
      ...uuidBytes(core.id),
      ...uuidBytes(device),
      ...uuidBytes(nonce),
      ...ascii.encode(token),
    ], keyPair: await _key);
    return hex(signature.bytes);
  }

  Future<void> _saveReplacement() async {
    if (_dirty) {
      await _save(_credentials);
      _dirty = false;
    }
  }
}

Future<Json> _json(
  Transport transport,
  String method,
  String path, [
  Json? body,
]) async {
  final deadline = Deadline();
  final response = await transport.open(
    method,
    path,
    deadline,
    headers: body == null
        ? const {}
        : const {'Content-Type': 'application/json'},
    body: body == null ? null : utf8.encode(jsonEncode(body)),
  );
  final data = await readResponse(response, deadline, 16 * 1024);
  if (!successful(response.statusCode))
    throw problem(response.statusCode, data);
  return decodeObject(data);
}

void _validateSession(Json value) {
  final device = object(value['device']);
  uuid(device['id']);
  if (device['revoked_at'] != null ||
      !validToken(text(value, 'access_token')) ||
      !validToken(text(value, 'refresh_token')) ||
      !timestamp(
        value,
        'refresh_expires_at',
      ).isAfter(timestamp(value, 'access_expires_at'))) {
    throw const StillWatchException('credentials');
  }
}

Uint8List _decodeDer(String value) {
  if (value.isEmpty ||
      value.length > 8192 ||
      !RegExp(r'^[A-Za-z0-9_-]+$').hasMatch(value)) {
    throw const StillWatchException('endpoint');
  }
  try {
    return base64Url.decode(base64Url.normalize(value));
  } on FormatException {
    throw const StillWatchException('endpoint');
  }
}

Uint8List _decodeSeed(String value) {
  try {
    final bytes = base64Url.decode(value);
    if (bytes.length != 32) throw const StillWatchException('credentials');
    return bytes;
  } on FormatException {
    throw const StillWatchException('credentials');
  }
}
