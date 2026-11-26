import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:typed_data';

import 'transport.dart';
import 'wire.dart';

const _maxEvent = 64 * 1024;
final _jitter = Random();

typedef OpenEvents =
    Future<HttpClientResponse> Function(Transport transport, Cursor? cursor);

Stream<Change> changeStream(
  Transport transport,
  OpenEvents open,
  Cursor? cursor,
) {
  late final StreamController<Change> output;
  final stop = Completer<void>();
  Completer<void>? paused;

  Future<void> listen() async {
    var failures = 0;
    try {
      while (!stop.isCompleted) {
        Duration? serverDelay;
        try {
          final response = await open(transport, cursor);
          if (!successful(response.statusCode)) {
            serverDelay = retryAfter(response, failures.clamp(0, 4));
            throw problem(
              response.statusCode,
              await readResponse(response, Deadline(), 16 * 1024),
            );
          }
          if (response.headers.contentType?.mimeType != 'text/event-stream') {
            await response.listen(null).cancel();
            throw const StillWatchException('protocol');
          }
          await for (final change in parseChanges(
            response.timeout(streamIdle),
          )) {
            if (paused != null) await paused!.future;
            if (stop.isCompleted) return;
            final last = cursor;
            if (last != null) {
              if (last.generation != change.cursor.generation) {
                throw const StillWatchException('cursor_expired');
              }
              if (BigInt.parse(change.cursor.sequence) <=
                  BigInt.parse(last.sequence))
                continue;
            }
            output.add(change);
            cursor = change.cursor;
            failures = 0;
          }
        } on IOException {
          // Reconnect to the same pinned origin.
        } on TimeoutException {
          // A quiet Core emits heartbeats, so this connection has stalled.
        } on StillWatchException catch (error) {
          if (!error.canReconnect) rethrow;
        }
        if (stop.isCompleted) return;
        failures = min(5, failures + 1);
        final delay =
            serverDelay ??
            Duration(milliseconds: (200 << failures) + _jitter.nextInt(200));
        await Future.any([Future<void>.delayed(delay), stop.future]);
      }
    } catch (error, stack) {
      if (!stop.isCompleted) output.addError(error, stack);
    } finally {
      transport.close();
      unawaited(output.close());
    }
  }

  output = StreamController<Change>(
    sync: true,
    onListen: () => unawaited(listen()),
    onPause: () => paused ??= Completer<void>(),
    onResume: () {
      paused?.complete();
      paused = null;
    },
    onCancel: () {
      if (!stop.isCompleted) stop.complete();
      paused?.complete();
      paused = null;
      transport.close();
    },
  );
  return output.stream;
}

Stream<Change> parseChanges(Stream<List<int>> input) async* {
  final line = BytesBuilder(copy: false);
  final data = <String>[];
  var event = '';
  var id = '';
  var size = 0;
  var skipLineFeed = false;
  await for (final chunk in input) {
    var start = 0;
    for (var i = 0; i < chunk.length; i++) {
      final byte = chunk[i];
      if (skipLineFeed) {
        skipLineFeed = false;
        if (byte == 10) {
          start = i + 1;
          continue;
        }
      }
      if (byte != 10 && byte != 13) {
        if (line.length + i - start >= _maxEvent)
          throw const StillWatchException('protocol');
        continue;
      }
      line.add(chunk.sublist(start, i));
      skipLineFeed = byte == 13;
      start = i + 1;
      if (line.isEmpty) {
        if (data.isNotEmpty) {
          if (event == 'resync')
            throw const StillWatchException('cursor_expired');
          try {
            yield Change.fromJson(
              id,
              event,
              object(jsonDecode(data.join('\n'))),
            );
          } on FormatException {
            throw const StillWatchException('protocol');
          }
        }
        event = '';
        id = '';
        size = 0;
        data.clear();
        continue;
      }
      final bytes = line.takeBytes();
      if (bytes.first == 58) continue;
      size += bytes.length + 1;
      if (size > _maxEvent) throw const StillWatchException('protocol');
      final String message;
      try {
        message = utf8.decode(bytes);
      } on FormatException {
        throw const StillWatchException('protocol');
      }
      final colon = message.indexOf(':');
      final field = colon < 0 ? message : message.substring(0, colon);
      var value = colon < 0 ? '' : message.substring(colon + 1);
      if (value.startsWith(' ')) value = value.substring(1);
      switch (field) {
        case 'event':
          event = value;
        case 'id':
          if (!value.contains('\x00')) id = value;
        case 'data':
          data.add(value);
      }
    }
    if (start < chunk.length) line.add(chunk.sublist(start));
  }
}
