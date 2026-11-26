CREATE INDEX pairing_grants_expiry ON pairing_grants(expires_at);
CREATE INDEX device_sessions_refresh_expiry ON device_sessions(refresh_expiry);
CREATE INDEX retired_refresh_device ON retired_refresh_credentials(device_id);
CREATE INDEX alert_silences_expiry ON alert_silences(until_utc);
CREATE INDEX queued_outbox_expiry ON notification_outbox(expires_at, id) WHERE state = 'queued';
CREATE INDEX expired_outbox_retention ON notification_outbox(expires_at, id) WHERE state = 'expired';
ALTER TABLE evidence_packets ADD COLUMN has_retirable_images INTEGER NOT NULL DEFAULT 0
    CHECK (has_retirable_images IN (0, 1));
UPDATE evidence_packets SET has_retirable_images = 1 WHERE EXISTS (
    SELECT 1 FROM evidence_images
    WHERE packet_id = evidence_packets.id AND retired = 0 AND digest IS NOT NULL
);
DROP INDEX evidence_retention;
CREATE INDEX evidence_retention ON evidence_packets(captured_at, id) WHERE has_retirable_images = 1;
CREATE INDEX incident_active_evidence ON evidence_packets(incident_id) WHERE has_retirable_images = 1;
