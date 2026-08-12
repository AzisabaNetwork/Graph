ALTER TABLE players
    ADD COLUMN current_locale VARCHAR(255) NULL AFTER current_server,
    ADD COLUMN current_client_version VARCHAR(255) NULL AFTER current_locale,
    ADD COLUMN first_login_at DATETIME(6) NULL AFTER bio,
    ADD COLUMN last_seen_at DATETIME(6) NULL AFTER first_login_at;

CREATE INDEX players_current_server_id_idx
    ON players (current_server, id);

CREATE INDEX players_current_locale_id_idx
    ON players (current_locale, id);

CREATE INDEX players_current_client_version_id_idx
    ON players (current_client_version, id);

CREATE INDEX players_first_login_at_id_idx
    ON players (first_login_at, id);

CREATE INDEX players_last_seen_at_id_idx
    ON players (last_seen_at, id);
