ALTER TABLE cameras ADD COLUMN source_measurements TEXT
    CHECK (source_measurements IS NULL OR json_valid(source_measurements));
