import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_companion/main.dart';
import 'package:stillwatch_companion/scene_editor.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

void main() {
  testWidgets('offers pairing on first launch', (tester) async {
    FlutterSecureStorage.setMockInitialValues({});
    await tester.pumpWidget(const StillWatchApp());
    await tester.pumpAndSettle();
    expect(find.text('StillWatch'), findsOneWidget);
    await tester.tap(find.widgetWithText(FilledButton, 'Add a Core'));
    await tester.pumpAndSettle();
    expect(find.text('Review offer'), findsOneWidget);
    expect(find.text('Find nearby'), findsOneWidget);
  });

  testWidgets('draws an ordered convex region in Core coordinates', (
    tester,
  ) async {
    Json? saved;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: TextButton(
              onPressed: () async => saved = await showDialog<Json>(
                context: context,
                builder: (_) => SceneRegionEditor(
                  snapshot: () async => base64Decode(
                    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/b1sAAAAASUVORK5CYII=',
                  ),
                ),
              ),
              child: const Text('Edit region'),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.text('Edit region'));
    await tester.pumpAndSettle();
    final area = tester.getRect(
      find.byKey(const ValueKey('scene-region-canvas')),
    );
    for (final (x, y) in const [
      (.25, .25),
      (.75, .75),
      (.25, .75),
      (.75, .25),
    ]) {
      await tester.tapAt(
        area.topLeft + Offset(area.width * x, area.height * y),
      );
    }
    await tester.pumpAndSettle();
    await tester.tap(find.text('Apply'));
    await tester.pumpAndSettle();
    expect(saved, isNull);
    expect(find.textContaining('convex area'), findsOneWidget);
    await tester.ensureVisible(find.text('Undo'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Undo'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Apply'));
    await tester.pumpAndSettle();
    expect(saved, {
      'source': 'candle',
      'mode': 'steady',
      'polygon': [
        {'x': 2500, 'y': 2500},
        {'x': 7500, 'y': 7500},
        {'x': 2500, 'y': 7500},
      ],
    });
  });
}
