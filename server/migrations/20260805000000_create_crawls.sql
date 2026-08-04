CREATE TABLE crawls (
    id BINARY(16) NOT NULL,
    address VARCHAR(255) NOT NULL,
    port SMALLINT UNSIGNED NOT NULL,
    ping INT UNSIGNED NOT NULL,
    version TEXT NOT NULL,
    protocol_version INT NOT NULL,
    max_players INT UNSIGNED NOT NULL,
    online_players INT UNSIGNED NOT NULL,
    description TEXT NULL,
    favicon MEDIUMTEXT NULL,
    crawled_at DATETIME(6) NOT NULL,
    PRIMARY KEY (id),
    CONSTRAINT crawls_port_check CHECK (port >= 1)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

CREATE INDEX crawls_crawled_at_id_idx
    ON crawls (crawled_at DESC, id DESC);

CREATE INDEX crawls_address_port_crawled_at_id_idx
    ON crawls (address, port, crawled_at DESC, id DESC);

ALTER TABLE api_key_scopes
    DROP CONSTRAINT api_key_scopes_scope_check;

ALTER TABLE api_key_scopes
    ADD CONSTRAINT api_key_scopes_scope_check CHECK (
        scope IN (
            '*',
            'api-keys:read',
            'api-keys:write',
            'crawls:read',
            'crawls:write',
            'patch-notes:read',
            'patch-notes:write',
            'players:read',
            'players:read-details',
            'players:write',
            'punishments:read',
            'punishments:write'
        )
    );
