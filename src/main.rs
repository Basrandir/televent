use frankenstein::AllowedUpdate;
use frankenstein::Api;
use frankenstein::GetUpdatesParams;
use frankenstein::ReplyParameters;
use frankenstein::SendMessageParams;
use frankenstein::TelegramApi;
use frankenstein::UpdateContent;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::Sqlite;
use sqlx::{migrate::MigrateDatabase, SqlitePool};
use std::collections::HashMap;
use std::str::FromStr;

#[derive(Debug, Default)]
pub struct Event {
    pub title: String,
    pub description: String,
    pub location: String,
    pub datetime: String,
}

impl Event {
    pub fn new() -> Self {
        Default::default()
    }
}

#[derive(Debug, PartialEq)]
enum UserState {
    AwaitingName,
    AwaitingDescription,
    AwaitingLocation,
    AwaitingTime,
}

const DB_URL: &str = "sqlite://events_bot.db";

async fn init_db() -> Result<SqlitePool, sqlx::Error> {
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(DB_URL)?.create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await?;

    let _ = sqlx::query(
        "
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  creator INTEGER NOT NULL,
  title TEXT NOT NULL,
  description TEXT,
  location TEXT,
  event_date TEXT NOT NULL,
  chat_id INTEGER, -- NULL if a direct message
  created_at TEXT DEFAULT CURRENT_TIMESTAMP
)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let _ = sqlx::query(
        "
CREATE TABLE IF NOT EXISTS attendees (
  event_id INTEGER NOT NULL,
  user_id INTEGER NOT NULL,
  PRIMARY KEY (event_id, user_id),
  FOREIGN KEY (event_id) REFERENCES events (id)
)",
    )
    .execute(&pool)
    .await
    .unwrap();

    Ok(pool)
}

// This is a supremely ugly function. Need to look into sqlx macros for this.
async fn create_event(
    pool: &SqlitePool,
    creator: i64,
    title: &str,
    description: &str,
    location: &str,
    event_date: &str,
    chat_id: i64,
) -> Result<(), sqlx::Error> {
    let _ = sqlx::query(
        "INSERT INTO events (creator, title, description, location, event_date, chat_id) VALUES (?, ?, ?, ?, ?, ?)",
    )
        .bind(creator)
        .bind(title)
        .bind(description)
        .bind(location)
        .bind(event_date)
        .bind(chat_id)
        .execute(pool)
        .await?;

    Ok(())
}

fn send_message(api: &Api, chat_id: i64, text: &str) {
    let send_message_params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text)
        .build();

    if let Err(err) = api.send_message(&send_message_params) {
        println!("Failed to send message: {err:?}");
    }
}

#[tokio::main]
pub async fn main() {
    let pool = init_db().await.unwrap();
    let token = std::env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
    let api = Api::new(&token.to_string());

    let update_params_builder = GetUpdatesParams::builder();
    let mut update_params = update_params_builder.clone().build();

    let mut user_states: HashMap<u64, UserState> = HashMap::new();
    let mut user_events: HashMap<u64, Event> = HashMap::new();

    loop {
        let result = api.get_updates(&update_params);

        match result {
            Ok(response) => {
                for update in response.result {
                    if let UpdateContent::Message(message) = update.content {
                        // let reply_parameters = ReplyParameters::builder()
                        //     .message_id(message.message_id)
                        //     .build();

                        let user_id = message.from.unwrap().id;
                        let chat_id = message.chat.id;

                        if let Some(text) = message.text {
                            if text == "/start" {
                                user_states.insert(user_id, UserState::AwaitingName);
                                user_events.insert(user_id, Event::new());

                                send_message(&api, chat_id, "Please enter the Name of the event.");
                            } else if let Some(state) = user_states.get(&user_id) {
                                match state {
                                    UserState::AwaitingName => {
                                        if let Some(event) = user_events.get_mut(&user_id) {
                                            event.title = text.clone();
                                            user_states
                                                .insert(user_id, UserState::AwaitingDescription);

                                            send_message(
                                                &api,
                                                chat_id,
                                                "Please enter an Event description.",
                                            );
                                        }
                                    }
                                    UserState::AwaitingDescription => {
                                        if let Some(event) = user_events.get_mut(&user_id) {
                                            event.description = text.clone();
                                            user_states
                                                .insert(user_id, UserState::AwaitingLocation);

                                            send_message(
                                                &api,
                                                chat_id,
                                                "Please enter the Location of the event.",
                                            );
                                        }
                                    }
                                    UserState::AwaitingLocation => {
                                        if let Some(event) = user_events.get_mut(&user_id) {
                                            event.location = text.clone();
                                            user_states.insert(user_id, UserState::AwaitingTime);

                                            send_message(
                                                &api,
                                                chat_id,
                                                "Please enter the Time the event takes place.",
                                            );
                                        }
                                    }
                                    UserState::AwaitingTime => {
                                        if let Some(event) = user_events.get_mut(&user_id) {
                                            event.datetime = text.clone();

                                            match create_event(
                                                &pool,
                                                user_id as i64,
                                                &event.title,
                                                &event.description,
                                                &event.location,
                                                &event.datetime,
                                                chat_id as i64,
                                            )
                                            .await
                                            {
                                                Ok(_) => send_message(
                                                    &api,
                                                    chat_id,
                                                    "The Event has been saved.",
                                                ),
                                                Err(e) => send_message(
                                                    &api,
                                                    chat_id,
                                                    &format!("Failed to save event: {}", e),
                                                ),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    update_params = update_params_builder
                        .clone()
                        .offset(update.update_id + 1)
                        .build();
                }
            }
            Err(_) => {}
        }
    }
}
