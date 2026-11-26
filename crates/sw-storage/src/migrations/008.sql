CREATE TABLE health_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id BLOB CHECK (camera_id IS NULL OR length(camera_id) = 16),
    code TEXT NOT NULL,
    opened_at INTEGER NOT NULL,
    ended_at INTEGER CHECK (ended_at >= opened_at)
) STRICT;

CREATE UNIQUE INDEX health_events_open_key ON health_events(ifnull(camera_id, X''), code)
    WHERE ended_at IS NULL;
CREATE INDEX health_events_active_order ON health_events(sequence) WHERE ended_at IS NULL;
CREATE INDEX health_events_retention ON health_events(ended_at, sequence) WHERE ended_at IS NOT NULL;
