CREATE TABLE opening_reviews (
    incident_id BLOB PRIMARY KEY REFERENCES incidents(id) ON DELETE CASCADE,
    packet_id BLOB UNIQUE REFERENCES evidence_packets(id) ON DELETE CASCADE,
    deadline INTEGER NOT NULL
) STRICT;
