import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'profiles.dart';

Future<Json?> selectOnvifSource(
  BuildContext context,
  StillWatchClient client,
  String? credential,
) => showDialog<Json>(
  context: context,
  builder: (_) => _OnvifPicker(client, credential),
);

class _OnvifPicker extends StatefulWidget {
  const _OnvifPicker(this.client, this.credential);

  final StillWatchClient client;
  final String? credential;

  @override
  State<_OnvifPicker> createState() => _OnvifPickerState();
}

class _OnvifPickerState extends State<_OnvifPicker> {
  final _endpoint = TextEditingController();
  List<Json> _devices = const [];
  List<Json> _profiles = const [];
  String? _status;
  bool _busy = false;

  Future<void> _run(Future<void> Function() work) async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _status = null;
    });
    try {
      await work();
    } catch (error) {
      if (mounted) setState(() => _status = describeFailure(error));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _discover() => _run(() async {
    final devices = await widget.client.discoverOnvif();
    if (!mounted) return;
    setState(() {
      _devices = devices;
      if (devices.isEmpty) _status = 'No cameras replied on the Core network.';
    });
  });

  Future<void> _readProfiles() => _run(() async {
    setState(() => _profiles = const []);
    final profiles = await widget.client.onvifProfiles(
      _endpoint.text.trim(),
      credential: widget.credential,
    );
    if (!mounted) return;
    setState(() {
      _profiles = profiles;
      if (profiles.isEmpty) _status = 'The camera has no video profiles.';
    });
  });

  Future<void> _choose(Json profile) => _run(() async {
    final result = await widget.client.onvifStream(
      _endpoint.text.trim(),
      text(profile, 'media'),
      text(profile, 'token'),
      credential: widget.credential,
    );
    if (!mounted) return;
    Navigator.of(context).pop(result);
  });

  @override
  void dispose() {
    _endpoint.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('ONVIF cameras'),
    content: SizedBox(
      width: 500,
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (_status != null) Text(_status!),
            OutlinedButton.icon(
              onPressed: _busy ? null : _discover,
              icon: const Icon(Icons.radar),
              label: const Text('Discover on Core network'),
            ),
            for (final device in _devices)
              ListTile(
                contentPadding: EdgeInsets.zero,
                title: Text(text(device, 'endpoint')),
                subtitle: device['authorized'] == true
                    ? null
                    : const Text('Authorize this endpoint in Core settings.'),
                enabled: !_busy && device['authorized'] == true,
                onTap: () {
                  _endpoint.text = text(device, 'endpoint');
                  _readProfiles();
                },
              ),
            TextField(
              controller: _endpoint,
              enabled: !_busy,
              keyboardType: TextInputType.url,
              autocorrect: false,
              onChanged: (_) => setState(() => _profiles = const []),
              decoration: const InputDecoration(
                labelText: 'Authorized ONVIF device URL',
                hintText: 'https://192.168.1.40/onvif/device_service',
              ),
            ),
            TextButton(
              onPressed: _busy || _endpoint.text.trim().isEmpty
                  ? null
                  : _readProfiles,
              child: const Text('Load video profiles'),
            ),
            if (_busy) const LinearProgressIndicator(),
            for (final profile in _profiles)
              ListTile(
                contentPadding: EdgeInsets.zero,
                title: Text(text(profile, 'name')),
                subtitle: Text(
                  text(profile, 'media') == 'media2' ? 'Media2' : 'Media',
                ),
                enabled: !_busy,
                onTap: () => _choose(profile),
              ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Close'),
      ),
    ],
  );
}
