import 'dart:async';

import 'package:flutter_webrtc/flutter_webrtc.dart';
import 'package:material_ui/material_ui.dart';
import 'package:stillwatch_sdk/stillwatch_sdk.dart';

import 'profiles.dart';

class CameraPreview extends StatefulWidget {
  const CameraPreview({
    super.key,
    required this.profile,
    required this.camera,
    required this.active,
    this.showAge = false,
  });
  final CoreProfile profile;
  final Camera camera;
  final bool active;
  final bool showAge;

  @override
  State<CameraPreview> createState() => _CameraPreviewState();
}

class _CameraPreviewState extends State<CameraPreview> {
  static const _maxAge = Duration(seconds: 5);
  MemoryImage? _image;
  Duration? _ageAtReceipt;
  final _sinceReceipt = Stopwatch();
  Timer? _clock;
  Timer? _expiry;
  bool _pending = false;
  int _generation = 0;

  @override
  void initState() {
    super.initState();
    _watchAge();
    if (widget.active) unawaited(_fetch());
  }

  @override
  void didUpdateWidget(CameraPreview oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.showAge != oldWidget.showAge ||
        widget.active != oldWidget.active) {
      _watchAge();
    }
    if (widget.profile != oldWidget.profile ||
        widget.camera.id != oldWidget.camera.id ||
        widget.camera.sceneRevision != oldWidget.camera.sceneRevision ||
        widget.camera.sourceEpoch != oldWidget.camera.sourceEpoch ||
        widget.active != oldWidget.active) {
      _generation++;
      _replace(null);
      _pending = false;
      if (widget.active) unawaited(_fetch());
    } else if (widget.active &&
        widget.camera.runtime['frame'] != oldWidget.camera.runtime['frame']) {
      unawaited(_fetch());
    }
  }

  void _watchAge() {
    _clock?.cancel();
    if (widget.showAge && widget.active) {
      _clock = Timer.periodic(const Duration(seconds: 1), (_) {
        if (_image != null && mounted) setState(() {});
      });
    }
  }

  void _replace(CameraSnapshot? snapshot) {
    _expiry?.cancel();
    final remaining = _maxAge - (snapshot?.ageAtReceipt ?? Duration.zero);
    if (remaining <= Duration.zero) snapshot = null;
    final old = _image;
    if (old != null) PaintingBinding.instance.imageCache.evict(old);
    _image = snapshot == null ? null : MemoryImage(snapshot.jpeg);
    _ageAtReceipt = snapshot?.ageAtReceipt;
    _sinceReceipt
      ..stop()
      ..reset();
    if (_ageAtReceipt != null) _sinceReceipt.start();
    if (_image != null) {
      _expiry = Timer(remaining, () => setState(() => _replace(null)));
    }
  }

  Future<void> _fetch() async {
    if (_pending) return;
    _pending = true;
    final generation = _generation;
    CameraSnapshot? image;
    try {
      image = await widget.profile.client.cameraSnapshot(widget.camera.id);
    } catch (_) {
      // Runtime state and the full viewer can show the failure without a stale thumbnail.
    } finally {
      if (mounted && generation == _generation) {
        setState(() {
          _pending = false;
          _replace(image);
        });
      }
    }
  }

  @override
  void dispose() {
    _clock?.cancel();
    _replace(null);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final image = _image;
    final age = _ageAtReceipt;
    return ColoredBox(
      color: const Color(0xff0b1517),
      child: Center(
        child: image == null
            ? Icon(
                Icons.videocam_outlined,
                color: Theme.of(context).colorScheme.onSurfaceVariant,
                size: 36,
              )
            : Stack(
                alignment: Alignment.bottomLeft,
                children: [
                  Image(
                    image: image,
                    fit: BoxFit.contain,
                    width: double.infinity,
                    height: double.infinity,
                  ),
                  if (widget.showAge)
                    Positioned(
                      left: 12,
                      bottom: 10,
                      child: DecoratedBox(
                        decoration: BoxDecoration(
                          color: Colors.black87,
                          borderRadius: BorderRadius.circular(6),
                        ),
                        child: Padding(
                          padding: const EdgeInsets.all(8),
                          child: Text(
                            age == null
                                ? 'Snapshot age unavailable'
                                : 'Snapshot from ${(age + _sinceReceipt.elapsed).inSeconds}s ago',
                            style: const TextStyle(color: Colors.white),
                          ),
                        ),
                      ),
                    ),
                ],
              ),
      ),
    );
  }
}

