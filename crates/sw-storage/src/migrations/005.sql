CREATE TABLE legacy_incidents (
    incident_id BLOB PRIMARY KEY REFERENCES incidents(id),
    camera_label TEXT NOT NULL,
    original_body TEXT NOT NULL CHECK (json_valid(original_body))
) STRICT;

CREATE TABLE legacy_assessments (
    id INTEGER PRIMARY KEY,
    incident_id BLOB NOT NULL REFERENCES legacy_incidents(incident_id),
    created_at INTEGER NOT NULL,
    legacy_provider TEXT,
    packet TEXT NOT NULL CHECK (json_valid(packet)),
    result TEXT NOT NULL CHECK (json_valid(result))
) STRICT;

CREATE TABLE legacy_feedback (
    id INTEGER PRIMARY KEY,
    incident_id BLOB NOT NULL REFERENCES legacy_incidents(incident_id),
    created_at INTEGER NOT NULL,
    operator TEXT NOT NULL,
    action TEXT NOT NULL
) STRICT;

CREATE TABLE legacy_packets (
    packet_id BLOB PRIMARY KEY CHECK (length(packet_id) = 16),
    incident_id BLOB NOT NULL REFERENCES legacy_incidents(incident_id),
    created_at INTEGER NOT NULL,
    metadata TEXT NOT NULL CHECK (json_valid(metadata)),
    request_hash BLOB NOT NULL CHECK (length(request_hash) = 32)
) STRICT;
