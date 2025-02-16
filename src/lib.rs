pub mod bot;
pub mod db;
pub mod error;
pub mod event;
use crate::error::BotError;
use std::env;

pub async fn run() -> Result<(), BotError> {
    let token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
    let db_pool = db::init_db().await?;
    let mut bot = bot::Bot::new(&token, db_pool).await?;
    bot.run().await
}