class LiveView extends StatefulWidget {
  const LiveView({
    super.key,
    required this.console,
    required this.profile,
    required this.initial,
  });
  final ConsoleState console;
  final CoreProfile profile;
  final Camera initial;

  @override
  State<LiveView> createState() => _LiveViewState();
}

class _LiveViewState extends State<LiveView> {
  _Playback? _playback;
  Camera? _camera;
  int _generation = 0;
  bool _starting = false;
  String? _failure;
  bool _active = false;
  Future<void> _closing = Future.value();

  @override
  void initState() {
    super.initState();
    widget.console.addListener(_observe);
    _observe();
  }

  void _observe() {
    final console = widget.console;
    final camera = console.cameras
        .where((item) => item.id == widget.initial.id)
        .firstOrNull;
    final active =
        console.foreground &&
        console.selected == widget.profile &&
        console.link == LinkState.connected &&
        widget.profile.can('streams_read') &&
        camera != null &&
        (camera.state == 'streaming' || camera.state == 'replaying');
    final previousCamera = _camera;
    final newScope =
        camera?.sceneRevision != previousCamera?.sceneRevision ||
        camera?.sourceEpoch != previousCamera?.sourceEpoch;
    final changedRepeat =
        camera?.repeatedFrame != previousCamera?.repeatedFrame;
    _camera = camera;
    if (active == _active && !newScope && !changedRepeat) {
      if (active &&
          !_starting &&
          _playback == null &&
          camera.runtime['frame'] != previousCamera?.runtime['frame']) {
        setState(() {});
      }
      return;
    }
    _active = active;
    if (active) {
      if (newScope || _playback == null && !_starting) {
        unawaited(_connect());
      } else {
        setState(() {});
      }
    } else {
      _generation++;
      final previous = _playback;
      setState(() {
        _starting = false;
        _playback = null;
        _failure = console.link != LinkState.connected
            ? 'The Core is offline.'
            : camera?.repeatedFrame == true
            ? 'The camera is sending the same frame.'
            : 'The camera is unavailable.';
      });
      if (previous != null) _closePlayback(previous);
    }
  }

  Future<void> _connect() async {
    final camera = _camera;
    if (!_active || camera == null) return;
    final generation = ++_generation;
    final previous = _playback;
    final next = _Playback(widget.profile.client, camera);
    setState(() {
      _starting = true;
      _failure = null;
      _playback = null;
    });
    if (previous != null) _closePlayback(previous);
    await _closing;
    if (!mounted || generation != _generation) return;
    _playback = next;
    try {
      await next
          .open(
            (reason) {
              if (!mounted || generation != _generation || _playback != next) {
                return;
              }
              setState(() {
                _starting = false;
                _playback = null;
                _failure = switch (reason) {
                  _PlaybackEnd.stalled =>
                    'Video stopped updating. Reconnect to resume playback.',
                  _PlaybackEnd.unverified => 'Video freshness could not be verified. Reconnect to retry.',
                  _PlaybackEnd.disconnected => 'The live connection stopped. Retry or view the latest snapshot.',
                };
              });
              _closePlayback(next);
            },
            renew: () {
              if (mounted &&
                  _active &&
                  generation == _generation &&
                  _playback == next) {
                unawaited(_connect());
              }
            },
          )
          .timeout(const Duration(seconds: 30));
      if (!mounted || generation != _generation || _playback != next) {
        _closePlayback(next);
        return;
      }
      setState(() => _starting = false);
    } catch (error) {
      _closePlayback(next);
      if (!mounted || generation != _generation || _playback != next) return;
      setState(() {
        _starting = false;
        _playback = null;
        _failure = switch (error) {
          StillWatchException(status: 429) =>
            'The Core reached its video limit.',
          StillWatchException(status: 503) =>
            'The Core has no ready video stream.',
          StillWatchException(status: 401 || 403) =>
            'This device cannot view the video.',
          StillWatchException(code: 'media_scope_changed') =>
            'The camera source changed. Reconnect to see the current video.',
          StillWatchException(code: 'ice_timeout') =>
            'Video network discovery timed out. Check the media route.',
          StillWatchException(code: 'first_frame_timeout') => 'The video connection opened without a picture. Reconnect to retry.',
          StillWatchException(code: 'media_disconnected') =>
            'The video route disconnected. Check the network and reconnect.',
          _ =>
            'Live video is unavailable. Check the Core media route or retry.',
        };
      });
    }
  }

