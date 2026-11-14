import 'dart:typed_data';

import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'profiles.dart';

String categoryName(String value) => switch (value) {
  'fire' => 'Fire or smoke',
  'medical' => 'Possible fall',
  _ => 'Incident',
};

String readable(String value) => value.replaceAll('_', ' ');

String shortTime(DateTime date) {
  final local = date.toLocal();
  return '${local.day.toString().padLeft(2, '0')}/${local.month.toString().padLeft(2, '0')}  '
      '${local.hour.toString().padLeft(2, '0')}:${local.minute.toString().padLeft(2, '0')}';
}

Color attentionColor(String attention, ColorScheme colors) =>
    switch (attention) {
      'urgent' => colors.error,
      'review' => colors.tertiary,
      _ => colors.primary,
    };

class IncidentHistory extends StatefulWidget {
  const IncidentHistory({super.key, required this.console});
  final ConsoleState console;

  @override
  State<IncidentHistory> createState() => _IncidentHistoryState();
}

class _IncidentHistoryState extends State<IncidentHistory> {
  bool _openOnly = false;
  bool _loading = false;

  Future<void> _more() async {
    setState(() => _loading = true);
    try {
      await widget.console.loadOlder();
    } catch (error) {
      if (mounted) showFailure(context, error);
    } finally {
      if (mounted) setState(() => _loading = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final console = widget.console;
    final profile = console.selected!;
    final cameraNames = {
      for (final camera in console.cameras) camera.id: camera.name,
    };
    final entries = console.incidents
        .where((item) => !_openOnly || item.lifecycle == 'open')
        .toList();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(20, 14, 20, 10),
          child: Wrap(
            spacing: 8,
            children: [
              ChoiceChip(
                label: const Text('All incidents'),
                selected: !_openOnly,
                onSelected: (_) => setState(() => _openOnly = false),
              ),
              ChoiceChip(
                label: const Text('Open'),
                selected: _openOnly,
                onSelected: (_) => setState(() => _openOnly = true),
              ),
            ],
          ),
        ),
        if (console.hasOlderOpenIncidents && console.historyCursor != null)
          const Padding(
            padding: EdgeInsets.fromLTRB(20, 0, 20, 10),
            child: Text(
              'Load older incidents to see the rest of the open history.',
            ),
          ),
        Expanded(
          child: entries.isEmpty && console.historyCursor == null
              ? const Center(child: Text('No incidents in this view.'))
              : ListView.separated(
                  padding: const EdgeInsets.fromLTRB(20, 0, 20, 20),
                  itemCount:
                      entries.length + (console.historyCursor == null ? 0 : 1),
                  separatorBuilder: (_, _) => const Divider(height: 1),
                  itemBuilder: (context, index) {
                    if (index == entries.length) {
                      return Padding(
                        padding: const EdgeInsets.all(12),
                        child: OutlinedButton(
                          onPressed:
                              _loading || console.link != LinkState.connected
                              ? null
                              : _more,
                          child: Text(
                            _loading ? 'Loading…' : 'Load older incidents',
                          ),
                        ),
                      );
                    }
                    final item = entries[index];
                    final colors = Theme.of(context).colorScheme;
                    final color = attentionColor(item.attention, colors);
                    return ListTile(
                      leading: Icon(
                        item.category == 'fire'
                            ? Icons.local_fire_department_outlined
                            : Icons.accessibility_new,
                        color: color,
                      ),
                      title: Text(categoryName(item.category)),
                      subtitle: Text(
                        '${cameraNames[item.cameraId] ?? 'Camera ${item.cameraId.substring(0, 8)}'} · ${item.lifecycle == 'resolved' ? 'Resolved' : readable(item.attention)}',
                      ),
                      trailing: console.pendingAction(profile, item.id) == null
                          ? Text(shortTime(item.updatedAt))
                          : const Text('Pending action'),
                      onTap: () => Navigator.of(context).push(
                        MaterialPageRoute(
                          builder: (_) => IncidentDetail(
                            console: console,
                            profile: profile,
                            initial: item,
                            generation: console.streamGeneration,
                            cameraName: cameraNames[item.cameraId],
                          ),
                        ),
                      ),
                    );
                  },
                ),
        ),
      ],
    );
  }
}

class IncidentDetail extends StatefulWidget {
  const IncidentDetail({
    super.key,
    required this.console,
    required this.profile,
    required this.initial,
    required this.generation,
    this.cameraName,
  });
  final ConsoleState console;
  final CoreProfile profile;
  final Incident initial;
  final String? generation;
  final String? cameraName;

  @override
  State<IncidentDetail> createState() => _IncidentDetailState();
}

class _IncidentDetailState extends State<IncidentDetail> {
  late Incident _item = widget.initial;
  late String? _streamGeneration = widget.generation;
  Future<Json>? _evidence;
  String? _packet;
  String? _evidenceGeneration;

