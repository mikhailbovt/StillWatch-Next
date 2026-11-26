ALTER TABLE evidence_images ADD COLUMN retired INTEGER NOT NULL DEFAULT 0
    CHECK (retired IN (0, 1));
CREATE INDEX evidence_retention ON evidence_packets(captured_at, id);
CREATE INDEX evidence_active_references ON evidence_images(digest)
    WHERE retired = 0 AND digest IS NOT NULL;
CREATE TABLE retired_blob_cleanup (
    digest TEXT PRIMARY KEY REFERENCES artifacts(digest)
) STRICT;