  void _closePlayback(_Playback playback) {
    _closing = _closing.then((_) => playback.close());
  }

  @override
  void dispose() {
    widget.console.removeListener(_observe);
    _generation++;
    final playback = _playback;
    if (playback != null) _closePlayback(playback);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final playback = _playback;
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.initial.name),
        actions: [
          IconButton(
            onPressed: _active && !_starting ? _connect : null,
            icon: const Icon(Icons.refresh),
            tooltip: 'Reconnect video',
          ),
        ],
      ),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 1280),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Expanded(
                child: Padding(
                  padding: const EdgeInsets.all(16),
                  child: ClipRRect(
                    borderRadius: BorderRadius.circular(16),
                    child: ColoredBox(
                      color: const Color(0xff071012),
                      child: SizedBox.expand(
                        child: _starting
                            ? const Center(child: CircularProgressIndicator())
                            : playback != null
                            ? RTCVideoView(
                                playback.renderer,
                                objectFit: RTCVideoViewObjectFit
                                    .RTCVideoViewObjectFitContain,
                              )
                            : _camera != null
                            ? CameraPreview(
                                profile: widget.profile,
                                camera: _camera!,
                                active: _active,
                                showAge: true,
                              )
                            : const Center(
                                child: Icon(
                                  Icons.videocam_off_outlined,
                                  size: 42,
                                ),
                              ),
                      ),
                    ),
                  ),
                ),
              ),
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                child: Column(
                  children: [
                    Text(
                      _starting
                          ? 'Connecting video…'
                          : playback != null
                          ? (widget.initial.live
                                ? 'Live · privacy masked'
                                : 'Replay · privacy masked')
                          : _failure ?? 'Waiting for the camera.',
                    ),
                    if (!_starting && playback == null && _active)
                      const Text('Camera snapshots'),
                    if (_camera?.repeatedFrame == true)
                      const Text('Visual changes unconfirmed'),
                    if (_camera?.sourceLagging == true)
                      const Text('Camera stream is delayed'),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

enum _PlaybackEnd { disconnected, stalled, unverified }

final class _Playback {
  _Playback(this.client, this.camera);
  final StillWatchClient client;
  final Camera camera;
  final renderer = RTCVideoRenderer();
  RTCPeerConnection? _peer;
  MediaStream? _stream;
  StreamGrant? _grant;
  bool _initialized = false;
  bool _closed = false;
  bool _playing = false;
  bool _connectionEnded = false;
  Timer? _freshnessTimer;
  Timer? _renewalTimer;
  static Future<void>? _windowsInitialization;
  final _ice = Completer<void>();
  final _videoTrack = Completer<RTCTrackEvent?>();
  final _firstFrame = Completer<void>();

  Future<void> open(
    void Function(_PlaybackEnd) interrupted, {
    required VoidCallback renew,
  }) async {
    if (WebRTC.platformIsWindows) {
      await (_windowsInitialization ??= _initializeWindows());
      _check();
    }
    await renderer.initialize();
    if (_closed) {
      await _dispose(renderer.dispose);
      _check();
    }
    _initialized = true;
    renderer.onFirstFrameRendered = _markFirstFrame;
    renderer.onResize = () {
      if (renderer.videoWidth > 0 && renderer.videoHeight > 0) {
        _markFirstFrame();
      }
    };
    final grant = await _streamGrant();
    if (_closed) {
      unawaited(_release(grant));
      _check();
    }
    _grant = grant;
    if (grant.cameraId != camera.id ||
        grant.sourceEpoch != camera.sourceEpoch ||
        grant.sceneRevision != camera.sceneRevision) {
      throw const StillWatchException('media_scope_changed');
    }
    final peer = await createPeerConnection({
      'sdpSemantics': 'unified-plan',
      'iceServers': [
        for (final server in grant.iceServers)
          {
            'urls': [text(server, 'url')],
            'username': text(server, 'username'),
            'credential': text(server, 'credential'),
          },
      ],
    });
    if (_closed) {
      await _disposePeer(peer);
      _check();
    }
    _peer = peer;
    peer.onIceGatheringState = (state) {
      if (state == RTCIceGatheringState.RTCIceGatheringStateComplete &&
          !_ice.isCompleted) {
        _ice.complete();
      }
    };
    peer.onConnectionState = (state) {
      if (_closed) return;
      final ended =
          state == RTCPeerConnectionState.RTCPeerConnectionStateFailed ||
          state == RTCPeerConnectionState.RTCPeerConnectionStateClosed;
      _connectionEnded = ended;
      if (_playing &&
          (ended ||
              state ==
                  RTCPeerConnectionState.RTCPeerConnectionStateDisconnected)) {
        interrupted(_PlaybackEnd.disconnected);
      } else if (ended && !_firstFrame.isCompleted) {
        _firstFrame.completeError(
          const StillWatchException('media_disconnected'),
        );
      }
    };
    // A native callback can fail while the remote description is being set.
    // Observe its result before passing the offer to the Core.
    unawaited(_firstFrame.future.then<void>((_) {}, onError: (Object _) {}));
    peer.onTrack = (event) {
      if (!_videoTrack.isCompleted && event.track.kind == 'video') {
        _videoTrack.complete(event);
      }
    };
    await peer.addTransceiver(
      kind: RTCRtpMediaType.RTCRtpMediaTypeVideo,
      init: RTCRtpTransceiverInit(direction: TransceiverDirection.RecvOnly),
    );
    _check();
    final offer = await peer.createOffer();
    _check();
    await peer.setLocalDescription(offer);
    _check();
    if (await peer.getIceGatheringState() !=
        RTCIceGatheringState.RTCIceGatheringStateComplete) {
      await _ice.future.timeout(
        const Duration(seconds: 5),
        onTimeout: () => throw const StillWatchException('ice_timeout'),
      );
    }
    _check();
    final local = await peer.getLocalDescription();
    final sdp = local?.sdp;
    if (sdp == null) throw const StillWatchException('protocol');
    final answer = await client.offer(grant, sdp);
    _check();
    await Future.wait<void>([
      _firstFrame.future.timeout(
        const Duration(seconds: 12),
        onTimeout: () => throw const StillWatchException('first_frame_timeout'),
      ),
      _attachRemoteVideo(peer, answer.sdp),
    ], eagerError: true);
    _check();
    if (_connectionEnded) {
      throw const StillWatchException('media_disconnected');
    }
    _playing = true;
    _watchFrames(peer, interrupted);
    // Rotate the grant before the Core's 30-minute media session expires.
    _renewalTimer = Timer(const Duration(minutes: 25), renew);
  }

  void _watchFrames(
    RTCPeerConnection peer,
    void Function(_PlaybackEnd) interrupted,
  ) {
    final sinceProgress = Stopwatch()..start();
    var previous = <String, num>{};

    Future<void> check() async {
      final current = <String, num>{};
      try {
        final reports = await peer.getStats().timeout(
          const Duration(seconds: 2),
        );
        if (_closed) return;
        for (final report in reports) {
          final count = report.values['framesDecoded'];
          if (report.type != 'inbound-rtp' ||
              count is! num ||
              !count.isFinite ||
              count < 0) {
            continue;
          }
          if (count > (previous[report.id] ?? 0)) sinceProgress.reset();
          current[report.id] = count;
        }
      } catch (_) {
        // A peer with unavailable statistics cannot establish freshness.
      }
      if (_closed) return;
      if (current.isNotEmpty) previous = current;
      if (sinceProgress.elapsed >= const Duration(seconds: 8)) {
        interrupted(
          current.isEmpty ? _PlaybackEnd.unverified : _PlaybackEnd.stalled,
        );
      } else {
        _freshnessTimer = Timer(const Duration(seconds: 2), check);
      }
    }

    _freshnessTimer = Timer(Duration.zero, check);
  }

  void _markFirstFrame() {
    if (!_firstFrame.isCompleted) _firstFrame.complete();
  }

  Future<StreamGrant> _streamGrant() async {
    for (var attempt = 0; ; attempt++) {
      _check();
      try {
        return await client.streamSession(camera.id, profile: 'detail');
      } on StillWatchException catch (error) {
        if (error.status != 503 || attempt == 2) rethrow;
        await Future<void>.delayed(Duration(milliseconds: 250 * (attempt + 1)));
      }
    }
  }

  static Future<void> _initializeWindows() async {
    try {
      await WebRTC.initialize();
    } catch (_) {
      _windowsInitialization = null;
      rethrow;
    }
  }

  Future<void> _attachRemoteVideo(RTCPeerConnection peer, String sdp) async {
    // Native track events can arrive while the remote stream is still being registered.
    await peer.setRemoteDescription(RTCSessionDescription(sdp, 'answer'));
    final event = await _videoTrack.future;
    _check();
    if (event == null) return;
    final stream =
        event.streams.firstOrNull ?? await createLocalMediaStream('stillwatch');
    if (_closed) {
      await _dispose(stream.dispose);
      return;
    }
    _stream = stream;
    if (event.streams.isEmpty) await stream.addTrack(event.track);
    _check();
    await renderer.setSrcObject(stream: stream, trackId: event.track.id);
  }

  void _check() {
    if (_closed) throw const StillWatchException('closed');
  }

  Future<void> _release(StreamGrant grant) async {
    try {
      await client.closeStream(grant).timeout(const Duration(seconds: 2));
    } catch (_) {
      // Core expires disconnected sessions and stops the local renderer immediately.
    }
  }

  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    _freshnessTimer?.cancel();
    _renewalTimer?.cancel();
    if (!_ice.isCompleted) _ice.complete();
    if (!_videoTrack.isCompleted) _videoTrack.complete(null);
    if (!_firstFrame.isCompleted) _firstFrame.complete();
    final grant = _grant;
    final release = grant == null ? null : _release(grant);
    final peer = _peer;
    if (_initialized) await _dispose(renderer.dispose);
    if (peer != null) await _disposePeer(peer);
    final stream = _stream;
    if (stream != null) await _dispose(stream.dispose);
    if (release != null) await release;
  }

  Future<void> _disposePeer(RTCPeerConnection peer) async {
    await _dispose(peer.close);
    await _dispose(peer.dispose);
  }

  Future<void> _dispose(Future<void> Function() dispose) async {
    try {
      await dispose().timeout(const Duration(seconds: 2));
    } catch (_) {
      // A failed platform call must not keep the player open or block reconnect.
    }
  }
}
