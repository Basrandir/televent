use frankenstein::AllowedUpdate;
use frankenstein::Api;
use frankenstein::GetUpdatesParams;
use frankenstein::ReplyParameters;
use frankenstein::SendMessageParams;
use frankenstein::TelegramApi;
use frankenstein::UpdateContent;
use frankenstein::{
    AnswerCallbackQueryParams, CallbackQuery, EditMessageTextParams, InlineKeyboardButton,
    InlineKeyboardMarkup, MessageEntity,
};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::Row;
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

async fn toggle_attendance(
    pool: &SqlitePool,
    event_id: i64,
    user_id: i64,
) -> Result<bool, sqlx::Error> {
    // Check if user is already attending
    let exists = sqlx::query("SELECT 1 FROM attendees WHERE event_id = ? AND user_id = ?")
        .bind(event_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;

    if exists.is_some() {
        // Remove attendance
        sqlx::query("DELETE FROM attendees WHERE event_id = ? AND user_id = ?")
            .bind(event_id)
            .bind(user_id)
            .execute(pool)
            .await?;
        Ok(false)
    } else {
        // Add attendance
        sqlx::query("INSERT INTO attendees (event_id, user_id) VALUES (?, ?)")
            .bind(event_id)
            .bind(user_id)
            .execute(pool)
            .await?;
        Ok(true)
    }
}

async fn list_events(
    api: &Api,
    pool: &SqlitePool,
    chat_id: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    // Query to get events for the chat, ordered by date
    let events = sqlx::query(
        r#"
        SELECT 
            id,
            title,
            description,
            location,
            event_date,
            creator,
            (SELECT COUNT(*) FROM attendees WHERE event_id = events.id) as attendee_count
        FROM events 
        WHERE chat_id = ?
        ORDER BY event_date
        "#,
    )
    .bind(chat_id)
    .fetch_all(pool)
    .await?;

    if events.is_empty() {
        let send_message_params = SendMessageParams::builder()
            .chat_id(chat_id)
            .text("No events scheduled.")
            .build();
        api.send_message(&send_message_params)?;
        return Ok(());
    }

    for row in events {
        let id: i64 = row.get("id");
        let title: String = row.get("title");
        let description: String = row.get("description");
        let location: String = row.get("location");
        let event_date: String = row.get("event_date");
        let attendee_count: i64 = row.get("attendee_count");

        let mut message = String::new();
        message.push_str(&format!("🎯 {}\n", title));
        message.push_str(&format!("📝 {}\n", description));
        message.push_str(&format!("📍 {}\n", location));
        message.push_str(&format!("⏰ {}\n", event_date));
        message.push_str(&format!("👥 {} attending\n", attendee_count));
        message.push_str(&format!("🆔 {}\n", id));

        // Add RSVP buttons for this event
        let accept_button = InlineKeyboardButton::builder()
            .text("✅ Accept")
            .callback_data(format!("rsvp_{}", id))
            .build();

        let decline_button = InlineKeyboardButton::builder()
            .text("❌ Decline")
            .callback_data(format!("cancel_{}", id))
            .build();

        let keyboard = vec![vec![accept_button, decline_button]];
        let inline_keyboard = InlineKeyboardMarkup::builder()
            .inline_keyboard(keyboard)
            .build();

        let send_message_params = SendMessageParams::builder()
            .chat_id(chat_id)
            .text(message)
            .reply_markup(frankenstein::ReplyMarkup::InlineKeyboardMarkup(
                inline_keyboard,
            ))
            .build();

        api.send_message(&send_message_params)?;
    }

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

async fn handle_callback_query(
    api: &Api,
    pool: &SqlitePool,
    callback_query: CallbackQuery,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Received callback query: {:?}", callback_query.data);

    let data = callback_query.data.unwrap_or_default();
    let user_id = callback_query.from.id as i64;

    if data.starts_with("rsvp_") || data.starts_with("cancel_") {
        println!("Processing RSVP/cancel for event");
        let event_id = data[5..].parse::<i64>()?;
        let is_attending = toggle_attendance(pool, event_id, user_id).await?;

        // Answer the callback query
        let answer_text = if is_attending {
            "You're now attending this event!"
        } else {
            "You've cancelled your RSVP."
        };

        let answer_params = AnswerCallbackQueryParams::builder()
            .callback_query_id(callback_query.id)
            .text(answer_text)
            .show_alert(true)
            .build();

        api.answer_callback_query(&answer_params)?;

        // Update just this event's message
        if let Some(message) = callback_query.message {
            let (chat_id, message_id) = match message {
                frankenstein::MaybeInaccessibleMessage::Message(msg) => {
                    (msg.chat.id, msg.message_id)
                }
                frankenstein::MaybeInaccessibleMessage::InaccessibleMessage(_) => {
                    return Ok(());
                }
            };

            // Query just this event's updated information
            let event = sqlx::query(
                r#"
                SELECT 
                    id,
                    title,
                    description,
                    location,
                    event_date,
                    creator,
                    (SELECT COUNT(*) FROM attendees WHERE event_id = events.id) as attendee_count
                FROM events 
                WHERE id = ?
                "#,
            )
            .bind(event_id)
            .fetch_one(pool)
            .await?;

            let mut updated_text = String::new();
            updated_text.push_str(&format!("🎯 {}\n", event.get::<String, _>("title")));
            updated_text.push_str(&format!("📝 {}\n", event.get::<String, _>("description")));
            updated_text.push_str(&format!("📍 {}\n", event.get::<String, _>("location")));
            updated_text.push_str(&format!("⏰ {}\n", event.get::<String, _>("event_date")));
            updated_text.push_str(&format!(
                "👥 {} attending\n",
                event.get::<i64, _>("attendee_count")
            ));
            updated_text.push_str(&format!("🆔 {}", event_id));

            // Recreate the keyboard
            let rsvp_button = InlineKeyboardButton::builder()
                .text("✅ RSVP")
                .callback_data(format!("rsvp_{}", event_id))
                .build();

            let cancel_button = InlineKeyboardButton::builder()
                .text("❌ Cancel RSVP")
                .callback_data(format!("cancel_{}", event_id))
                .build();

            let keyboard = vec![vec![rsvp_button, cancel_button]];
            let inline_keyboard = InlineKeyboardMarkup::builder()
                .inline_keyboard(keyboard)
                .build();

            let edit_params = EditMessageTextParams::builder()
                .chat_id(chat_id)
                .message_id(message_id)
                .text(updated_text)
                .reply_markup(inline_keyboard)
                .build();

            api.edit_message_text(&edit_params)?;
        }
    }

    Ok(())
}

#[tokio::main]
pub async fn main() {
    let pool = init_db().await.unwrap();
    let token = std::env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
    let api = Api::new(&token.to_string());

    let update_params_builder = GetUpdatesParams::builder()
        .allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::CallbackQuery]);
    let mut update_params = update_params_builder.clone().build();

    let mut user_states: HashMap<u64, UserState> = HashMap::new();
    let mut user_events: HashMap<u64, Event> = HashMap::new();

    loop {
        let result = api.get_updates(&update_params);

        match result {
            Ok(response) => {
                for update in response.result {
                    if let UpdateContent::CallbackQuery(callback_query) = update.content {
                        println!("Received callback update");
                        if let Err(e) = handle_callback_query(&api, &pool, callback_query).await {
                            println!("Error handling callback query: {:?}", e);
                        }
                    } else if let UpdateContent::Message(message) = update.content {
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
                            } else if text == "/list" {
                                if let Err(e) = list_events(&api, &pool, chat_id).await {
                                    send_message(
                                        &api,
                                        chat_id,
                                        &format!("Failed to list events: {}", e),
                                    );
                                }
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
