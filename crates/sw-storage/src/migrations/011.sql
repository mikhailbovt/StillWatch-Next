CREATE TABLE hazard_observations (
    incident_id BLOB NOT NULL REFERENCES incidents(id),
    source_us INTEGER NOT NULL,
    observed_host_utc INTEGER NOT NULL,
    facts TEXT NOT NULL CHECK (json_valid(facts)),
    PRIMARY KEY (incident_id, source_us)
) STRICT, WITHOUT ROWID;
