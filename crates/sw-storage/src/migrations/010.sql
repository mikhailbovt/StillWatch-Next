CREATE TABLE semantic_jobs (
    packet_id BLOB PRIMARY KEY REFERENCES evidence_packets(id),
    request_id BLOB NOT NULL UNIQUE CHECK (length(request_id) = 16),
    state TEXT NOT NULL CHECK (state IN (
        'queued', 'claimed', 'dispatched', 'response_ready', 'completed', 'expired', 'unknown_outcome'
    )),
    lease_owner BLOB CHECK (lease_owner IS NULL OR length(lease_owner) = 16),
    lease_token BLOB CHECK (lease_token IS NULL OR length(lease_token) = 16),
    lease_until INTEGER,
    dispatch_until INTEGER,
    route TEXT CHECK (route IS NULL OR route IN ('cloud_only', 'local_only', 'prefer_local')),
    projected_assessment TEXT CHECK (projected_assessment IS NULL OR projected_assessment IN (
        'unknown', 'no_visible_concern', 'concern', 'immediate_visible_concern'
    )),
    CHECK ((state = 'claimed') = (lease_until IS NOT NULL)),
    CHECK ((state = 'dispatched') = (dispatch_until IS NOT NULL))
) STRICT;
CREATE INDEX semantic_jobs_active ON semantic_jobs(state, lease_until)
    WHERE state IN ('queued', 'claimed', 'dispatched', 'response_ready');
CREATE INDEX semantic_jobs_dispatched_deadline ON semantic_jobs(dispatch_until, packet_id)
    WHERE state = 'dispatched';
