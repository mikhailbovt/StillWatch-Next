ALTER TABLE hazard_observations ADD COLUMN scene_signature BLOB
    CHECK (scene_signature IS NULL OR length(scene_signature) = 48);
CREATE INDEX camera_open_hazards ON incidents(camera_id, updated_at DESC, id)
    WHERE resolved = 0 AND camera_hazard = 1;
