import 'dart:async';
import 'dart:convert';

import 'package:file_selector/file_selector.dart';
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'incidents.dart';
import 'live_view.dart';
import 'onvif_picker.dart';
import 'profiles.dart';
import 'scene_editor.dart';

class CameraGrid extends StatelessWidget {
  const CameraGrid({super.key, required this.console});
  final ConsoleState console;

  @override
  Widget build(BuildContext context) {
    final profile = console.selected!;
    final online = console.link == LinkState.connected;
    if (console.cameras.isEmpty) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Icon(Icons.videocam_outlined, size: 42),
            const SizedBox(height: 10),
            Text(
              online
                  ? 'No cameras are available to this device.'
                  : 'Waiting for camera information.',
            ),
            if (online && profile.role == 'owner')
              TextButton(
                onPressed: () => editCamera(context, console),
                child: const Text('Add camera'),
              ),
          ],
        ),
      );
    }
    return LayoutBuilder(
      builder: (context, constraints) {
        final columns = (constraints.maxWidth / 300).floor().clamp(1, 4);
        return GridView.builder(
          padding: const EdgeInsets.all(20),
          gridDelegate: SliverGridDelegateWithFixedCrossAxisCount(
            crossAxisCount: columns,
            childAspectRatio: 1.29,
            crossAxisSpacing: 12,
            mainAxisSpacing: 12,
          ),
          itemCount: console.cameras.length,
          itemBuilder: (context, index) {
            final camera = console.cameras[index];
            final running =
                camera.state == 'streaming' || camera.state == 'replaying';
            final canView =
                online &&
                console.foreground &&
                running &&
                profile.can('streams_read');
            final status = !online
                ? 'Core offline'
                : camera.runtime['repeated_frame'] == true
                ? 'Repeated camera frame'
                : readable(camera.state);
            return Card(
              clipBehavior: Clip.antiAlias,
              key: ValueKey(camera.id),
              margin: EdgeInsets.zero,
              child: Column(
                children: [
                  Expanded(
                    child: InkWell(
                      onTap: canView
                          ? () => Navigator.of(context).push(
                              MaterialPageRoute(
                                builder: (_) => LiveView(
                                  console: console,
                                  profile: profile,
                                  initial: camera,
                                ),
                              ),
                            )
                          : null,
                      child: Stack(
                        fit: StackFit.expand,
                        children: [
                          CameraPreview(
                            profile: profile,
                            camera: camera,
                            active: canView,
                          ),
                          if (canView)
                            const Align(
                              alignment: Alignment.bottomRight,
                              child: Padding(
                                padding: EdgeInsets.all(10),
                                child: Icon(
                                  Icons.open_in_full,
                                  color: Colors.white,
                                ),
                              ),
                            ),
                        ],
                      ),
                    ),
                  ),
                  ListTile(
                    dense: true,
                    title: Text(camera.name, overflow: TextOverflow.ellipsis),
                    subtitle: Text(
                      running && online
                          ? '${camera.live ? 'Live source' : 'Replay'} · preview'
                          : status,
                    ),
                    trailing: profile.role == 'owner'
                        ? IconButton(
                            icon: const Icon(Icons.tune),
                            tooltip: 'Camera settings',
                            onPressed: online
                                ? () => editCamera(context, console, camera)
                                : null,
                          )
                        : null,
                  ),
                ],
              ),
            );
          },
        );
      },
    );
  }
}

Future<void> editCamera(
  BuildContext context,
  ConsoleState console, [
  Camera? camera,
]) => showDialog<void>(
  context: context,
  builder: (_) => _CameraEditor(console, console.selected!, camera),
);

class _CameraEditor extends StatefulWidget {
  const _CameraEditor(this.console, this.profile, this.camera);
  final ConsoleState console;
  final CoreProfile profile;
  final Camera? camera;

  @override
  State<_CameraEditor> createState() => _CameraEditorState();
}

