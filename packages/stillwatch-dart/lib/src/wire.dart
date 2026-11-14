import 'dart:convert';
import 'dart:math';
import 'dart:typed_data';

typedef Json = Map<String, Object?>;

final _decimal = RegExp(r'^(0|[1-9][0-9]{0,19})(?![\s\S])');
final _uuid = RegExp(
  r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$',
);
final _limit = (BigInt.one << 64) - BigInt.one;
final _random = Random.secure();

String counter(Object? input) {
  if (input is! String ||
      !_decimal.hasMatch(input) ||
      BigInt.parse(input) > _limit) {
    throw const StillWatchException('protocol');
  }
  return input;
}

String uuid(Object? input) {
  if (input is! String ||
      !_uuid.hasMatch(input) ||
      input == '00000000-0000-0000-0000-000000000000') {
    throw const StillWatchException('protocol');
  }
  return input;
}

String operationId() {
  final bytes = List.generate(16, (_) => _random.nextInt(256));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  final text = hex(bytes);
  return '${text.substring(0, 8)}-${text.substring(8, 12)}-${text.substring(12, 16)}-${text.substring(16, 20)}-${text.substring(20)}';
}

String hex(List<int> bytes) =>
    bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();

Uint8List uuidBytes(String value) {
  final compact = uuid(value).replaceAll('-', '');
  return Uint8List.fromList(
    List.generate(
      16,
      (i) => int.parse(compact.substring(2 * i, 2 * i + 2), radix: 16),
    ),
  );
}

Json object(Object? value) {
  if (value is! Map<String, Object?>)
    throw const StillWatchException('protocol');
  return value;
}

Json decodeObject(List<int> bytes) {
  try {
    return object(jsonDecode(utf8.decode(bytes)));
  } on FormatException {
    throw const StillWatchException('protocol');
  }
}

String text(Json value, String key) {
  final field = value[key];
  if (field is! String) throw const StillWatchException('protocol');
  return field;
}

DateTime timestamp(Json value, String key) {
  final field = text(value, key);
  final date = DateTime.tryParse(field);
  if (date == null || !date.isUtc) throw const StillWatchException('protocol');
  return date;
}

List<Json> objects(Json value, String key) {
  final field = value[key];
  if (field is! List) throw const StillWatchException('protocol');
  return List.unmodifiable(field.map(object));
}

final class StillWatchException implements Exception {
  const StillWatchException(this.code, {this.status});

  final String code;
  final int? status;

  bool get canReconnect =>
      status == 429 ||
      status == 502 ||
      status == 503 ||
      status == 504 ||
      code == 'transport' ||
      code == 'deadline';

  @override
  String toString() =>
      'StillWatchException($code${status == null ? '' : ', HTTP $status'})';
}

final class Cursor {
  Cursor(String value)
    : generation = uuid(value.split(':').first),
      sequence = counter(
        value.contains(':') ? value.substring(value.indexOf(':') + 1) : null,
      );

  final String generation;
  final String sequence;

  @override
  String toString() => '$generation:$sequence';
}

final class CoreInfo {
  CoreInfo.fromJson(Json value)
    : id = uuid(value['instance_id']),
      cameraSampling = value['camera_sampling'] == true,
      cameraProbeCapacity = value['camera_probe_capacity'] == true,
      analysisCapacityProbe = value['analysis_capacity_probe'] == true,
      cameraPriority = value['camera_priority'] == true,
      cameraMainStream = value['camera_main_stream'] == true,
      version = object(value['version']) {
    if (version['major'] != 2 || version['minor'] is! int) {
      throw const StillWatchException('identity');
    }
  }

  final String id;
  final bool cameraSampling;
  final bool cameraProbeCapacity;
  final bool analysisCapacityProbe;
  final bool cameraPriority;
  final bool cameraMainStream;
  final Json version;
}

final class Camera {
  Camera.fromJson(Json value)
    : id = uuid(value['id']),
      revision = counter(value['revision']),
      sourceEpoch = counter(value['source_epoch']),
      sceneRevision = counter(value['scene_revision']),
      configuration = Map.unmodifiable(object(value['config'])),
      runtime = Map.unmodifiable(object(value['runtime'])),
      dimensions = value['dimensions'] == null
          ? null
          : Map.unmodifiable(object(value['dimensions'])),
      live = value['live'] == true,
      sceneReview = text(value, 'scene_review');

  final String id;
  final String revision;
  final String sourceEpoch;
  final String sceneRevision;
  final Json configuration;
  final Json runtime;
  final Json? dimensions;
  final bool live;
  final String sceneReview;

  String get name => text(configuration, 'name');
  bool get enabled => configuration['enabled'] == true;
  String get state => text(runtime, 'state');
  bool get repeatedFrame => runtime['repeated_frame'] == true;
  bool get sourceLagging => runtime['source_lagging'] == true;
  bool get controlledSceneSuspended =>
      runtime['controlled_scene_suspended'] == true;
}

