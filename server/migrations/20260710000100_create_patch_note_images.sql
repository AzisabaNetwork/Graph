CREATE TABLE patch_note_images (
    patch_note_id BINARY(16) NOT NULL,
    position INT UNSIGNED NOT NULL,
    object_key VARCHAR(1024) NOT NULL,
    url TEXT NOT NULL,
    content_type VARCHAR(255) NULL,
    created_at DATETIME(6) NOT NULL,
    PRIMARY KEY (patch_note_id, position),
    CONSTRAINT patch_note_images_patch_note_id_fk
        FOREIGN KEY (patch_note_id)
        REFERENCES patch_notes (id)
        ON DELETE CASCADE
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