class _CameraEditorState extends State<_CameraEditor> {
  late final Json _initial = widget.camera?.configuration ?? {};
  late final Json _initialSource = _initial['source'] == null
      ? {}
      : object(_initial['source']);
  late final TextEditingController _name = TextEditingController(
    text: _initial['name'] as String?,
  );
  late final TextEditingController _address = TextEditingController(
    text: _initialSource['peer'] as String?,
  );
  late final TextEditingController _path = TextEditingController(
    text:
        (_initialSource['stream_path'] ??
                _initialSource['file'] ??
                _initialSource['device_id'])
            as String?,
  );
  late final TextEditingController _reference = TextEditingController(
    text: _initialSource['credential_ref'] as String?,
  );
  late final _regions = <Json>[
    if (_initial['controlled_fire_regions'] is List)
      ...objects(_initial, 'controlled_fire_regions'),
  ];
  late final _masks = <Json>[
    if (_initial['privacy_masks'] is List)
      ...objects(
        _initial,
        'privacy_masks',
      ).map((item) => Map<String, Object?>.of(item)),
  ];
  late String _type = _initialSource['type'] as String? ?? 'rtsp';
  late bool _enabled = _initial['enabled'] == true;
  late bool _repeat = _initialSource['repeat'] == true;
  late bool _secure = _initialSource['secure'] == true;
  late bool _cloudConsent = _initial['cloud_consent'] == true;
  late String _transport = _initialSource['transport'] as String? ?? 'tcp';
  bool _busy = false;
  String? _status;
  bool _error = false;
  bool _recheck = false;

  Future<void> _addMask([int? index]) async {
    final mask = await showDialog<Json>(
      context: context,
      builder: (_) => _MaskDialog(index == null ? null : _masks[index]),
    );
    if (!mounted || mask == null) return;
    setState(() {
      if (index == null) {
        _masks.add(mask);
      } else {
        _masks[index] = mask;
      }
    });
  }

  Future<void> _editRegion([int? index]) async {
    final camera = widget.camera;
    if (camera == null || !camera.enabled) {
      setState(() {
        _error = true;
        _status = 'Enable and save the camera before drawing regions.';
      });
      return;
    }
    final region = await showDialog<Json>(
      context: context,
      builder: (_) => SceneRegionEditor(
        existing: index == null ? null : _regions[index],
        snapshot: () => widget.profile.client.cameraImage(camera.id),
      ),
    );
    if (!mounted || region == null) return;
    setState(() {
      if (index == null) {
        _regions.add(region);
      } else {
        _regions[index] = region;
      }
    });
  }

  Json _source() {
    final path = _path.text.trim();
    if (path.isEmpty) throw const FormatException();
    return switch (_type) {
      'replay' => {
        'type': 'replay',
        'file': path,
        'pace': _repeat || _initialSource['pace'] != false,
        if (_repeat) 'repeat': true,
      },
      'usb' => {'type': 'usb', 'device_id': path},
      _ => {
        'type': 'rtsp',
        'peer': _address.text.trim(),
        'stream_path': path,
        'secure': _secure,
        'transport': _secure ? 'tcp' : _transport,
        'credential_ref': _reference.text.trim().isEmpty
            ? null
            : uuid(_reference.text.trim()),
      },
    };
  }

