import 'dart:convert';
import 'dart:io';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_companion/cameras.dart';
import 'package:stillwatch_companion/incidents.dart';
import 'package:stillwatch_companion/profiles.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

const _coreId = '8ea319b0-7752-4894-acfe-3f09cf4385c3';
const _generation = '4cbadcf4-4f81-4729-a1a0-af03e4da8817';
const _incidentId = 'ff99e874-0710-40fc-8ea3-bac894af062c';
const _deviceId = '886fbeab-5fe2-44bb-9ed3-2254a23b092b';
const _cameraId = 'bda823d1-2a02-413f-9d91-4138a3432ff1';
const _jpeg =
    '/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkS'
    'Ew8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJ'
    'CQwLDBgNDRgyIRwhMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIy'
    'MjIyMjIyMjIyMjIyMjL/wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAA'
    'AAAAAAAAAAf/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFAEBAAAAAAAAAAAAAAAA'
    'AAAABf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AJ2AUHP/2Q==';
const _camera = <String, Object?>{
  'id': _cameraId,
  'revision': '1',
  'source_epoch': '3',
  'scene_revision': '1',
  'live': true,
  'scene_review': 'unconfigured',
  'config': {'name': 'Frame health QA', 'enabled': true},
  'runtime': {'state': 'streaming', 'frame': '16', 'dropped': '0'},
};
const _incident = <String, Object?>{
  'id': _incidentId,
  'camera_id': _cameraId,
  'revision': '4',
  'evidence_version': '2',
  'evidence_packet_id': '5907fd70-fe2d-4413-bb42-55d1281ed822',
  'category': 'fire',
  'detection_reason': 'visible_smoke',
  'assessment': 'concern',
  'attention': 'review',
  'attention_cycle': '1',
  'lifecycle': 'open',
  'updated_at': '2026-01-01T12:00:00.000Z',
};

