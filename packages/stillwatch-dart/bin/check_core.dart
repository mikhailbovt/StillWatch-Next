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
  final client = StillWatchClient.paired(
    enrollment.core,
    seed: enrollment.seed,
    credentials: credentials,
    save: (_) async => refreshes++,
  );
  try {
    check((await client.core()).id == offer.core.id, 'Core ID mismatch');
    final reads = await Future.wait([
      client.snapshot(),
      client.snapshot(),
      client.cameras(),
    ]);
    check(refreshes == 1, 'Refresh was not single-flight');
    final cameras = reads[2] as Page<Camera>;
    check(
      cameras.items.length == 1 && cameras.items.single.id == options['camera'],
      'Camera scope mismatch',
    );
    final cursor = (reads[0] as Snapshot).cursor;
    final change = await client
        .changes(cursor: cursor)
        .first
        .timeout(const Duration(seconds: 30));
    check(
      change.incident?.cameraId == options['camera'],
      'Event scope mismatch',
    );
    stdout.writeln('dart pairing, refresh and scoped SSE verified');
  } finally {
    client.close();
  }
}