  Future<void> _perform(Future<void> Function() work) async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _status = null;
    });
    try {
      await work();
    } on FormatException {
      if (mounted) {
        setState(() {
          _error = true;
          _status = 'Check the camera name and source fields.';
        });
      }
    } catch (error) {
      if (mounted) {
        setState(() {
          _error = true;
          _status = describeFailure(error);
        });
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _save() => _perform(() async {
    final name = _name.text.trim();
    if (name.isEmpty || name.runes.length > 80) throw const FormatException();
    final config = <String, Object?>{
      'name': name,
      'enabled': _enabled,
      'source': _source(),
      if (_cloudConsent || _initial['cloud_consent'] == true)
        'cloud_consent': _cloudConsent,
      'privacy_masks': _masks,
      'controlled_fire_regions': _regions,
    };
    final camera = widget.camera;
    if (camera == null) {
      await widget.profile.client.createCamera(config, operationId());
    } else {
      final patch = Map<String, Object?>.fromEntries(
        config.entries.where((entry) {
          final old =
              _initial[entry.key] ??
              (entry.key == 'controlled_fire_regions' ? const [] : null);
          return _canonical(entry.value) != _canonical(old);
        }),
      );
      if (_recheck) patch['controlled_fire_regions'] = _regions;
      if (_regions.isNotEmpty &&
          patch.containsKey('controlled_fire_regions') &&
          (patch.containsKey('source') || patch.containsKey('privacy_masks'))) {
        setState(() {
          _error = true;
          _status = 'Save the source or mask changes first, then review the regions on the updated camera.';
        });
        return;
      }
      if (patch.isNotEmpty) {
        await widget.profile.client.patchCamera(camera, patch, operationId());
      }
    }
    unawaited(widget.console.refreshCameras());
    if (mounted) Navigator.of(context).pop();
  });

  Future<void> _probe() => _perform(() async {
    final report = await widget.profile.client.probe(_source(), masks: _masks);
    final dims = object(report['dimensions']);
    if (mounted) {
      setState(() {
        _error = false;
        _status =
            '${dims['width']} × ${dims['height']} · ${report['native_pts'] == true ? 'native timestamps' : 'estimated timestamps'}';
      });
    }
  });

  Future<void> _onvif() => _perform(() async {
    final ref = _reference.text.trim();
    final selected = await selectOnvifSource(
      context,
      widget.profile.client,
      ref.isEmpty ? null : uuid(ref),
    );
    if (!mounted || selected == null) return;
    final source = object(selected['source']);
    final peer = text(source, 'peer');
    setState(() {
      _address.text = peer;
      _path.text = text(source, 'stream_path');
      _secure = source['secure'] == true;
      _transport = text(source, 'transport');
      _error = selected['authorized'] != true;
      _status = _error
          ? 'Authorize $peer in the Core RTSP settings before probing.'
          : 'ONVIF stream selected. Probe the source before enabling monitoring.';
    });
  });

  Future<void> _upload() => _perform(() async {
    const videos = XTypeGroup(
      label: 'Video',
      extensions: ['mp4', 'mov', 'mkv', 'webm', 'avi', 'ts'],
      uniformTypeIdentifiers: [
        'public.movie',
        'org.webmproject.webm',
        'org.matroska.mkv',
      ],
    );
    final video = await openFile(acceptedTypeGroups: [videos]);
    if (video == null || !mounted) return;
    final length = await video.length();
    if (!mounted) return;
    if (length <= 0 || length > 128 * 1024 * 1024) {
      setState(() {
        _error = true;
        _status = 'Choose a non-empty video up to 128 MiB.';
      });
      return;
    }
    final uploaded = await widget.profile.client.uploadReplay(
      video.openRead(),
      length,
    );
    if (!mounted) return;
    final dims = object(object(uploaded['source_probe'])['dimensions']);
    setState(() {
      _path.text = text(uploaded, 'file');
      _error = false;
      _status = 'Video ready for replay · ${dims['width']} × ${dims['height']}';
    });
  });

  Future<void> _remove() async {
    final camera = widget.camera;
    if (camera == null) return;
    final confirm = await showDialog<bool>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('Remove camera?'),
        content: Text(
          '${camera.name} will stop monitoring. Its incident history follows Core retention.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialog).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(dialog).pop(true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (!mounted || confirm != true) return;
    await _perform(() async {
      await widget.profile.client.deleteCamera(camera, operationId());
      unawaited(widget.console.refreshCameras());
      if (mounted) Navigator.of(context).pop();
    });
  }

  @override
  void dispose() {
    _name.dispose();
    _address.dispose();
    _path.dispose();
    _reference.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return AlertDialog(
      title: Text(widget.camera == null ? 'Add camera' : 'Camera settings'),
      content: SizedBox(
        width: 520,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              TextField(
                controller: _name,
                maxLength: 80,
                decoration: const InputDecoration(labelText: 'Camera name'),
              ),
              DropdownButtonFormField<String>(
                initialValue: _type,
                decoration: const InputDecoration(labelText: 'Source'),
                items: const [
                  DropdownMenuItem(value: 'rtsp', child: Text('RTSP / RTSPS')),
                  DropdownMenuItem(value: 'usb', child: Text('USB camera')),
                  DropdownMenuItem(
                    value: 'replay',
                    child: Text('Core replay file'),
                  ),
                ],
                onChanged: _busy
                    ? null
                    : (value) {
                        if (value != null) {
                          setState(() {
                            _type = value;
                            _path.clear();
                          });
                        }
                      },
              ),
              const SizedBox(height: 10),
              if (_type == 'rtsp') ...[
                TextField(
                  controller: _address,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'Camera IP and port',
                    hintText: '192.168.1.40:554',
                  ),
                ),
                const SizedBox(height: 10),
                TextField(
                  controller: _reference,
                  autocorrect: false,
                  decoration: const InputDecoration(
                    labelText: 'Core credential reference (optional)',
                  ),
                ),
                TextButton.icon(
                  onPressed: _busy ? null : _onvif,
                  icon: const Icon(Icons.radar),
                  label: const Text('Find ONVIF stream'),
                ),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  title: const Text('TLS (RTSPS)'),
                  value: _secure,
                  onChanged: (value) => setState(() => _secure = value),
                ),
                if (!_secure)
                  DropdownButtonFormField<String>(
                    key: ValueKey(_transport),
                    initialValue: _transport,
                    decoration: const InputDecoration(labelText: 'Transport'),
                    items: const [
                      DropdownMenuItem(value: 'tcp', child: Text('TCP')),
                      DropdownMenuItem(value: 'udp', child: Text('UDP')),
                    ],
                    onChanged: (value) =>
                        setState(() => _transport = value ?? 'tcp'),
                  ),
                const SizedBox(height: 10),
              ],
              TextField(
                controller: _path,
                enabled: !_busy,
                autocorrect: false,
                decoration: InputDecoration(
                  labelText: switch (_type) {
                    'rtsp' => 'RTSP stream path',
                    'usb' => 'Core capture device ID',
                    _ => 'File within the Core replay directory',
                  },
                ),
              ),
              if (_type == 'replay') ...[
                TextButton.icon(
                  onPressed: _busy ? null : _upload,
                  icon: const Icon(Icons.upload_file),
                  label: const Text('Upload replay video'),
                ),
                SwitchListTile(
                  contentPadding: EdgeInsets.zero,
                  title: const Text('Repeat replay'),
                  value: _repeat,
                  onChanged: (value) => setState(() => _repeat = value),
                ),
              ],
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: const Text('Monitoring enabled'),
                value: _enabled,
                onChanged: (value) => setState(() => _enabled = value),
              ),
              SwitchListTile(
                contentPadding: EdgeInsets.zero,
                title: const Text('Allow Firebase cloud analysis'),
                subtitle: const Text(
                  'The configured provider may receive masked evidence from this camera.',
                ),
                value: _cloudConsent,
                onChanged: _busy
                    ? null
                    : (value) => setState(() => _cloudConsent = value),
              ),
              const Divider(),
              Text('Privacy masks', style: theme.textTheme.titleMedium),
              for (final (index, mask) in _masks.indexed)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  title: Text('Mask ${index + 1}'),
                  subtitle: Text(
                    '${mask['left']}, ${mask['top']} → ${mask['right']}, ${mask['bottom']}',
                  ),
                  onTap: () => _addMask(index),
                  trailing: IconButton(
                    onPressed: () => setState(() => _masks.removeAt(index)),
                    tooltip: 'Remove mask',
                    icon: const Icon(Icons.close),
                  ),
                ),
              TextButton.icon(
                onPressed: _masks.length < 32 ? _addMask : null,
                icon: const Icon(Icons.add),
                label: const Text('Add mask'),
              ),
              Text(
                'Controlled fire regions',
                style: theme.textTheme.titleMedium,
              ),
              for (final (index, region) in _regions.indexed)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  title: Text(readable(text(region, 'source'))),
                  subtitle: Text(readable(text(region, 'mode'))),
                  onTap: _busy ? null : () => _editRegion(index),
                  trailing: IconButton(
                    onPressed: _busy
                        ? null
                        : () => setState(() => _regions.removeAt(index)),
                    tooltip: 'Remove region',
                    icon: const Icon(Icons.close),
                  ),
                ),
              TextButton.icon(
                onPressed: _busy || _regions.length >= 16 ? null : _editRegion,
                icon: const Icon(Icons.add),
                label: const Text('Add fire region'),
              ),
              if (widget.camera?.sceneReview == 'needs_review' &&
                  _regions.isNotEmpty)
                CheckboxListTile(
                  contentPadding: EdgeInsets.zero,
                  title: const Text(
                    'Confirm regions against the current scene',
                  ),
                  value: _recheck,
                  onChanged: _busy
                      ? null
                      : (value) => setState(() => _recheck = value == true),
                ),
              if (_status != null)
                Padding(
                  padding: const EdgeInsets.only(top: 10),
                  child: Text(
                    _status!,
                    style: TextStyle(
                      color: _error
                          ? theme.colorScheme.error
                          : theme.colorScheme.primary,
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
      actions: [
        if (widget.camera != null)
          TextButton(
            onPressed: _busy ? null : _remove,
            child: const Text('Remove'),
          ),
        TextButton(
          onPressed: _busy ? null : _probe,
          child: const Text('Probe'),
        ),
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _busy ? null : _save,
          child: Text(_busy ? 'Working…' : 'Save'),
        ),
      ],
    );
  }
}