void main() {
  test('keeps the command and key across an uncertain send', () async {
    final core = await _CoreFixture.start();
    addTearDown(core.close);
    final console = core.console;
    final profile = core.profile;
    final reviewed = console.incidents.single;
    core.failWrites = true;
    await expectLater(
      console.queueIncidentAction(
        profile,
        reviewed,
        console.streamGeneration,
        IncidentAction.falseAlarm,
        reason: 'Steam from the kettle',
      ),
      throwsA(isA<StillWatchException>()),
    );
    expect(console.pendingAction(profile, reviewed.id)?.sending, isFalse);
    expect(console.incidents.single.revision, '4');
    expect(core.writes, isNotEmpty);
    final first = core.writes.first;
    core.failWrites = false;
    await console.sendPendingAction(profile, reviewed.id);
    expect(core.writes.last, first);
    expect(first.revision, '"4"');
    expect(uuid(first.key), first.key);
    expect(first.path, '/api/v2/incidents/$_incidentId/feedback');
    expect(jsonDecode(first.body), {
      'classification': 'false_alarm',
      'reason': 'Steam from the kettle',
      'evidence_version': '2',
    });
    expect(console.pendingAction(profile, reviewed.id), isNull);
    expect(console.incidents.single.revision, '5');
  });

  test(
    'reconnect waits for explicit review and rejects a stale action',
    () async {
      final core = await _CoreFixture.start();
      addTearDown(core.close);
      final console = core.console;
      final profile = core.profile;
      final reviewed = console.incidents.single;
      console.link = LinkState.offline;
      await console.queueIncidentAction(
        profile,
        reviewed,
        console.streamGeneration,
        IncidentAction.resolve,
        reason: 'Checked the room',
      );
      expect(
        console.pendingAction(profile, reviewed.id)?.action,
        IncidentAction.resolve,
      );
      expect(console.incidents.single.lifecycle, 'open');
      expect(core.writes, isEmpty);

      core.incident = {..._incident, 'revision': '5', 'evidence_version': '3'};
      await core.reconnect();
      expect(console.link, LinkState.connected);
      expect(core.writes, isEmpty);
      await expectLater(
        console.sendPendingAction(profile, reviewed.id),
        throwsA(
          isA<StillWatchException>().having(
            (error) => error.code,
            'code',
            'revision_conflict',
          ),
        ),
      );
      expect(core.writes, isEmpty);
      expect(console.pendingAction(profile, reviewed.id), isNull);
      expect(console.incidents.single.revision, '5');
    },
  );

  test('can verify and acknowledge an older open incident', () async {
    final core = await _CoreFixture.start();
    addTearDown(core.close);
    core.omitFromSnapshot = true;
    await core.reconnect();
    final console = core.console;
    expect(console.hasOlderOpenIncidents, isTrue);
    final reviewed = console.incidents.single;
    await console.queueIncidentAction(
      core.profile,
      reviewed,
      console.streamGeneration,
      IncidentAction.acknowledge,
    );
    expect(
      core.writes.single.path,
      '/api/v2/incidents/$_incidentId/acknowledgements',
    );
    expect(core.writes.single.revision, '"4"');
  });

  test('rejects commands reviewed before restoration and clears forgotten profiles', () async {
    FlutterSecureStorage.setMockInitialValues({});
    final core = await _CoreFixture.start();
    addTearDown(core.close);
    final console = core.console;
    final profile = core.profile;
    final reviewed = console.incidents.single;
    console.link = LinkState.offline;
    await console.queueIncidentAction(
      profile,
      reviewed,
      console.streamGeneration,
      IncidentAction.silence,
    );
    await expectLater(
      console.queueIncidentAction(
        profile,
        reviewed,
        console.streamGeneration,
        IncidentAction.acknowledge,
      ),
      throwsA(
        isA<StillWatchException>().having(
          (error) => error.code,
          'code',
          'incident_action_pending',
        ),
      ),
    );
    core.generation = 'bc82ed43-80ce-4c76-90fe-419581f206b2';
    await core.reconnect();
    await expectLater(
      console.sendPendingAction(profile, reviewed.id),
      throwsA(
        isA<StillWatchException>().having(
          (error) => error.code,
          'code',
          'revision_conflict',
        ),
      ),
    );
    expect(core.writes, isEmpty);
    console.link = LinkState.offline;
    await console.queueIncidentAction(
      profile,
      reviewed,
      console.streamGeneration,
      IncidentAction.silence,
    );
    await console.forget(profile);
    expect(console.pendingAction(profile, reviewed.id), isNull);
  });

  testWidgets(
    'an offline action stays pending and expires without acknowledgement',
    (tester) async {
      final core = (await tester.runAsync(_CoreFixture.start))!;
      final console = core.console;
      final profile = core.profile;
      console.link = LinkState.offline;
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: ListenableBuilder(
                listenable: console,
                builder: (_, _) => IncidentHistory(console: console),
              ),
            ),
          ),
        );
        await tester.tap(find.text('Fire or smoke'));
        await tester.pumpAndSettle();
        await tester.tap(find.text('Acknowledge'));
        await tester.pumpAndSettle();
        expect(find.text('Acknowledge · Pending'), findsOneWidget);
        expect(find.text('Unacknowledged'), findsOneWidget);
        final send = find.widgetWithText(FilledButton, 'Verify and send');
        expect(tester.widget<FilledButton>(send).onPressed, isNull);
        expect(console.pendingAction(profile, _incidentId), isNotNull);
        await tester.pageBack();
        await tester.pumpAndSettle();
        expect(find.text('Pending action'), findsOneWidget);
        await tester.pump(PendingIncidentAction.lifetime);
        expect(console.pendingAction(profile, _incidentId), isNull);
        expect(find.text('Pending action'), findsNothing);
        expect(core.writes, isEmpty);
      } finally {
        await tester.pumpWidget(const SizedBox.shrink());
        await tester.runAsync(core.close);
      }
    },
  );

  testWidgets('expires a preview and reports repeated camera pixels', (
    tester,
  ) async {
    final core = (await tester.runAsync(_CoreFixture.start))!;
    final console = core.console;
    final tree = MaterialApp(
      home: Scaffold(body: CameraGrid(console: console)),
    );
    try {
      console.cameras = [Camera.fromJson(_camera)];
      await tester.pumpWidget(tree);
      for (var i = 0; i < 30 && find.byType(Image).evaluate().isEmpty; i++) {
        await tester.runAsync(
          () => Future<void>.delayed(const Duration(milliseconds: 25)),
        );
        await tester.pump();
      }
      expect(find.byType(Image), findsOneWidget);
      await tester.pump(const Duration(seconds: 2));
      expect(find.byType(Image), findsNothing);
      expect(find.text('Repeated camera frame'), findsNothing);
      console.cameras = [
        Camera.fromJson({
          ..._camera,
          'runtime': {'state': 'unavailable', 'repeated_frame': true},
        }),
      ];
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(body: CameraGrid(console: console)),
        ),
      );
      expect(find.text('Repeated camera frame'), findsOneWidget);
      expect(find.byType(Image), findsNothing);
    } finally {
      await tester.pumpWidget(const SizedBox.shrink());
      await tester.runAsync(core.close);
    }
  });
}

