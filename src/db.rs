use sqlx::mysql::{MySqlPool, MySqlPoolOptions};

pub type DbPool = MySqlPool;

pub async fn connect(database_url: &str) -> Result<DbPool, sqlx::Error> {
    MySqlPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
}
