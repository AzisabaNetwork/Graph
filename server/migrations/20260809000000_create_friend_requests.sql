CREATE TABLE friend_requests (
    player_id BINARY(16) NOT NULL,
    sender_id BINARY(16) NOT NULL,
    created_at DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
    PRIMARY KEY (player_id, sender_id),
    CONSTRAINT friend_requests_distinct_players_check CHECK (
        player_id <> sender_id
    )
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

CREATE INDEX friend_requests_sender_id_idx
    ON friend_requests (sender_id);
