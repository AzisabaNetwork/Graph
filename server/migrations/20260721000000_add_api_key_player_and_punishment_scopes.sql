CREATE TABLE api_key_players (
    api_key_public_id VARCHAR(64) NOT NULL,
    player_id BINARY(16) NOT NULL,
    PRIMARY KEY (api_key_public_id),
    KEY api_key_players_player_id_idx (player_id),
    CONSTRAINT api_key_players_api_key_fk
        FOREIGN KEY (api_key_public_id)
        REFERENCES api_keys (public_id)
        ON DELETE CASCADE,
    CONSTRAINT api_key_players_player_fk
        FOREIGN KEY (player_id)
        REFERENCES players (id)
        ON DELETE CASCADE
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

ALTER TABLE api_key_scopes
    DROP CONSTRAINT api_key_scopes_scope_check;

ALTER TABLE api_key_scopes
    ADD CONSTRAINT api_key_scopes_scope_check CHECK (
        scope IN (
            '*',
            'api-keys:read',
            'api-keys:write',
            'patch-notes:read',
            'patch-notes:write',
            'players:read',
            'players:read-details',
            'players:write',
            'punishments:read',
            'punishments:write'
        )
    );
