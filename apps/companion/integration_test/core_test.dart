import 'dart:convert';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_webrtc/flutter_webrtc.dart';
import 'package:integration_test/integration_test.dart';
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_companion/main.dart';

const _camera = 'Masked camera';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('pairs, decodes masked video and reloads the saved Core', (
    tester,
  ) async {
    const encodedOffer = String.fromEnvironment('STILLWATCH_QA_OFFER');
    expect(encodedOffer, isNotEmpty, reason: 'Start the native Core fixture.');
    final offer = utf8.decode(base64Decode(encodedOffer));
    await tester.pumpWidget(const StillWatchApp());
    await _waitFor(tester, find.widgetWithText(FilledButton, 'Add a Core'));
    await tester.tap(find.widgetWithText(FilledButton, 'Add a Core'));
    await tester.pumpAndSettle();

    final offerField = find.byWidgetPredicate(
      (widget) =>
          widget is TextField &&
          widget.decoration?.labelText == 'Pairing offer',
    );
    await tester.ensureVisible(offerField);
    await tester.enterText(offerField, offer);
    FocusManager.instance.primaryFocus?.unfocus();
    await tester.pumpAndSettle();
    await tester.tap(find.text('Review offer'));
    await tester.pumpAndSettle();
    expect(find.text('Verify the Core'), findsOneWidget);
    final nameField = find.byType(TextField);
    await tester.ensureVisible(nameField);
    // Submit through the same controller and change callback as native input.
    // This also works when the simulator presents a hardware keyboard.
    final input = tester.widget<TextField>(nameField);
    input.controller!.text = 'Simulator QA';
    input.onChanged!('Simulator QA');
    await tester.pumpAndSettle();
    final trust = find.widgetWithText(FilledButton, 'Trust and pair');
    expect(tester.widget<FilledButton>(trust).onPressed, isNotNull);
    await tester.tap(trust);
    await _waitFor(tester, find.text('Core connected'), diagnose: true);
    await _waitFor(tester, find.text(_camera));
    final open = find.byIcon(Icons.open_in_full);
    await _waitFor(tester, open);
    await tester.tap(open);
    await _waitFor(
      tester,
      find.text('Replay · privacy masked'),
      diagnose: true,
    );

    final view = find.byType(RTCVideoView);
    final track = tester
        .widget<RTCVideoView>(view)
        .videoRenderer
        .srcObject!
        .getVideoTracks()
        .single;
    final first = await _readMaskedFrame(track);
    // Exercise the frame freshness watchdog as well as the first frame callback.
    await Future<void>.delayed(const Duration(seconds: 10));
    await tester.pump();
    expect(view, findsOneWidget);
    expect(find.text('Camera snapshots'), findsNothing);
    final second = await _readMaskedFrame(track);
    expect(second, isNot(orderedEquals(first)));

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(seconds: 1));
    await tester.pumpWidget(const StillWatchApp());
    await _waitFor(tester, find.text('Core connected'));
    await _waitFor(tester, find.text(_camera));
    expect(find.widgetWithText(FilledButton, 'Add a Core'), findsNothing);
    await tester.pumpWidget(const SizedBox.shrink());
  }, timeout: const Timeout(Duration(minutes: 3)));
}

Future<void> _waitFor(
  WidgetTester tester,
  Finder finder, {
  bool diagnose = false,
}) async {
  final watch = Stopwatch()..start();
  while (finder.evaluate().isEmpty &&
      watch.elapsed < const Duration(seconds: 40)) {
    await tester.pump(const Duration(milliseconds: 150));
  }
  final labels = diagnose && finder.evaluate().isEmpty
      ? find.byType(Text).evaluate().map((node) => (node.widget as Text).data)
      : null;
  expect(
    finder,
    findsOneWidget,
    reason: labels?.whereType<String>().join(' | '),
  );
}

Future<List<int>> _readMaskedFrame(MediaStreamTrack track) async {
  final frame = await track.captureFrame().timeout(const Duration(seconds: 8));
  final codec = await ui.instantiateImageCodec(frame.asUint8List());
  try {
    final image = (await codec.getNextFrame()).image;
    try {
      expect((image.width, image.height), (320, 240));
      final data = (await image.toByteData(
        format: ui.ImageByteFormat.rawRgba,
      ))!;
      int highest(int start, int end) {
        var maximum = 0;
        for (var y = 20; y < 220; y += 5) {
          for (var x = start; x < end; x += 4) {
            final index = (y * image.width + x) * 4;
            for (var channel = 0; channel < 3; channel++) {
              final value = data.getUint8(index + channel);
              if (value > maximum) maximum = value;
            }
          }
        }
        return maximum;
      }

      expect(highest(10, 135), lessThan(30));
      expect(highest(190, 310), greaterThan(100));
      return Uint8List.fromList(data.buffer.asUint8List());
    } finally {
      image.dispose();
    }
  } finally {
    codec.dispose();
  }
}
