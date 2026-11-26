CREATE TABLE core_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    instance_id BLOB NOT NULL CHECK (length(instance_id) = 16),
    stream_generation BLOB NOT NULL CHECK (length(stream_generation) = 16)
) STRICT;

CREATE TABLE incidents (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    camera_id BLOB NOT NULL CHECK (length(camera_id) = 16),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    resolved INTEGER NOT NULL CHECK (resolved IN (0, 1)),
    attention INTEGER NOT NULL CHECK (attention BETWEEN 0 AND 2),
    creation_hash BLOB NOT NULL CHECK (length(creation_hash) = 32),
    state TEXT NOT NULL CHECK (json_valid(state))
) STRICT;
CREATE INDEX incidents_history ON incidents(created_at DESC, id DESC);
CREATE INDEX incidents_camera ON incidents(camera_id, created_at DESC, id DESC);
CREATE INDEX incidents_open ON incidents(created_at DESC, id DESC) WHERE resolved = 0;

CREATE TABLE change_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    incident_id BLOB NOT NULL REFERENCES incidents(id),
    revision INTEGER NOT NULL,
    occurred_at INTEGER NOT NULL,
    opened INTEGER NOT NULL CHECK (opened IN (0, 1)),
    incident TEXT NOT NULL CHECK (json_valid(incident)),
    UNIQUE (incident_id, revision)
) STRICT;

CREATE TABLE operator_actions (
    change_sequence INTEGER PRIMARY KEY,
    incident_id BLOB NOT NULL REFERENCES incidents(id),
    incident_revision INTEGER NOT NULL,
    actor_id BLOB NOT NULL CHECK (length(actor_id) = 16),
    occurred_at INTEGER NOT NULL,
    action TEXT NOT NULL CHECK (json_valid(action))
) STRICT;

CREATE TABLE idempotency_records (
    actor_id BLOB NOT NULL CHECK (length(actor_id) = 16),
    operation TEXT NOT NULL,
    key TEXT NOT NULL,
    request_hash BLOB NOT NULL CHECK (length(request_hash) = 32),
    result TEXT NOT NULL CHECK (json_valid(result)),
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (actor_id, operation, key)
) STRICT;
CREATE INDEX idempotency_expiry ON idempotency_records(expires_at);

CREATE TABLE cameras (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    scene_revision INTEGER NOT NULL CHECK (scene_revision >= 1),
    source_epoch INTEGER NOT NULL CHECK (source_epoch >= 0),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    deleted INTEGER NOT NULL CHECK (deleted IN (0, 1)),
    bytes_reserved INTEGER NOT NULL CHECK (bytes_reserved >= 0),
    config TEXT NOT NULL CHECK (json_valid(config)),
    width INTEGER,
    height INTEGER,
    CHECK ((width IS NULL) = (height IS NULL))
) STRICT;
CREATE INDEX cameras_enabled ON cameras(id) WHERE enabled = 1 AND deleted = 0;

CREATE TABLE camera_config_versions (
    camera_id BLOB NOT NULL REFERENCES cameras(id),
    revision INTEGER NOT NULL,
    actor_id BLOB NOT NULL CHECK (length(actor_id) = 16),
    changed_at INTEGER NOT NULL,
    config TEXT NOT NULL CHECK (json_valid(config)),
    deleted INTEGER NOT NULL CHECK (deleted IN (0, 1)),
    PRIMARY KEY (camera_id, revision)
) STRICT;

CREATE TABLE alert_silences (
    incident_id BLOB NOT NULL REFERENCES incidents(id),
    device_id BLOB NOT NULL CHECK (length(device_id) = 16),
    attention_cycle INTEGER NOT NULL,
    until_utc INTEGER NOT NULL,
    PRIMARY KEY (incident_id, device_id)
) STRICT;

CREATE TABLE notification_outbox (
    id INTEGER PRIMARY KEY,
    incident_id BLOB NOT NULL REFERENCES incidents(id),
    attention_cycle INTEGER NOT NULL CHECK (attention_cycle >= 1),
    attention INTEGER NOT NULL CHECK (attention IN (1, 2)),
    state TEXT NOT NULL CHECK (state IN ('queued', 'expired')),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    next_attempt_at INTEGER NOT NULL,
    UNIQUE (incident_id, attention_cycle)
) STRICT;
CREATE INDEX outbox_due ON notification_outbox(next_attempt_at, id) WHERE state = 'queued';

CREATE TABLE artifacts (
    digest TEXT PRIMARY KEY NOT NULL CHECK (length(digest) = 64),
    byte_length INTEGER NOT NULL CHECK (byte_length > 0),
    available INTEGER NOT NULL CHECK (available IN (0, 1))
) STRICT;

CREATE TABLE evidence_packets (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    incident_id BLOB NOT NULL REFERENCES incidents(id),
    version INTEGER NOT NULL,
    incident_revision INTEGER NOT NULL,
    captured_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    request_hash BLOB NOT NULL CHECK (length(request_hash) = 32),
    UNIQUE (incident_id, version)
) STRICT;

CREATE TABLE evidence_images (
    packet_id BLOB NOT NULL REFERENCES evidence_packets(id),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 15),
    digest TEXT REFERENCES artifacts(digest),
    source_us INTEGER NOT NULL,
    failure TEXT CHECK (failure IN ('quota', 'io')),
    PRIMARY KEY (packet_id, ordinal),
    CHECK ((digest IS NULL) != (failure IS NULL))
) STRICT;
