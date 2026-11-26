ALTER TABLE cameras ADD COLUMN reviewed_scene BLOB
    CHECK (reviewed_scene IS NULL OR length(reviewed_scene) = 48);
ALTER TABLE evidence_packets ADD COLUMN context TEXT
    CHECK (context IS NULL OR json_valid(context));
ALTER TABLE evidence_images ADD COLUMN kind TEXT NOT NULL DEFAULT 'overview'
    CHECK (kind IN ('overview', 'crop'));
ALTER TABLE incidents ADD COLUMN camera_hazard INTEGER NOT NULL DEFAULT 0
    CHECK (camera_hazard IN (0, 1));

CREATE TABLE semantic_attempts (
    packet_id BLOB NOT NULL REFERENCES evidence_packets(id),
    route TEXT NOT NULL CHECK (route IN ('cloud_only', 'local_only', 'prefer_local')),
    incident_id BLOB NOT NULL REFERENCES incidents(id),
    camera_id BLOB NOT NULL REFERENCES cameras(id),
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    outcome TEXT CHECK (outcome IN ('applied', 'stale', 'unavailable', 'invalid', 'deadline')),
    report TEXT CHECK (report IS NULL OR json_valid(report)),
    PRIMARY KEY (packet_id, route)
) STRICT;
CREATE INDEX semantic_attempts_day ON semantic_attempts(started_at);
CREATE INDEX semantic_attempts_camera ON semantic_attempts(camera_id, started_at);
