import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:bonsoir/bonsoir.dart';
import 'package:flutter/services.dart';
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'profiles.dart';

Future<void> addCore(BuildContext context, ConsoleState console) =>
    showDialog<void>(context: context, builder: (_) => _PairDialog(console));

class _PairDialog extends StatefulWidget {
  const _PairDialog(this.console);
  final ConsoleState console;

  @override
  State<_PairDialog> createState() => _PairDialogState();
}

class _PairDialogState extends State<_PairDialog> {
  final _offer = TextEditingController();
  final _name = TextEditingController();
  final _address = TextEditingController();
  final _nearby = <String, (String, String)>{};
  BonsoirDiscovery? _discovery;
  StreamSubscription<BonsoirDiscoveryEvent>? _subscription;
  Timer? _scanTimer;
  PairingOffer? _parsed;
  String? _error;
  bool _scanning = false;
  bool _busy = false;

  Future<void> _discover() async {
    await _stopDiscovery();
    if (!mounted) return;
    setState(() {
      _error = null;
      _nearby.clear();
      _scanning = true;
    });
    final discovery = BonsoirDiscovery(
      type: '_stillwatch._tcp',
      printLogs: false,
    );
    _discovery = discovery;
    try {
      await discovery.initialize().timeout(const Duration(seconds: 4));
      if (!mounted || _discovery != discovery) {
        await discovery.stop();
        return;
      }
      _subscription = discovery.eventStream?.listen(
        (event) {
          if (!mounted || _discovery != discovery) return;
          switch (event) {
            case BonsoirDiscoveryServiceFoundEvent():
              event.service.resolve(discovery.serviceResolver);
            case BonsoirDiscoveryServiceResolvedEvent() ||
                BonsoirDiscoveryServiceUpdatedEvent():
              final service = event.service;
              if (service == null) return;
              final ip = service.hostAddresses
                  .map(InternetAddress.tryParse)
                  .whereType<InternetAddress>()
                  .firstOrNull;
              if (ip == null || service.port <= 0 || service.port > 65535) {
                return;
              }
              final address = Uri(
                scheme: 'https',
                host: ip.address,
                port: service.port,
              ).origin;
              setState(() => _nearby[service.name] = (service.name, address));
            case BonsoirDiscoveryServiceLostEvent():
              setState(() => _nearby.remove(event.service.name));
            default:
              break;
          }
        },
        onError: (Object _) {
          if (mounted) {
            setState(
              () => _error = 'Network discovery is unavailable. Enter the Core address manually.',
            );
          }
        },
      );
      await discovery.start().timeout(const Duration(seconds: 4));
      _scanTimer = Timer(const Duration(seconds: 8), () {
        unawaited(_stopDiscovery());
        if (mounted) setState(() => _scanning = false);
      });
    } catch (_) {
      await _stopDiscovery();
      if (!mounted) return;
      setState(() {
        _scanning = false;
        _error = 'Network discovery is unavailable. Enter the Core address manually.';
      });
    }
  }

  Future<void> _stopDiscovery() async {
    _scanTimer?.cancel();
    final discovery = _discovery;
    final subscription = _subscription;
    _discovery = null;
    _subscription = null;
    await subscription?.cancel();
    if (discovery?.isReady == true && discovery?.isStopped == false) {
      try {
        await discovery!.stop();
      } catch (_) {
        // The platform can already have stopped browsing during suspension.
      }
    }
  }

  void _review() {
    try {
      if (_offer.text.length > 8192) throw const FormatException();
      final offer = PairingOffer.fromJson(
        object(jsonDecode(_offer.text.trim())),
      );
      if (!offer.expires.isAfter(DateTime.now())) {
        throw const StillWatchException('credentials');
      }
      final supplied = _address.text.trim();
      if (supplied.isNotEmpty) {
        final uri = Uri.tryParse(supplied);
        if (uri == null ||
            uri.scheme != 'https' ||
            uri.host.isEmpty ||
            uri.userInfo.isNotEmpty ||
            uri.origin != offer.core.origin.origin ||
            uri.hasQuery ||
            uri.hasFragment ||
            (uri.path.isNotEmpty && uri.path != '/')) {
          throw const StillWatchException('identity');
        }
      }
      _offer.clear();
      setState(() {
        _parsed = offer;
        _error = null;
      });
    } catch (error) {
      setState(
        () => _error = error is StillWatchException && error.code == 'identity'
            ? 'The offer address differs from the selected Core address.'
            : 'Use a valid, unexpired pairing offer from the Core owner.',
      );
    }
  }