  bool get _visible => widget.console.canViewCachedIncidents(widget.profile);

  bool get _online =>
      _visible &&
      widget.console.foreground &&
      widget.console.link == LinkState.connected;

  @override
  void initState() {
    super.initState();
    widget.console.addListener(_observe);
    _observe();
  }

  void _observe() {
    final current = widget.console.incidents
        .where((item) => item.id == _item.id)
        .firstOrNull;
    setState(() {
      if (current != null && _visible) {
        _item = current;
        _streamGeneration = widget.console.streamGeneration;
      }
      final packet = _online && widget.profile.can('evidence_read')
          ? _item.packetId
          : null;
      if (_packet != packet ||
          (packet != null && _evidenceGeneration != _streamGeneration)) {
        _packet = packet;
        _evidenceGeneration = _streamGeneration;
        _evidence = packet == null
            ? null
            : widget.profile.client.evidence(_item.id, packet);
      }
    });
  }

  Future<void> _apply(IncidentAction action, Incident item) async {
    final generation = _streamGeneration;
    final needsReason =
        action == IncidentAction.resolve || action == IncidentAction.falseAlarm;
    final reason = needsReason
        ? await showDialog<String>(
            context: context,
            builder: (_) =>
                _ReasonDialog(resolve: action == IncidentAction.resolve),
          )
        : '';
    if (!mounted || reason == null) return;
    try {
      await widget.console.queueIncidentAction(
        widget.profile,
        item,
        generation,
        action,
        reason: reason,
      );
    } catch (error) {
      if (mounted) showFailure(context, error);
    }
  }

  Future<void> _sendPending() async {
    try {
      await widget.console.sendPendingAction(widget.profile, _item.id);
    } catch (error) {
      if (mounted) showFailure(context, error);
    }
  }

  @override
  void dispose() {
    widget.console.removeListener(_observe);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (!_visible) {
      return Scaffold(
        appBar: AppBar(title: const Text('Incident')),
        body: const Center(child: Text('This Core session is unavailable.')),
      );
    }
    final theme = Theme.of(context);
    final item = _item;
    final pending = widget.console.pendingAction(widget.profile, item.id);
    final mayAct =
        widget.console.foreground &&
        pending == null &&
        widget.profile.can('events_ack') &&
        _streamGeneration != null &&
        _streamGeneration == widget.console.streamGeneration &&
        item.lifecycle == 'open';
    final evidence = _evidence;
    return Scaffold(
      appBar: AppBar(title: Text(categoryName(item.category))),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 860),
          child: ListView(
            padding: const EdgeInsets.all(20),
            children: [
              Text(
                widget.cameraName ?? 'Camera ${item.cameraId.substring(0, 8)}',
                style: theme.textTheme.headlineSmall,
              ),
              const SizedBox(height: 4),
              Text(
                '${shortTime(item.updatedAt)} · ${item.live ? 'Live source' : 'Historical'} · Revision ${item.revision}',
              ),
              const SizedBox(height: 14),
              Wrap(
                spacing: 8,
                runSpacing: 6,
                children: [
                  Chip(label: Text(readable(item.lifecycle))),
                  Chip(
                    label: Text(readable(item.attention)),
                    side: BorderSide(
                      color: attentionColor(item.attention, theme.colorScheme),
                    ),
                  ),
                  Chip(
                    label: Text(
                      item.acknowledged ? 'Acknowledged' : 'Unacknowledged',
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 12),
              Text(
                item.needsReassessment
                    ? 'Assessment · Needs reassessment'
                    : 'Assessment · ${readable(item.assessment)}',
              ),
              const SizedBox(height: 4),
              Text(readable(item.reason), style: theme.textTheme.bodySmall),
              if (item.possibleContinuationOf case final previous?)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(
                    'Possible continuation of incident ${previous.substring(0, 8)}',
                    style: theme.textTheme.bodySmall,
                  ),
                ),
              if (!_online)
                const Padding(
                  padding: EdgeInsets.only(top: 12),
                  child: Text('Offline · last synchronized incident.'),
                ),
              if (widget.profile.can('events_ack') &&
                  item.lifecycle == 'open' &&
                  pending == null) ...[
                const SizedBox(height: 18),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    FilledButton(
                      onPressed: mayAct && !item.acknowledged
                          ? () => _apply(IncidentAction.acknowledge, item)
                          : null,
                      child: Text(IncidentAction.acknowledge.label),
                    ),
                    for (final action in const [
                      IncidentAction.silence,
                      IncidentAction.falseAlarm,
                      IncidentAction.resolve,
                    ])
                      OutlinedButton(
                        onPressed:
                            mayAct &&
                                (action != IncidentAction.falseAlarm ||
                                    item.packetId != null)
                            ? () => _apply(action, item)
                            : null,
                        child: Text(action.label),
                      ),
                  ],
                ),
              ],
              if (pending != null) ...[
                const SizedBox(height: 18),
                Text('${pending.action.label} · Pending'),
                if (pending.sending)
                  const LinearProgressIndicator()
                else ...[
                  const Text('Pending actions expire after two minutes.'),
                  Wrap(
                    spacing: 8,
                    children: [
                      FilledButton(
                        onPressed: _online ? _sendPending : null,
                        child: const Text('Verify and send'),
                      ),
                      TextButton(
                        onPressed: () => widget.console.discardIncidentAction(
                          widget.profile,
                          item.id,
                        ),
                        child: const Text('Discard pending action'),
                      ),
                    ],
                  ),
                ],
              ],
              const SizedBox(height: 28),
              Text('Evidence', style: theme.textTheme.titleLarge),
              const SizedBox(height: 8),
              if (evidence == null)
                const Text(
                  'No evidence available for this incident and session.',
                )
              else
                FutureBuilder<Json>(
                  key: ValueKey((_packet, _evidenceGeneration)),
                  future: evidence,
                  builder: (context, snapshot) {
                    if (snapshot.hasError) {
                      return Text(describeFailure(snapshot.error!));
                    }
                    if (!snapshot.hasData) {
                      return const LinearProgressIndicator();
                    }
                    final images = objects(snapshot.data!, 'images');
                    return Wrap(
                      spacing: 10,
                      runSpacing: 10,
                      children: [
                        for (final image in images)
                          _EvidenceImage(
                            key: ValueKey('$_packet:${image['ordinal']}'),
                            client: widget.profile.client,
                            incident: item.id,
                            packet: _packet!,
                            ordinal: image['ordinal'] as int,
                            kind: text(image, 'kind'),
                            available: image['availability'] == 'available',
                          ),
                      ],
                    );
                  },
                ),
            ],
          ),
        ),
      ),
    );
  }
}

