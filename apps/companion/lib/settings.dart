import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:material_ui/material_ui.dart';
import 'package:qr_flutter/qr_flutter.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'alerts.dart';
import 'incidents.dart';
import 'profiles.dart';

class CoreSettings extends StatefulWidget {
  const CoreSettings({super.key, required this.console, required this.alerts});
  final ConsoleState console;
  final LocalAlerts alerts;

  @override
  State<CoreSettings> createState() => _CoreSettingsState();
}

class _CoreSettingsState extends State<CoreSettings> {
  Future<Json>? _semantic;
  Future<List<Json>>? _devices;
  bool _loading = false;

  @override
  void initState() {
    super.initState();
    if (widget.console.link == LinkState.connected) _refresh();
  }

  void _refresh() {
    final profile = widget.console.selected;
    if (profile == null || widget.console.link != LinkState.connected) return;
    setState(() {
      _semantic = profile.client.semanticStatus();
      _devices = profile.role == 'owner' ? profile.client.devices() : null;
    });
  }

  Future<void> _forget(CoreProfile profile) async {
    final yes = await _confirm(
      'Forget this Core?',
      'The credentials on this device will be removed. The Core owner can revoke any other paired devices.',
      'Forget',
    );
    if (!mounted || !yes) return;
    try {
      await widget.console.forget(profile);
    } catch (error) {
      if (mounted) showFailure(context, error);
    }
  }