final class _CoreFixture {
  _CoreFixture(this.server);
  final HttpServer server;
  final console = ConsoleState(const ProfileStore(FlutterSecureStorage()));
  late final CoreProfile profile;
  String generation = _generation;
  Json incident = _incident;
  bool failWrites = false;
  bool omitFromSnapshot = false;
  final writes = <({String path, String key, String revision, String body})>[];

  static Future<_CoreFixture> start() =>
      HttpOverrides.runWithHttpOverrides(_start, _LoopbackHttp());

  static Future<_CoreFixture> _start() async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final fixture = _CoreFixture(server);
    final origin = '127.0.0.1:${server.port}';
    fixture.profile = CoreProfile(
      'QA Core',
      PinnedCore.fromJson({
        'endpoint': 'https://$origin',
        'core_instance_id': _coreId,
        'tls_certificate_der': 'AA',
        'tls_certificate_sha256':
            '6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d',
      }),
      '',
      {
        'device': {'id': _deviceId, 'role': 'owner'},
      },
      (_, _) async {},
      client: StillWatchClient.loopback('http://$origin', 'a' * 64),
    );
    server.listen(fixture.handle);
    fixture.console.profiles = [fixture.profile];
    await fixture.console.select(fixture.profile);
    expect(
      fixture.console.link,
      LinkState.connected,
      reason: fixture.console.error,
    );
    return fixture;
  }

  Future<void> handle(HttpRequest request) async {
    final response = request.response;
    final path = request.uri.path;
    response.headers.contentType = ContentType.json;
    if (request.method == 'POST') {
      writes.add((
        path: path,
        key: request.headers.value('idempotency-key')!,
        revision: request.headers.value('if-match')!,
        body: await utf8.decoder.bind(request).join(),
      ));
      if (failWrites) {
        response.statusCode = 503;
        response.headers.set('Retry-After', '30');
        response.write(jsonEncode({'status': 503, 'code': 'busy'}));
      } else {
        incident = {...incident, 'revision': '5'};
        response.write(jsonEncode(incident));
      }
    } else if (path.endsWith('/changes')) {
      response.headers.contentType = ContentType('text', 'event-stream');
      response.write(': ready\n\n');
      await response.flush();
      return;
    } else if (path == '/api/v2/cameras/$_cameraId/snapshot.jpg') {
      response.headers.contentType = ContentType('image', 'jpeg');
      response.headers.set('X-Frame-Age-Ms', '3000');
      response.add(base64Decode(_jpeg));
    } else {
      final value = switch (path) {
        '/bootstrap/info' => {
          'instance_id': _coreId,
          'version': {'major': 2, 'minor': 0},
        },
        '/api/v2/snapshot' => {
          'cursor': '$generation:1',
          'incidents': omitFromSnapshot ? [] : [incident],
          if (omitFromSnapshot) 'has_older_open_incidents': true,
        },
        '/api/v2/incidents' => {
          'incidents': [incident],
        },
        '/api/v2/incidents/$_incidentId' => incident,
        '/api/v2/cameras' => {'cameras': []},
        '/health/ready' => {'state': 'ready'},
        _ => null,
      };
      if (value == null) response.statusCode = 404;
      response.write(jsonEncode(value));
    }
    await response.close();
  }

  Future<void> reconnect() =>
      HttpOverrides.runWithHttpOverrides(console.reconnect, _LoopbackHttp());

  Future<void> close() async {
    console.dispose();
    profile.client.close();
    await server.close(force: true);
  }
}

final class _LoopbackHttp extends HttpOverrides {}
