ALTER TABLE evidence_packets ADD COLUMN observed_host_utc INTEGER;
ALTER TABLE evidence_packets ADD COLUMN persisted_at INTEGER;
ALTER TABLE evidence_packets ADD COLUMN semantic_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (semantic_eligible IN (0, 1));

UPDATE evidence_packets SET persisted_at = captured_at;
