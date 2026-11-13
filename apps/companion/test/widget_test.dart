import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_companion/main.dart';

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
}
