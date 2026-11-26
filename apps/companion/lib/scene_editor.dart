import 'dart:typed_data';

import 'package:flutter/foundation.dart' show listEquals;
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'incidents.dart';

const _regionUnits = 10000;

class SceneRegionEditor extends StatefulWidget {
  const SceneRegionEditor({super.key, this.existing, required this.snapshot});

  final Json? existing;
  final Future<Uint8List> Function() snapshot;

  @override
  State<SceneRegionEditor> createState() => _SceneRegionEditorState();
}

class _SceneRegionEditorState extends State<SceneRegionEditor> {
  late final _points = <(int, int)>[
    if (widget.existing case final region?)
      for (final point in objects(region, 'polygon'))
        (point['x'] as int, point['y'] as int),
  ];
  late String _source = widget.existing?['source'] as String? ?? 'candle';
  late String _mode = widget.existing?['mode'] as String? ?? 'steady';
  late Future<Uint8List> _image = widget.snapshot();
  String? _problem;

  void _add(Offset position, Size size) {
    if (_points.length == 16) return;
    final x = (position.dx / size.width * _regionUnits).round().clamp(
      0,
      _regionUnits,
    );
    final y = (position.dy / size.height * _regionUnits).round().clamp(
      0,
      _regionUnits,
    );
    setState(() {
      _points.add((x, y));
      _problem = null;
    });
  }

  void _apply() {
    if (!_isConvex(_points)) {
      setState(
        () => _problem = 'Choose 3–16 corners in order around a convex area.',
      );
      return;
    }
    Navigator.of(context).pop({
      'source': _source,
      'mode': _mode,
      'polygon': [
        for (final (x, y) in _points) {'x': x, 'y': y},
      ],
    });
  }

  Widget _canvas(Uint8List bytes) => SizedBox(
    height: 242,
    child: LayoutBuilder(
      builder: (context, constraints) {
        final size = constraints.biggest;
        final colors = Theme.of(context).colorScheme;
        return GestureDetector(
          key: const ValueKey('scene-region-canvas'),
          behavior: HitTestBehavior.opaque,
          onTapUp: (details) => _add(details.localPosition, size),
          child: Stack(
            fit: StackFit.expand,
            children: [
              ColoredBox(color: colors.surfaceContainerLowest),
              Image.memory(bytes, fit: BoxFit.fill),
              CustomPaint(
                painter: _PolygonPainter(List.of(_points), colors.primary),
              ),
            ],
          ),
        );
      },
    ),
  );

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Controlled fire region'),
    content: SizedBox(
      width: 520,
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            DropdownButtonFormField<String>(
              initialValue: _source,
              decoration: const InputDecoration(labelText: 'Source'),
              items: [
                for (final value in const [
                  'burner',
                  'candle',
                  'fireplace',
                  'grill',
                ])
                  DropdownMenuItem(value: value, child: Text(readable(value))),
              ],
              onChanged: (value) => setState(() => _source = value ?? _source),
            ),
            const SizedBox(height: 10),
            DropdownButtonFormField<String>(
              initialValue: _mode,
              decoration: const InputDecoration(labelText: 'Expected flame'),
              items: [
                for (final value in const ['steady', 'intermittent'])
                  DropdownMenuItem(value: value, child: Text(readable(value))),
              ],
              onChanged: (value) => setState(() => _mode = value ?? _mode),
            ),
            const SizedBox(height: 12),
            const Text('Tap the corners in order around the expected flame.'),
            const SizedBox(height: 6),
            FutureBuilder<Uint8List>(
              future: _image,
              builder: (context, result) {
                final loading =
                    result.connectionState == ConnectionState.waiting;
                return Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (loading) const LinearProgressIndicator(),
                    if (result.hasData && !loading)
                      _canvas(result.data!)
                    else if (!loading)
                      const Text(
                        'Camera snapshot unavailable. Refresh to draw on the current frame.',
                      ),
                  ],
                );
              },
            ),
            Wrap(
              spacing: 8,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                Text('${_points.length}/16 corners'),
                TextButton(
                  onPressed: _points.isEmpty
                      ? null
                      : () => setState(() {
                          _points.removeLast();
                          _problem = null;
                        }),
                  child: const Text('Undo'),
                ),
                TextButton(
                  onPressed: _points.isEmpty
                      ? null
                      : () => setState(() {
                          _points.clear();
                          _problem = null;
                        }),
                  child: const Text('Clear'),
                ),
                TextButton(
                  onPressed: () => setState(() => _image = widget.snapshot()),
                  child: const Text('Refresh snapshot'),
                ),
              ],
            ),
            if (_problem case final problem?)
              Text(
                problem,
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: _points.length < 3 ? null : _apply,
        child: const Text('Apply'),
      ),
    ],
  );
}

bool _isConvex(List<(int, int)> polygon) {
  if (polygon.length < 3 || polygon.length > 16) return false;
  int cross((int, int) a, (int, int) b, (int, int) c) =>
      ((b.$1 - a.$1) * (c.$2 - a.$2) - (b.$2 - a.$2) * (c.$1 - a.$1)).sign;
  final orientation = cross(polygon[0], polygon[1], polygon[2]);
  if (orientation == 0) return false;
  for (var i = 0; i < polygon.length; i++) {
    final j = (i + 1) % polygon.length;
    for (var k = 0; k < polygon.length; k++) {
      if (k != i &&
          k != j &&
          cross(polygon[i], polygon[j], polygon[k]) != orientation) {
        return false;
      }
    }
  }
  return true;
}

final class _PolygonPainter extends CustomPainter {
  _PolygonPainter(this.points, this.color);

  final List<(int, int)> points;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    if (points.isEmpty) return;
    final offsets = [
      for (final (x, y) in points)
        Offset(x * size.width / _regionUnits, y * size.height / _regionUnits),
    ];
    final path = Path()..addPolygon(offsets, points.length >= 3);
    if (points.length >= 3) {
      canvas.drawPath(path, Paint()..color = color.withValues(alpha: .22));
    }
    final stroke = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2;
    canvas.drawPath(path, stroke);
    final dot = Paint()..color = color;
    for (final offset in offsets) {
      canvas.drawCircle(offset, 4, dot);
    }
  }

  @override
  bool shouldRepaint(_PolygonPainter oldDelegate) =>
      color != oldDelegate.color || !listEquals(points, oldDelegate.points);
}
