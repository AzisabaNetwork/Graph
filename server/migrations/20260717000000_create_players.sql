CREATE TABLE players (
    id BINARY(16) NOT NULL,
    discord_id VARCHAR(20) NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'offline',
    current_server VARCHAR(255) NULL,
    bio VARCHAR(160) NULL,
    PRIMARY KEY (id),
    CONSTRAINT players_status_check CHECK (status IN ('online', 'offline'))
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

CREATE INDEX players_discord_id_idx
    ON players (discord_id);

CREATE INDEX players_status_id_idx
    ON players (status, id);

ALTER TABLE patch_notes
    ADD COLUMN author_id BINARY(16) NULL AFTER body,
    ADD CONSTRAINT patch_notes_author_id_fk
        FOREIGN KEY (author_id)
        REFERENCES players (id)
        ON DELETE SET NULL;
