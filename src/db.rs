use std::str::FromStr;

use crate::error::BotError;
use sqlx::sqlite::SqlitePool;

pub async fn init_db() -> Result<SqlitePool, BotError> {
    const DB_URL: &str = "sqlite://events_bot.db";
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(DB_URL)?.create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await?;

    sqlx::query(
        r#"
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                creator INTEGER NOT NULL,
                title TEXT NOT NULL,
                description TEXT,
                location TEXT,
                event_date TEXT NOT NULL,
                chat_id INTEGER,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            )
            "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
            CREATE TABLE IF NOT EXISTS attendees (
                event_id INTEGER NOT NULL,
                user_id INTEGER NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('accepted', 'declined')),
                PRIMARY KEY (event_id, user_id),
                FOREIGN KEY (event_id) REFERENCES events (id)
            )
            "#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}
