import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:stillwatch_sdk/stillwatch_sdk.dart';

void check(bool valid, String failure) {
  if (!valid) throw StateError(failure);
}

Future<void> main(List<String> arguments) async {
  final fixture = object(jsonDecode(await File(arguments[0]).readAsString()));
  for (final candidate in fixture['counter_valid'] as List) {
    check(counter(candidate) == candidate, 'Counter changed');
  }
  for (final candidate in fixture['counter_invalid'] as List) {
    try {
      counter(candidate);
      throw StateError('Accepted invalid counter');
    } on StillWatchException {
      // Expected.
    }
  }
  final options = object(jsonDecode(await File(arguments[1]).readAsString()));
  final offer = PairingOffer.fromJson(object(options['offer']));
  final enrollment = await offer.enroll('SDK QA Dart');
  final credentials = Map<String, Object?>.of(enrollment.credentials)
    ..['access_expires_at'] = DateTime.utc(2001).toIso8601String();
  var refreshes = 0;
  var pendingWrites = 0;
  final client = StillWatchClient.paired(
    enrollment.core,
    seed: enrollment.seed,
    credentials: credentials,
    save: (value) async {
      if (value['pending_refresh'] == null) {
        refreshes++;
      } else {
        pendingWrites++;
      }
    },
  );
  try {
    check((await client.core()).id == offer.core.id, 'Core ID mismatch');
    final reads = await Future.wait([
      client.snapshot(),
      client.snapshot(),
      client.cameras(),
    ]);
    check(refreshes == 1, 'Refresh was not single-flight');
    check(pendingWrites == 1, 'Pending refresh was not persisted');
    final cameras = reads[2] as Page<Camera>;
    check(
      cameras.items.length == 1 && cameras.items.single.id == options['camera'],
      'Camera scope mismatch',
    );
    final health = await client.healthEvents(limit: 1);
    check(
      health.items.single.cameraId == options['camera'] && health.next != null,
      'Camera health scope mismatch',
    );
    final older = await client.healthEvents(before: health.next, limit: 1);
    check(
      older.items.single.cameraId == null && older.next == null,
      'Health page mismatch',
    );
    final cursor = (reads[0] as Snapshot).cursor;
    final markers = options['reconnect_markers'] as String?;
    if (markers != null) {
      await File('$markers/ready-dart').create(exclusive: true);
    }
    final stream = StreamIterator(client.changes(cursor: cursor));
    try {
      check(
        await stream.moveNext().timeout(const Duration(seconds: 30)),
        'No scoped event',
      );
      final change = stream.current;
      check(
        change.incident?.cameraId == options['camera'],
        'Event scope mismatch',
      );
      if (markers != null) {
        check(change.incident?.reason == 'fixture 0', 'Wrong initial event');
        await File('$markers/after-first-dart').create(exclusive: true);
        check(
          await stream.moveNext().timeout(const Duration(seconds: 45)),
          'No event after reconnect',
        );
        final next = stream.current;
        check(
          next.incident?.cameraId == options['camera'] &&
              next.incident?.reason == 'fixture 1000' &&
              next.cursor.generation == change.cursor.generation &&
              BigInt.parse(next.cursor.sequence) >
                  BigInt.parse(change.cursor.sequence),
          'Reconnected cursor or scope mismatch',
        );
        await stream.cancel();

        final otherGeneration = next.cursor.generation.replaceRange(
          0,
          1,
          next.cursor.generation.startsWith('0') ? '1' : '0',
        );
        final expired = StreamIterator(
          client.changes(cursor: Cursor('$otherGeneration:0')),
        );
        try {
          await expired.moveNext().timeout(const Duration(seconds: 5));
          throw StateError('Accepted cursor from another generation');
        } on StillWatchException catch (error) {
          check(
            error.status == 409 && error.code == 'cursor_expired',
            'Wrong cursor expiration problem',
          );
        } finally {
          await expired.cancel();
        }
        final fresh = await client.snapshot();
        check(
          fresh.cursor.toString() == next.cursor.toString() &&
              fresh.incidents.length == 2 &&
              fresh.incidents.every(
                (item) => item.cameraId == options['camera'],
              ),
          'Snapshot did not recover the visible state',
        );
      }
    } finally {
      await stream.cancel();
    }
    stdout.writeln('dart pairing, refresh and scoped SSE verified');
  } finally {
    client.close();
  }
}
