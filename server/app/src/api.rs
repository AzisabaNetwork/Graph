use sqlx::MySqlPool;

pub mod patch_notes;

#[derive(Clone, Debug)]
pub struct Api {
    pool: MySqlPool,
}

impl Api {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}
