CREATE TABLE api_keys (
    public_id VARCHAR(64) NOT NULL,
    created_by_public_id VARCHAR(64) NULL,
    name VARCHAR(100) NOT NULL,
    secret_digest BINARY(32) NOT NULL,
    created_at DATETIME(6) NOT NULL,
    expires_at DATETIME(6) NULL,
    PRIMARY KEY (public_id),
    UNIQUE KEY api_keys_secret_digest_unique (secret_digest),
    KEY api_keys_created_by_public_id_idx (created_by_public_id),
    CONSTRAINT api_keys_created_by_fk
        FOREIGN KEY (created_by_public_id)
        REFERENCES api_keys (public_id)
        ON DELETE RESTRICT
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

CREATE INDEX api_keys_created_at_public_id_idx
    ON api_keys (created_at DESC, public_id DESC);

CREATE TABLE api_key_scopes (
    api_key_public_id VARCHAR(64) NOT NULL,
    scope VARCHAR(32) NOT NULL,
    PRIMARY KEY (api_key_public_id, scope),
    CONSTRAINT api_key_scopes_api_key_fk
        FOREIGN KEY (api_key_public_id)
        REFERENCES api_keys (public_id)
        ON DELETE CASCADE,
    CONSTRAINT api_key_scopes_scope_check CHECK (
        scope IN (
            '*',
            'api-keys:read',
            'api-keys:write',
            'patch-notes:read',
            'patch-notes:write'
        )
    )
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