Object? _canonical(Object? value) {
  if (value is List) return jsonEncode(value.map(_canonical).toList());
  if (value is Map<String, Object?>) {
    final names = value.keys.toList()..sort();
    return jsonEncode({for (final key in names) key: _canonical(value[key])});
  }
  return value;
}

class _MaskDialog extends StatefulWidget {
  const _MaskDialog(this.mask);
  final Json? mask;
  @override
  State<_MaskDialog> createState() => _MaskDialogState();
}

class _MaskDialogState extends State<_MaskDialog> {
  final _form = GlobalKey<FormState>();
  static const _keys = ['left', 'top', 'right', 'bottom'];
  final _values = <String, int>{};

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Privacy mask'),
    content: SizedBox(
      width: 320,
      child: Form(
        key: _form,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Text(
                'Coordinates range from 0 at the left or top to 10000 at the right or bottom.',
              ),
              for (final name in _keys)
                TextFormField(
                  initialValue:
                      (widget.mask?[name] ??
                              (name == 'right' || name == 'bottom' ? 5000 : 0))
                          .toString(),
                  decoration: InputDecoration(labelText: name),
                  keyboardType: TextInputType.number,
                  onChanged: (_) => _form.currentState?.validate(),
                  validator: (raw) {
                    final value = int.tryParse(raw ?? '');
                    if (value == null || value < 0 || value > 10000) {
                      return 'Enter 0–10000.';
                    }
                    _values[name] = value;
                    return null;
                  },
                ),
            ],
          ),
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: () {
          if (!_form.currentState!.validate()) return;
          if (_values['left']! >= _values['right']! ||
              _values['top']! >= _values['bottom']!) {
            showFailure(context, const StillWatchException('invalid_request'));
            return;
          }
          Navigator.of(context).pop(Json.of(_values));
        },
        child: const Text('Apply'),
      ),
    ],
  );
}
