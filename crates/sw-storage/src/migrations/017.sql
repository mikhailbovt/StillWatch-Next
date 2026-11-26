CREATE TABLE ios_push_registrations (
    device_id BLOB PRIMARY KEY REFERENCES trusted_devices(id),
    token_revision BLOB NOT NULL CHECK (length(token_revision) = 16),
    token_digest BLOB NOT NULL CHECK (length(token_digest) = 32),
    encrypted_token BLOB NOT NULL CHECK (length(encrypted_token) BETWEEN 30 AND 540),
    registered_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE ios_push_deliveries (
    notice BLOB PRIMARY KEY CHECK (length(notice) = 16),
    device_id BLOB NOT NULL REFERENCES ios_push_registrations(device_id) ON DELETE CASCADE,
    token_revision BLOB NOT NULL CHECK (length(token_revision) = 16),
    alert_id INTEGER NOT NULL REFERENCES notification_outbox(id) ON DELETE CASCADE,
    attempts INTEGER NOT NULL CHECK (attempts BETWEEN 1 AND 5),
    next_attempt_at INTEGER NOT NULL,
    provider_accepted_at INTEGER,
    UNIQUE (device_id, alert_id)
) STRICT;
CREATE INDEX ios_push_deliveries_by_alert ON ios_push_deliveries(alert_id);

CREATE TRIGGER revoke_ios_push AFTER UPDATE OF revoked_at ON trusted_devices
WHEN NEW.revoked_at IS NOT NULL
BEGIN
    DELETE FROM ios_push_registrations WHERE device_id = NEW.id;
END;
