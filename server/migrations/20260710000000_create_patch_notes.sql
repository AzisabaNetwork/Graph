CREATE TABLE patch_notes (
    id BINARY(16) NOT NULL,
    target VARCHAR(32) NOT NULL,
    category VARCHAR(32) NOT NULL,
    title VARCHAR(255) NOT NULL,
    body TEXT NOT NULL,
    created_at DATETIME(6) NOT NULL,
    PRIMARY KEY (id),
    CONSTRAINT patch_notes_target_check CHECK (
        target IN (
            'creativePro',
            'frontier',
            'life',
            'leonGunWar2',
            'sclat'
        )
    ),
    CONSTRAINT patch_notes_category_check CHECK (
        category IN (
            'balance',
            'feature',
            'fix',
            'improvement'
        )
    )
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

CREATE INDEX patch_notes_created_at_id_idx
    ON patch_notes (created_at DESC, id DESC);

CREATE INDEX patch_notes_target_created_at_id_idx
    ON patch_notes (target, created_at DESC, id DESC);

CREATE INDEX patch_notes_category_created_at_id_idx
    ON patch_notes (category, created_at DESC, id DESC);

CREATE INDEX patch_notes_target_category_created_at_id_idx
    ON patch_notes (target, category, created_at DESC, id DESC);
