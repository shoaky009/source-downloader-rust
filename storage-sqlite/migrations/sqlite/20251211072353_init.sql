CREATE TABLE IF NOT EXISTS processing_record
(
    id             INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    processor_name VARCHAR(255)                      NOT NULL,
    item_hash      VARCHAR(64)                       NOT NULL,
    item_identity  VARCHAR(256)                               DEFAULT NULL,
    item_content   JSON                              NOT NULL,
    rename_times   INT                               NOT NULL DEFAULT 0,
    status         INT                               NOT NULL,
    failure_reason TEXT,
    summary        JSON,
    created_at     DATETIME                          NOT NULL,
    updated_at     DATETIME                                   DEFAULT NULL
);
CREATE UNIQUE INDEX uidx_processorname_itemhash ON processing_record (processor_name, item_hash);
CREATE INDEX idx_itemhash ON processing_record (item_hash);
CREATE INDEX idx_processorname_status ON processing_record (processor_name, status);
CREATE INDEX idx_processorname_id ON processing_record (processor_name, id DESC);
CREATE INDEX idx_createdat on processing_record (created_at desc);
CREATE INDEX idx_processorname_itemidentity ON processing_record (processor_name, item_identity);

CREATE TABLE IF NOT EXISTS processor_source_state
(
    id                INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    processor_name    VARCHAR(255)                      NOT NULL,
    source_id         VARCHAR(64)                       NOT NULL,
    last_pointer_json JSON                              NOT NULL,
    retry_times       INTEGER                           NOT NULL DEFAULT 0,
    last_active_at    DATETIME
);
CREATE UNIQUE INDEX uidx_processorname_sourceid ON processor_source_state (processor_name, source_id);

CREATE TABLE IF NOT EXISTS target_path
(
    id             CHARACTER VARYING PRIMARY KEY,
    processor_name TEXT     NOT NULL,
    item_hash      TEXT     NOT NULL,
    created_at     DATETIME NOT NULL
);

CREATE TABLE IF NOT EXISTS item_file_content
(
    -- processing_content.id 1 to 1
    id           INTEGER PRIMARY KEY,
    -- all file contents of the processing_content, JSON Array compressed
    file_content BLOB
)