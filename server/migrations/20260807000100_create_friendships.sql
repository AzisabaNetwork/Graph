CREATE TABLE friendships (
    player1_id BINARY(16) NOT NULL,
    player2_id BINARY(16) NOT NULL,
    created_at DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
    PRIMARY KEY (player1_id, player2_id),
    CONSTRAINT friendships_player_order_check CHECK (
        player1_id < player2_id
    )
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;

CREATE INDEX friendships_player2_id_idx
    ON friendships (player2_id);