  Future<bool> _confirm(String title, String content, String action) async =>
      await showDialog<bool>(
        context: context,
        builder: (dialog) => AlertDialog(
          title: Text(title),
          content: Text(content),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(dialog).pop(false),
              child: const Text('Cancel'),
            ),
            FilledButton(
              onPressed: () => Navigator.of(dialog).pop(true),
              child: Text(action),
            ),
          ],
        ),
      ) ==
      true;

  Future<void> _revoke(CoreProfile profile, Json device) async {
    final yes = await _confirm(
      'Revoke access?',
      '${device['label']} will lose access and active streams.',
      'Revoke',
    );
    if (!mounted || !yes || _loading) return;
    setState(() => _loading = true);
    try {
      await profile.client.revokeDevice(uuid(device['id']));
      if (mounted) _refresh();
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
    final online = console.link == LinkState.connected;
    final theme = Theme.of(context);
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 840),
        child: ListView(
          padding: const EdgeInsets.all(20),
          children: [
            Text('Core connection', style: theme.textTheme.titleLarge),
            ListTile(
              contentPadding: EdgeInsets.zero,
              leading: const Icon(Icons.dns_outlined),
              title: Text(profile.name),
              subtitle: Text(profile.core.origin.origin),
              trailing: OutlinedButton(
                onPressed: console.reconnect,
                child: const Text('Reconnect'),
              ),
            ),
            ListTile(
              contentPadding: EdgeInsets.zero,
              leading: const Icon(Icons.shield_outlined),
              title: Text('Access · ${profile.role}'),
              subtitle: SelectableText(
                profile.core.pin,
                style: const TextStyle(fontFamily: 'monospace'),
              ),
            ),
            if (console.lastSync != null)
              Text('Last synchronized · ${shortTime(console.lastSync!)}'),
            const SizedBox(height: 18),
            const Divider(),
            Row(
              children: [
                Expanded(
                  child: Text(
                    'Semantic analysis',
                    style: theme.textTheme.titleLarge,
                  ),
                ),
                IconButton(
                  onPressed: online ? _refresh : null,
                  icon: const Icon(Icons.refresh),
                  tooltip: 'Refresh Core status',
                ),
              ],
            ),
            if (!online)
              const Text('Status unavailable while offline.')
            else if (_semantic == null)
              const Text('Refresh to check the configured provider.')
            else
              FutureBuilder<Json>(
                future: _semantic,
                builder: (context, state) {
                  if (state.hasError) {
                    return Text(describeFailure(state.error!));
                  }
                  if (!state.hasData) return const LinearProgressIndicator();
                  final value = state.data!;
                  return Text(
                    [
                      readable(text(value, 'state')),
                      if (value['last_inference_source'] is String)
                        readable(value['last_inference_source'] as String),
                      if (value['cloud_model'] is String)
                        value['cloud_model'] as String,
                    ].join(' · '),
                  );
                },
              ),
            if (profile.role == 'owner') ...[
              const SizedBox(height: 18),
              const Divider(),
              Wrap(
                alignment: WrapAlignment.spaceBetween,
                crossAxisAlignment: WrapCrossAlignment.center,
                spacing: 8,
                runSpacing: 8,
                children: [
                  Text('Paired devices', style: theme.textTheme.titleLarge),
                  OutlinedButton.icon(
                    onPressed: online
                        ? () => showDialog<void>(
                            context: context,
                            builder: (_) =>
                                _IssueOffer(profile, console.cameras),
                          )
                        : null,
                    icon: const Icon(Icons.qr_code),
                    label: const Text('Pair device'),
                  ),
                ],
              ),
              if (online && _devices != null)
                FutureBuilder<List<Json>>(
                  future: _devices,
                  builder: (context, snapshot) {
                    if (snapshot.hasError) {
                      return Text(describeFailure(snapshot.error!));
                    }
                    if (!snapshot.hasData) {
                      return const LinearProgressIndicator();
                    }
                    return Column(
                      children: [
                        for (final device in snapshot.data!)
                          ListTile(
                            contentPadding: EdgeInsets.zero,
                            title: Text(text(device, 'label')),
                            subtitle: Text(
                              '${device['role']}${device['revoked_at'] != null ? ' · revoked' : ''}${device['id'] == profile.deviceId ? ' · this device' : ''}',
                            ),
                            trailing:
                                device['revoked_at'] == null &&
                                    device['id'] != profile.deviceId
                                ? TextButton(
                                    onPressed: _loading
                                        ? null
                                        : () => _revoke(profile, device),
                                    child: const Text('Revoke'),
                                  )
                                : null,
                          ),
                      ],
                    );
                  },
                ),
            ],
            const SizedBox(height: 18),
            const Divider(),
            ListenableBuilder(
              listenable: widget.alerts,
              builder: (context, _) => SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: const Text('Foreground notifications'),
                subtitle: Text(
                  !widget.alerts.available
                      ? 'The system notification service is unavailable.'
                      : widget.alerts.enabled && !widget.alerts.allowedBySystem
                      ? 'System settings have blocked StillWatch notifications.'
                      : 'Show system notifications for urgent incidents while StillWatch is open.',
                ),
                value: widget.alerts.enabled,
                onChanged: widget.alerts.available
                    ? (value) async {
                        try {
                          await widget.alerts.setEnabled(value);
                        } catch (error) {
                          if (context.mounted) showFailure(context, error);
                        }
                      }
                    : null,
              ),
            ),
            if (Platform.isWindows || Platform.isLinux)
              ListTile(
                contentPadding: EdgeInsets.zero,
                title: const Text('Desktop alarm agent'),
                subtitle: const Text(
                  'Enable the agent separately to play urgent alerts while this window is closed. Use your session settings to stop it or disable automatic start.',
                ),
                onTap: () => showDialog<void>(
                  context: context,
                  builder: (dialog) => AlertDialog(
                    title: const Text('Desktop alarm agent'),
                    content: Text(
                      Platform.isWindows
                          ? 'See the Desktop alarm agent section of the operations guide to pair and enable the agent. In Task Scheduler, end the StillWatch alarm agent task to stop it now, or disable it to prevent the next sign-in from starting it.'
                          : 'See the Desktop alarm agent section of the operations guide to pair and enable the agent. Run systemctl --user stop stillwatch-alarm-agent to stop it now, or systemctl --user disable --now stillwatch-alarm-agent to also turn off automatic start.',
                    ),
                    actions: [
                      TextButton(
                        onPressed: () => Navigator.of(dialog).pop(),
                        child: const Text('Done'),
                      ),
                    ],
                  ),
                ),
              ),
            const Divider(),
            Align(
              alignment: Alignment.centerLeft,
              child: TextButton(
                onPressed: () => _forget(profile),
                child: const Text('Forget Core on this device'),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _IssueOffer extends StatefulWidget {
  const _IssueOffer(this.profile, this.cameras);
  final CoreProfile profile;
  final List<Camera> cameras;
  @override
  State<_IssueOffer> createState() => _IssueOfferState();
}

class _IssueOfferState extends State<_IssueOffer> {
  String _role = 'viewer';
  final _cameras = <String>{};
  String? _offer;
  String? _error;
  bool _busy = false;

  Future<void> _issue() async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final result = await widget.profile.client.issuePairing(
        role: _role,
        cameras: _role == 'owner' ? const [] : _cameras.toList(),
        scopes: _role == 'owner'
            ? const []
            : [
                'streams_read',
                'evidence_read',
                if (_role == 'operator') 'events_ack',
              ],
      );
      if (mounted) setState(() => _offer = jsonEncode(result));
    } catch (error) {
      if (mounted) setState(() => _error = describeFailure(error));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Pair another device'),
    content: SizedBox(
      width: 440,
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (_offer case final offer?) ...[
              const Text(
                'Scan or copy this one-time offer on the device you are pairing. It expires after five minutes.',
              ),
              const SizedBox(height: 12),
              Center(
                child: ColoredBox(
                  color: Colors.white,
                  child: QrImageView(data: offer, size: 245),
                ),
              ),
              const SizedBox(height: 12),
              ExpansionTile(
                title: const Text('Pairing offer JSON'),
                children: [SelectableText(offer)],
              ),
            ] else ...[
              DropdownButtonFormField<String>(
                initialValue: _role,
                decoration: const InputDecoration(labelText: 'Role'),
                items: const [
                  DropdownMenuItem(value: 'viewer', child: Text('Viewer')),
                  DropdownMenuItem(value: 'operator', child: Text('Operator')),
                  DropdownMenuItem(value: 'owner', child: Text('Owner')),
                ],
                onChanged: _busy
                    ? null
                    : (value) => setState(() => _role = value ?? 'viewer'),
              ),
              const SizedBox(height: 10),
              if (_role == 'owner')
                const Text('An owner can manage devices and view all cameras.')
              else
                for (final camera in widget.cameras)
                  CheckboxListTile(
                    contentPadding: EdgeInsets.zero,
                    title: Text(camera.name),
                    value: _cameras.contains(camera.id),
                    onChanged: _busy
                        ? null
                        : (value) => setState(() {
                            if (value == true) {
                              _cameras.add(camera.id);
                            } else {
                              _cameras.remove(camera.id);
                            }
                          }),
                  ),
            ],
            if (_error != null) Text(_error!),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: _busy ? null : () => Navigator.of(context).pop(),
        child: Text(_offer == null ? 'Cancel' : 'Done'),
      ),
      if (_offer == null)
        FilledButton(
          onPressed: _busy || (_role != 'owner' && _cameras.isEmpty)
              ? null
              : _issue,
          child: const Text('Generate offer'),
        ),
    ],
  );
}
