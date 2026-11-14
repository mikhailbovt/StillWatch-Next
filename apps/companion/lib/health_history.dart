import 'dart:async';

import 'package:material_ui/material_ui.dart' hide Page;
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'incidents.dart';
import 'profiles.dart';

class HealthHistory extends StatefulWidget {
  const HealthHistory({super.key, required this.console});

  final ConsoleState console;

  @override
  State<HealthHistory> createState() => _HealthHistoryState();
}

class _HealthHistoryState extends State<HealthHistory> {
  late final CoreProfile _profile = widget.console.selected!;
  Page<HealthEvent>? _page;
  String? _before;
  bool _activeOnly = false;
  bool _loading = false;
  Object? _error;

  bool get _online =>
      identical(widget.console.selected, _profile) &&
      widget.console.link == LinkState.connected;

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  Future<void> _load({String? before}) async {
    if (_loading || !_online) return;
    setState(() {
      _loading = true;
      _error = null;
      _page = null;
      _before = before;
    });
    try {
      final page = await _profile.client.healthEvents(
        before: before,
        limit: 30,
        activeOnly: _activeOnly,
      );
      if (mounted && _online) setState(() => _page = page);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.console,
    builder: (context, _) {
      final page = _page;
      final cameraNames = {
        for (final camera in widget.console.cameras) camera.id: camera.name,
      };
      return Scaffold(
        appBar: AppBar(
          title: const Text('Core health history'),
          actions: [
            IconButton(
              onPressed: _online && !_loading
                  ? () => _load(before: _before)
                  : null,
              icon: const Icon(Icons.refresh),
              tooltip: 'Refresh health events',
            ),
          ],
        ),
        body: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 840),
            child: ListView(
              padding: const EdgeInsets.all(20),
              children: [
                if (!_online)
                  const Text('Reconnect to view Core health events.')
                else ...[
                  Align(
                    alignment: Alignment.centerLeft,
                    child: SegmentedButton<bool>(
                      segments: const [
                        ButtonSegment(value: false, label: Text('All events')),
                        ButtonSegment(value: true, label: Text('Active')),
                      ],
                      selected: {_activeOnly},
                      onSelectionChanged: _loading
                          ? null
                          : (selection) {
                              if (selection.single == _activeOnly) return;
                              setState(() => _activeOnly = selection.single);
                              unawaited(_load());
                            },
                    ),
                  ),
                  const SizedBox(height: 12),
                  if (_loading)
                    const LinearProgressIndicator()
                  else if (_error case final error?)
                    Text(
                      error is StillWatchException && error.status == 404
                          ? 'Health history requires a newer Core.'
                          : describeFailure(error),
                    )
                  else if (page == null)
                    const Text('Refresh to load health events.')
                  else ...[
                    if (page.items.isEmpty)
                      Text(
                        _before != null
                            ? 'No older events.'
                            : _activeOnly
                            ? 'No active technical events.'
                            : 'No technical events recorded.',
                      ),
                    for (final event in page.items)
                      _EventTile(
                        event,
                        event.cameraId == null
                            ? 'Core'
                            : cameraNames[event.cameraId] ??
                                  'Camera ${event.cameraId!.substring(0, 8)}',
                      ),
                    Wrap(
                      spacing: 12,
                      children: [
                        if (_before != null)
                          TextButton(
                            onPressed: () => _load(),
                            child: const Text('Newest events'),
                          ),
                        if (page.next != null)
                          TextButton(
                            onPressed: () => _load(before: page.next),
                            child: const Text('Older events'),
                          ),
                      ],
                    ),
                  ],
                ],
              ],
            ),
          ),
        ),
      );
    },
  );
}

class _EventTile extends StatelessWidget {
  const _EventTile(this.event, this.source);

  final HealthEvent event;
  final String source;

  @override
  Widget build(BuildContext context) {
    final end = event.endedAt;
    final title = switch (event.code) {
      'camera_offline' => 'Camera offline',
      'camera_frozen' => 'Camera frames frozen',
      'camera_capacity_exceeded' => 'Camera exceeds capture capacity',
      'invalid_source_clock' => 'Camera timestamps stalled or invalid',
      'host_clock_discontinuity' => 'Core system clock changed abruptly',
      'inference_unavailable' => 'Local analysis unavailable',
      'model_pack_unverified' => 'Model pack could not be verified',
      'semantic_unavailable' => 'Semantic worker unavailable',
      'semantic_budget_exhausted' => 'Daily semantic budget exhausted',
      'disk_space_low' => 'Core disk space below 512 MiB',
      'disk_space_unknown' => 'Core disk space could not be checked',
      'database_budget_low' => 'Core database capacity is low',
      _ => 'Technical event',
    };
    return ListTile(
      contentPadding: EdgeInsets.zero,
      leading: Icon(end == null ? Icons.warning_amber : Icons.history),
      title: Text(title),
      subtitle: Text(
        '$source\nStarted ${shortTime(event.openedAt)}'
        '${end == null ? ' · Active' : ' · Ended ${shortTime(end)}'}',
      ),
      isThreeLine: true,
    );
  }
}
