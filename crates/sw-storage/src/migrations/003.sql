CREATE TABLE pairing_grants (
    challenge BLOB PRIMARY KEY CHECK (length(challenge) = 16),
    secret_hash BLOB NOT NULL CHECK (length(secret_hash) = 32),
    permissions TEXT NOT NULL CHECK (json_valid(permissions)),
    issued_by BLOB NOT NULL CHECK (length(issued_by) = 16),
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE trusted_devices (
    id BLOB PRIMARY KEY CHECK (length(id) = 16),
    public_key BLOB NOT NULL CHECK (length(public_key) = 32),
    label TEXT NOT NULL,
    permissions TEXT NOT NULL CHECK (json_valid(permissions)),
    paired_at INTEGER NOT NULL,
    revoked_at INTEGER
) STRICT;
CREATE UNIQUE INDEX unique_active_public_key ON trusted_devices(public_key) WHERE revoked_at IS NULL;

CREATE TABLE device_sessions (
    device_id BLOB PRIMARY KEY REFERENCES trusted_devices(id),
    access_hash BLOB NOT NULL UNIQUE CHECK (length(access_hash) = 32),
    access_expiry INTEGER NOT NULL,
    refresh_hash BLOB NOT NULL UNIQUE CHECK (length(refresh_hash) = 32),
    refresh_expiry INTEGER NOT NULL
) STRICT;

CREATE TABLE retired_refresh_credentials (
    digest BLOB PRIMARY KEY CHECK (length(digest) = 32),
    device_id BLOB NOT NULL REFERENCES trusted_devices(id),
    expires_at INTEGER NOT NULL
) STRICT;
CREATE INDEX retired_refresh_expiry ON retired_refresh_credentials(expires_at);

CREATE TABLE trust_audit (
    sequence INTEGER PRIMARY KEY,
    actor_id BLOB NOT NULL CHECK (length(actor_id) = 16),
    device_id BLOB CHECK (device_id IS NULL OR length(device_id) = 16),
    operation TEXT NOT NULL CHECK (operation IN ('pairing_grant', 'enrolled', 'revoked', 'refresh_reuse')),
    occurred_at INTEGER NOT NULL
) STRICT;