final class AnalysisOrigin {
  AnalysisOrigin.fromJson(Json value)
    : modelManifestSha256 = text(value, 'model_manifest_sha256'),
      policyVersion = text(value, 'policy_version');

  final String modelManifestSha256;
  final String policyVersion;
}

final class Incident {
  Incident.fromJson(Json value)
    : id = uuid(value['id']),
      cameraId = uuid(value['camera_id']),
      revision = counter(value['revision']),
      evidenceVersion = counter(value['evidence_version']),
      packetId = value['evidence_packet_id'] == null
          ? null
          : uuid(value['evidence_packet_id']),
      category = text(value, 'category'),
      reason = text(value, 'detection_reason'),
      analysisOrigin = value['analysis_origin'] == null
          ? null
          : AnalysisOrigin.fromJson(object(value['analysis_origin'])),
      possibleContinuationOf = value['possible_continuation_of'] == null
          ? null
          : uuid(object(value['possible_continuation_of'])['incident_id']),
      assessment = text(value, 'assessment'),
      attention = text(value, 'attention'),
      attentionCycle = counter(value['attention_cycle']),
      lifecycle = text(value, 'lifecycle'),
      needsReassessment = value['needs_reassessment'] == true,
      acknowledged = value['acknowledgement'] != null,
      live = value['live'] == true,
      updatedAt = timestamp(value, 'updated_at');

  final String id;
  final String cameraId;
  final String revision;
  final String evidenceVersion;
  final String? packetId;
  final String category;
  final String reason;
  final AnalysisOrigin? analysisOrigin;
  final String? possibleContinuationOf;
  final String assessment;
  final String attention;
  final String attentionCycle;
  final String lifecycle;
  final bool needsReassessment;
  final bool acknowledged;
  final bool live;
  final DateTime updatedAt;
}

final class HealthEvent {
  HealthEvent.fromJson(Json value)
    : sequence = counter(value['sequence']),
      cameraId = value['camera_id'] == null ? null : uuid(value['camera_id']),
      code = text(value, 'code'),
      openedAt = timestamp(value, 'opened_at'),
      endedAt = value['ended_at'] == null ? null : timestamp(value, 'ended_at');

  final String sequence;
  final String? cameraId;
  final String code;
  final DateTime openedAt;
  final DateTime? endedAt;
}

final class AlertPage {
  AlertPage.fromJson(Json value)
    : alerts = List.unmodifiable(objects(value, 'alerts')),
      next = value['next'] == null ? null : text(value, 'next'),
      generation = value['generation'] == null
          ? null
          : uuid(value['generation']),
      head = value['head'] == null ? null : counter(value['head']),
      observedAt = value['observed_at'] == null
          ? null
          : timestamp(value, 'observed_at');

  final List<Json> alerts;
  final String? next;
  final DateTime? observedAt;
  final String? generation;
  final String? head;
}

final class Page<T> {
  const Page(this.items, this.next);
  final List<T> items;
  final String? next;
}

final class Snapshot {
  Snapshot.fromJson(Json value)
    : cursor = Cursor(text(value, 'cursor')),
      hasOlderOpenIncidents = value['has_older_open_incidents'] == true,
      incidents = List.unmodifiable(
        objects(value, 'incidents').map(Incident.fromJson),
      );

  final Cursor cursor;
  final bool hasOlderOpenIncidents;
  final List<Incident> incidents;
}

final class Change {
  Change.fromJson(String id, String event, Json value)
    : cursor = Cursor(id),
      type = text(value, 'type'),
      incident = const ['incident.opened', 'incident.updated'].contains(event)
          ? Incident.fromJson(object(value['payload']))
          : null {
    if (value['schema_version'] != '1.0' ||
        value['stream_generation'] != cursor.generation ||
        value['sequence'] != cursor.sequence ||
        event != type ||
        (incident != null &&
            (incident!.id != value['entity_id'] ||
                incident!.revision != value['entity_revision']))) {
      throw const StillWatchException('protocol');
    }
  }

  final Cursor cursor;
  final String type;
  final Incident? incident;
}

final class StreamGrant {
  StreamGrant.fromJson(Json value)
    : id = uuid(value['id']),
      cameraId = uuid(value['camera_id']),
      sourceEpoch = counter(value['source_epoch']),
      sceneRevision = counter(value['scene_revision']),
      profile = text(value, 'profile'),
      iceServers = value['ice_servers'] == null
          ? const []
          : objects(value, 'ice_servers'),
      expires = timestamp(value, 'expires_at') {
    if (value['codec'] != 'h264' ||
        value['redacted'] != true ||
        value['whep_path'] != path) {
      throw const StillWatchException('protocol');
    }
  }
  final String id;
  final String cameraId;
  final String sourceEpoch;
  final String sceneRevision;
  final String profile;
  final List<Json> iceServers;
  final DateTime expires;

  String get path => '/api/v2/stream-sessions/$id/whep';
}