class _EvidenceImage extends StatefulWidget {
  const _EvidenceImage({
    super.key,
    required this.client,
    required this.incident,
    required this.packet,
    required this.ordinal,
    required this.kind,
    required this.available,
  });
  final StillWatchClient client;
  final String incident;
  final String packet;
  final int ordinal;
  final String kind;
  final bool available;

  @override
  State<_EvidenceImage> createState() => _EvidenceImageState();
}

class _EvidenceImageState extends State<_EvidenceImage> {
  late final Future<Uint8List>? _bytes = widget.available
      ? widget.client.evidenceImage(
          widget.incident,
          widget.packet,
          widget.ordinal,
        )
      : null;
  MemoryImage? _image;

  @override
  void dispose() {
    final image = _image;
    if (image != null) PaintingBinding.instance.imageCache.evict(image);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final bytes = _bytes;
    return SizedBox(
      width: 238,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          AspectRatio(
            aspectRatio: 4 / 3,
            child: DecoratedBox(
              decoration: BoxDecoration(
                color: Theme.of(context).colorScheme.surfaceContainer,
              ),
              child: bytes == null
                  ? const Center(child: Text('Unavailable'))
                  : FutureBuilder<Uint8List>(
                      future: bytes,
                      builder: (context, snapshot) => snapshot.hasData
                          ? Image(
                              image: _image ??= MemoryImage(snapshot.data!),
                              fit: BoxFit.contain,
                            )
                          : Center(
                              child: snapshot.hasError
                                  ? const Text('Unavailable')
                                  : const CircularProgressIndicator(),
                            ),
                    ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text('${readable(widget.kind)} ${widget.ordinal + 1}'),
          ),
        ],
      ),
    );
  }
}

class _ReasonDialog extends StatefulWidget {
  const _ReasonDialog({required this.resolve});
  final bool resolve;
  @override
  State<_ReasonDialog> createState() => _ReasonDialogState();
}

class _ReasonDialogState extends State<_ReasonDialog> {
  final _reason = TextEditingController();
  @override
  void dispose() {
    _reason.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(widget.resolve ? 'Resolve incident' : 'Record false alarm'),
    content: TextField(
      controller: _reason,
      autofocus: true,
      maxLength: 250,
      decoration: const InputDecoration(labelText: 'Reason'),
      onChanged: (_) => setState(() {}),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: _reason.text.trim().isEmpty
            ? null
            : () => Navigator.of(context).pop(_reason.text.trim()),
        child: Text(widget.resolve ? 'Resolve' : 'Record feedback'),
      ),
    ],
  );
}

void showFailure(BuildContext context, Object error) {
  ScaffoldMessenger.of(context)
      .showSnackBar(SnackBar(content: Text(describeFailure(error))));
}
