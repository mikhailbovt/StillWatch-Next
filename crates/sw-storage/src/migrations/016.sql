CREATE TABLE refresh_receipts (
    device_id BLOB PRIMARY KEY REFERENCES device_sessions(device_id) ON DELETE CASCADE,
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    response_refresh_hash BLOB NOT NULL CHECK (length(response_refresh_hash) = 32),
    envelope BLOB NOT NULL CHECK (length(envelope) BETWEEN 28 AND 1024),
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX refresh_receipts_expiry ON refresh_receipts(expires_at);
ALTER TABLE retired_refresh_credentials ADD COLUMN request_digest BLOB
    CHECK (request_digest IS NULL OR length(request_digest) = 32);
