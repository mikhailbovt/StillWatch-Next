CREATE TABLE notification_receipts (
    alert_id INTEGER NOT NULL REFERENCES notification_outbox(id) ON DELETE CASCADE,
    device_id BLOB NOT NULL CHECK (length(device_id) = 16),
    client_seen_at INTEGER NOT NULL,
    PRIMARY KEY (alert_id, device_id)
) STRICT;
