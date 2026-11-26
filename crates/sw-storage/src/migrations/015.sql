CREATE TABLE notification_sequence (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    head INTEGER NOT NULL CHECK (head >= 0)
) STRICT;
INSERT INTO notification_sequence VALUES (1, coalesce((SELECT max(id) FROM notification_outbox), 0));
CREATE TRIGGER advance_notification_sequence AFTER INSERT ON notification_outbox
WHEN NEW.id > (SELECT head FROM notification_sequence WHERE singleton = 1)
BEGIN
    UPDATE notification_sequence SET head = NEW.id WHERE singleton = 1;
END;
CREATE INDEX queued_notification_order ON notification_outbox(attention DESC, id)
    WHERE state = 'queued';