  Future<void> _pair() async {
    final offer = _parsed;
    final label = _name.text.trim();
    if (offer == null || label.isEmpty || _busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await widget.console.enroll(offer, label, label);
      if (mounted) Navigator.of(context).pop();
    } catch (error) {
      if (mounted) setState(() => _error = describeFailure(error));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _scan() async {
    const scanner = MethodChannel('com.stillwatch.companion/pairing');
    try {
      final value = await scanner.invokeMethod<String>('scanQr');
      if (!mounted || value == null || value.length > 8192) return;
      _offer.text = value;
      _review();
    } on PlatformException catch (error) {
      if (!mounted) return;
      setState(() {
        _error = error.code == 'camera_permission'
            ? 'To scan a QR code, allow camera access in Settings. You can also paste the pairing offer.'
            : 'The scanner is unavailable. You can paste the pairing offer.';
      });
    }
  }

  @override
  void dispose() {
    unawaited(_stopDiscovery());
    _offer.dispose();
    _name.dispose();
    _address.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final offer = _parsed;
    return AlertDialog(
      title: Text(offer == null ? 'Add a Core' : 'Verify the Core'),
      content: SizedBox(
        width: 485,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (_error != null) ...[
                Text(
                  _error!,
                  style: TextStyle(color: Theme.of(context).colorScheme.error),
                ),
                const SizedBox(height: 12),
              ],
              if (offer == null) ...[
                const Text(
                  'Get a one-time pairing offer from your Core owner.',
                ),
                const SizedBox(height: 16),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    OutlinedButton.icon(
                      onPressed: _scanning ? null : _discover,
                      icon: const Icon(Icons.radar),
                      label: Text(
                        _scanning ? 'Looking nearby…' : 'Find nearby',
                      ),
                    ),
                    if (Platform.isAndroid || Platform.isIOS)
                      OutlinedButton.icon(
                        onPressed: _scan,
                        icon: const Icon(Icons.qr_code_scanner),
                        label: const Text('Scan QR'),
                      ),
                  ],
                ),
                for (final (name, address) in _nearby.values)
                  ListTile(
                    dense: true,
                    title: Text(name),
                    subtitle: Text(address),
                    onTap: () => _address.text = address,
                  ),
                const SizedBox(height: 12),
                TextField(
                  controller: _address,
                  keyboardType: TextInputType.url,
                  decoration: const InputDecoration(
                    labelText: 'Core HTTPS address (optional)',
                  ),
                ),
                const SizedBox(height: 12),
                TextField(
                  controller: _offer,
                  maxLines: 4,
                  autocorrect: false,
                  enableSuggestions: false,
                  decoration: const InputDecoration(
                    labelText: 'Pairing offer',
                    hintText: 'Paste the JSON from your Core',
                  ),
                ),
              ] else ...[
                Text(
                  offer.core.origin.origin,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
                const SizedBox(height: 8),
                const Text(
                  'Compare this SHA-256 fingerprint with the Core owner before pairing.',
                ),
                const SizedBox(height: 6),
                SelectableText(
                  offer.core.pin,
                  style: const TextStyle(fontFamily: 'monospace'),
                ),
                const SizedBox(height: 16),
                TextField(
                  controller: _name,
                  maxLength: 80,
                  decoration: const InputDecoration(
                    labelText: 'Device and profile name',
                  ),
                  onChanged: (_) => setState(() {}),
                  onSubmitted: (_) => _pair(),
                ),
              ],
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _busy || (offer != null && _name.text.trim().isEmpty)
              ? null
              : offer == null
              ? _review
              : _pair,
          child: Text(
            _busy
                ? 'Pairing…'
                : offer == null
                ? 'Review offer'
                : 'Trust and pair',
          ),
        ),
      ],
    );
  }
}
