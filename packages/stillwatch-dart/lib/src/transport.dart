import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:crypto/crypto.dart';

import 'wire.dart';

const totalDeadline = Duration(seconds: 12);
const streamIdle = Duration(seconds: 25);
const _connectDeadline = Duration(seconds: 3);
const maxJsonBytes = 4 * 1024 * 1024;

Uri coreOrigin(String address, {bool loopback = false}) {
  final uri = Uri.tryParse(address);
  if (uri == null ||
      uri.scheme != (loopback ? 'http' : 'https') ||
      uri.host.isEmpty ||
      uri.userInfo.isNotEmpty ||
      uri.authority.contains('@') ||
      uri.hasQuery ||
      uri.hasFragment ||
      (uri.path.isNotEmpty && uri.path != '/') ||
      uri.port < 1 ||
      uri.port > 65535 ||
      (loopback &&
          !(InternetAddress.tryParse(uri.host)?.isLoopback ?? false))) {
    throw const StillWatchException('endpoint');
  }
  return uri.replace(path: '/');
}

bool validToken(String value) => RegExp(r'^[a-f0-9]{64}$').hasMatch(value);

final class Deadline {
  Deadline([this.budget = totalDeadline]);

  final Duration budget;
  final Stopwatch _watch = Stopwatch()..start();

  Duration get remaining => budget - _watch.elapsed;

  Future<T> wait<T>(Future<T> value, {FutureOr<T> Function()? onTimeout}) {
    final left = remaining;
    return value.timeout(
      left.isNegative ? Duration.zero : left,
      onTimeout: onTimeout ?? () => throw const StillWatchException('deadline'),
    );
  }
}

final class Transport {
  Transport(this.origin, {this.der, this.pin}) : _client = _openClient(der);

  final Uri origin;
  final Uint8List? der;
  final String? pin;
  final HttpClient _client;

  static HttpClient _openClient(Uint8List? der) {
    SecurityContext? context;
    if (der != null) {
      context = SecurityContext(withTrustedRoots: false)
        ..minimumTlsProtocolVersion = TlsProtocolVersion.tls1_3;
      final certificate = Platform.isIOS
          ? der
          : ascii.encode(
              '-----BEGIN CERTIFICATE-----\n${base64.encode(der)}\n-----END CERTIFICATE-----\n',
            );
      try {
        context.setTrustedCertificatesBytes(certificate);
      } on TlsException {
        throw const StillWatchException('endpoint');
      }
    }
    return HttpClient(context: context)
      ..connectionTimeout = _connectDeadline
      ..idleTimeout = const Duration(seconds: 20)
      ..autoUncompress = false
      ..maxConnectionsPerHost = 8
      ..findProxy = ((_) => 'DIRECT');
  }

  Transport fork() => Transport(origin, der: der, pin: pin);

  Future<HttpClientResponse> open(
    String method,
    String path,
    Deadline deadline, {
    String? token,
    Map<String, String> headers = const {},
    List<int>? body,
    Stream<List<int>>? upload,
    int? uploadLength,
  }) async {
    final target = origin.resolve(path);
    if (!path.startsWith('/') ||
        path.startsWith('//') ||
        path.contains(r'\') ||
        target.origin != origin.origin ||
        target.hasFragment) {
      throw const StillWatchException('endpoint');
    }
    if (token != null && !validToken(token)) {
      throw const StillWatchException('credentials');
    }
    try {
      final opening = _client.openUrl(method, target);
      final request = await deadline.wait(
        opening,
        onTimeout: () {
          unawaited(
            opening.then<void>(
              (request) => request.abort(),
              onError: (Object _) {},
            ),
          );
          throw const StillWatchException('deadline');
        },
      );
      request.followRedirects = false;
      request.headers.set('Accept-Encoding', 'identity');
      for (final entry in headers.entries) {
        request.headers.set(entry.key, entry.value);
      }
      if (token != null)
        request.headers.set(HttpHeaders.authorizationHeader, 'Bearer $token');
      if (body != null) {
        request.contentLength = body.length;
        request.add(body);
      }
      if (upload != null) {
        request.contentLength = uploadLength!;
        try {
          await deadline.wait(request.addStream(upload));
        } catch (_) {
          request.abort();
          rethrow;
        }
      }
      final response = await deadline.wait(
        request.close(),
        onTimeout: () {
          final error = const StillWatchException('deadline');
          request.abort(error);
          throw error;
        },
      );
      if (pin != null &&
          (response.certificate == null ||
              sha256.convert(response.certificate!.der).toString() != pin)) {
        await _discard(response);
        throw const StillWatchException('endpoint');
      }
      return response;
    } on StateError {
      throw const StillWatchException('closed');
    } on IOException {
      throw const StillWatchException('transport');
    }
  }

  void close() => _client.close(force: true);
}

Future<void> _discard(HttpClientResponse response) async {
  await response.listen(null).cancel();
}

Future<Uint8List> readResponse(
  HttpClientResponse response,
  Deadline deadline,
  int limit,
) async {
  if (response.contentLength > limit) {
    await _discard(response);
    throw const StillWatchException('protocol');
  }
  final parts = BytesBuilder(copy: false);
  final chunks = StreamIterator(response);
  try {
    while (await deadline.wait(chunks.moveNext())) {
      final data = chunks.current;
      if (data.length + parts.length > limit)
        throw const StillWatchException('protocol');
      parts.add(data);
    }
    return parts.takeBytes();
  } on IOException {
    throw const StillWatchException('transport');
  } finally {
    await chunks.cancel();
  }
}

bool successful(int status) => status >= 200 && status < 300;

StillWatchException problem(int status, List<int> data) {
  try {
    final value = decodeObject(data);
    final code = value['code'];
    if (value['status'] == status &&
        code is String &&
        RegExp(r'^[a-z_]{1,64}$').hasMatch(code)) {
      return StillWatchException(code, status: status);
    }
  } on StillWatchException {
    // A proxy can return an HTML response. Keep the status without its body.
  }
  return StillWatchException('http', status: status);
}

Duration retryAfter(HttpClientResponse response, int attempt) {
  final value = response.headers.value(HttpHeaders.retryAfterHeader);
  if (value != null) {
    final seconds = int.tryParse(value);
    if (seconds != null && seconds >= 0)
      return Duration(seconds: seconds.clamp(0, 3600));
    try {
      final difference = HttpDate.parse(value).difference(DateTime.now());
      return difference.isNegative ? Duration.zero : difference;
    } on HttpException {
      // Use the client backoff for malformed headers.
    }
  }
  return Duration(milliseconds: 150 << attempt);
}
