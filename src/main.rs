use frankenstein::{
    AllowedUpdate, AnswerCallbackQueryParams, Api, CallbackQuery, EditMessageTextParams,
    GetUpdatesParams, InlineKeyboardButton, InlineKeyboardMarkup, MaybeInaccessibleMessage,
    Message, ReplyMarkup, SendMessageParams, TelegramApi, UpdateContent,
};
use sqlx::{Row, SqlitePool};
use std::{collections::HashMap, fmt, str::FromStr};

/// Custom error type for operations
#[derive(Debug)]
pub enum BotError {
    Database(sqlx::Error),
    Telegram(frankenstein::Error),
    Parse(std::num::ParseIntError),
    MissingDraft,
}

// Implement std:fmt::Display for BotError
impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BotError::Database(e) => write!(f, "Database error: {}", e),
            BotError::Telegram(e) => write!(f, "Telegram API error: {}", e),
            BotError::Parse(e) => write!(f, "Failed to parse data: {}", e),
            BotError::MissingDraft => write!(f, "Event draft not found"),
        }
    }
}

// Implement std::error::Error for BotError
impl std::error::Error for BotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BotError::Database(e) => Some(e),
            BotError::Telegram(e) => Some(e),
            BotError::Parse(e) => Some(e),
            BotError::MissingDraft => None,
        }
    }
}

// Implement From for each error type
impl From<sqlx::Error> for BotError {
    fn from(err: sqlx::Error) -> Self {
        BotError::Database(err)
    }
}

impl From<frankenstein::Error> for BotError {
    fn from(err: frankenstein::Error) -> Self {
        BotError::Telegram(err)
    }
}

impl From<std::num::ParseIntError> for BotError {
    fn from(err: std::num::ParseIntError) -> Self {
        BotError::Parse(err)
    }
}

/// Represents the state of event creation for a user
#[derive(Debug, Clone, PartialEq)]
enum EventCreationState {
    AwaitingTitle,
    AwaitingDescription,
    AwaitingLocation,
    AwaitingTime,
}

/// Represents an event being created
#[derive(Clone, Debug, Default)]
struct EventDraft {
    title: String,
    description: String,
    location: String,
    datetime: String,
}

/// Represents a fully formed event from the database
#[derive(Debug)]
struct Event {
    id: i64,
    title: String,
    description: String,
    location: String,
    event_date: String,
    creator: i64,
    attendee_count: i64,
}

impl Event {
    /// Creates a formatted message for Telegram display
    fn format_message(&self) -> String {
        format!(
            "🎯 {}\n📝 {}\n📍 {}\n⏰ {}\n👥 {} attending\n🆔 {}",
            self.title,
            self.description,
            self.location,
            self.event_date,
            self.attendee_count,
            self.id
        )
    }

    /// Creates RSVP keyboard buttons for this event
    fn create_keyboard(&self) -> InlineKeyboardMarkup {
        let accept_button = InlineKeyboardButton::builder()
            .text("✅ Accept")
            .callback_data(format!("accept_{}", self.id))
            .build();

        let decline_button = InlineKeyboardButton::builder()
            .text("❌ Decline")
            .callback_data(format!("decline_{}", self.id))
            .build();

        InlineKeyboardMarkup::builder()
            .inline_keyboard(vec![vec![accept_button, decline_button]])
            .build()
    }

    /// Loads an event from a database row
    fn from_row(row: sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.get("id"),
            title: row.get("title"),
            description: row.get("description"),
            location: row.get("location"),
            event_date: row.get("event_date"),
            creator: row.get("creator"),
            attendee_count: row.get("attendee_count"),
        })
    }
}

/// Manages the bots state and operations
struct Bot {
    api: Api,
    pool: SqlitePool,
    user_states: HashMap<i64, EventCreationState>,
    user_drafts: HashMap<i64, EventDraft>,
}

impl Bot {
    /// Creates a new Televent instance
    async fn new(token: &str) -> Result<Self, BotError> {
        const DB_URL: &str = "sqlite://events_bot.db";
        let pool = Self::init_db(DB_URL).await?;

        Ok(Self {
            api: Api::new(token),
            pool,
            user_states: HashMap::new(),
            user_drafts: HashMap::new(),
        })
    }

    /// Initializes the database with required tables
    async fn init_db(url: &str) -> Result<SqlitePool, sqlx::Error> {
        let options = sqlx::sqlite::SqliteConnectOptions::from_str(url)?.create_if_missing(true);
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
                PRIMARY KEY (event_id, user_id),
                FOREIGN KEY (event_id) REFERENCES events (id)
            )
            "#,
        )
        .execute(&pool)
        .await?;

        Ok(pool)
    }

    /// Sends a message to a chat
    async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), BotError> {
        let params = SendMessageParams::builder()
            .chat_id(chat_id)
            .text(text)
            .build();
        self.api.send_message(&params)?;
        Ok(())
    }

    /// Handles an incoming message
    async fn handle_message(&mut self, message: Message) -> Result<(), BotError> {
        let user_id = message
            .from
            .as_ref()
            .map(|user| user.id as i64)
            .unwrap_or_default();
        let chat_id = message.chat.id;

        let text = match message.text {
            Some(text) => text,
            None => return Ok(()),
        };

        match text.as_str() {
            "/start" => self.handle_start(user_id, chat_id).await?,
            "/list" => self.list_events(chat_id).await?,
            _ => self.handle_event_creation(user_id, chat_id, &text).await?,
        }

        Ok(())
    }

    /// Handles the /start command
    async fn handle_start(&mut self, user_id: i64, chat_id: i64) -> Result<(), BotError> {
        self.user_states
            .insert(user_id, EventCreationState::AwaitingTitle);
        self.user_drafts.insert(user_id, EventDraft::default());
        self.send_message(chat_id, "Please enter the Title of the event.")
            .await?;
        Ok(())
    }

    /// Handles event creation state machine
    async fn handle_event_creation(
        &mut self,
        user_id: i64,
        chat_id: i64,
        text: &str,
    ) -> Result<(), BotError> {
        let state = match self.user_states.get(&user_id) {
            Some(state) => state.clone(),
            None => return Ok(()),
        };

        // Update draft first
        if let Some(draft) = self.user_drafts.get_mut(&user_id) {
            match state {
                EventCreationState::AwaitingTitle => draft.title = text.to_string(),
                EventCreationState::AwaitingDescription => draft.description = text.to_string(),
                EventCreationState::AwaitingLocation => draft.location = text.to_string(),
                EventCreationState::AwaitingTime => draft.datetime = text.to_string(),
            }
        }

        // Then handle state transition and messages
        match state {
            EventCreationState::AwaitingTitle => {
                self.user_states
                    .insert(user_id, EventCreationState::AwaitingDescription);
                self.send_message(chat_id, "Please enter an Event description.")
                    .await?;
            }
            EventCreationState::AwaitingDescription => {
                self.user_states
                    .insert(user_id, EventCreationState::AwaitingLocation);
                self.send_message(chat_id, "Please enter the Location of the event.")
                    .await?;
            }
            EventCreationState::AwaitingLocation => {
                self.user_states
                    .insert(user_id, EventCreationState::AwaitingTime);
                self.send_message(chat_id, "Please enter the Time the event takes place.")
                    .await?;
            }
            EventCreationState::AwaitingTime => {
                let draft = self
                    .user_drafts
                    .get(&user_id)
                    .cloned()
                    .ok_or_else(|| BotError::MissingDraft)?;

                self.create_event(user_id, chat_id, &draft).await?;
                self.user_states.remove(&user_id);
                self.user_drafts.remove(&user_id);
            }
        }

        Ok(())
    }

    /// Creates a new event in the database
    async fn create_event(
        &self,
        creator: i64,
        chat_id: i64,
        draft: &EventDraft,
    ) -> Result<(), BotError> {
        let event_id = sqlx::query(
            r#"
            INSERT INTO events (creator, title, description, location, event_date, chat_id)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(creator)
        .bind(&draft.title)
        .bind(&draft.description)
        .bind(&draft.location)
        .bind(&draft.datetime)
        .bind(chat_id)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        // Fetch and display the event
        let event = self.fetch_event(event_id).await?;

        self.send_message(chat_id, "The Event has been saved.")
            .await?;
        self.list_event(chat_id, &event).await?;

        Ok(())
    }

    /// List a single event in chat with RSVP buttons
    async fn list_event(&self, chat_id: i64, event: &Event) -> Result<(), BotError> {
        let params = SendMessageParams::builder()
            .chat_id(chat_id)
            .text(event.format_message())
            .reply_markup(ReplyMarkup::InlineKeyboardMarkup(event.create_keyboard()))
            .build();

        self.api.send_message(&params)?;
        Ok(())
    }

    /// Lists all events in a chat
    async fn list_events(&self, chat_id: i64) -> Result<(), BotError> {
        let events = self.fetch_events(chat_id).await?;

        if events.is_empty() {
            self.send_message(chat_id, "No events scheduled.").await?;
            return Ok(());
        }

        for event in events {
            self.list_event(chat_id, &event).await?;
        }

        Ok(())
    }

    /// Fetches a single event by ID with its attendee count
    async fn fetch_event(&self, event_id: i64) -> Result<Event, sqlx::Error> {
        let row = sqlx::query(
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
        .fetch_one(&self.pool)
        .await?;

        Event::from_row(row)
    }

    /// Fetches events from the database
    async fn fetch_events(&self, chat_id: i64) -> Result<Vec<Event>, sqlx::Error> {
        let event_ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM events WHERE chat_id = ? ORDER BY event_date",
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::with_capacity(event_ids.len());
        for id in event_ids {
            events.push(self.fetch_event(id).await?);
        }
        Ok(events)
    }

    /// Toggles a user's attendance for an event
    async fn toggle_attendance(&self, event_id: i64, user_id: i64) -> Result<bool, sqlx::Error> {
        let exists = sqlx::query("SELECT 1 FROM attendees WHERE event_id = ? AND user_id = ?")
            .bind(event_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

        if exists.is_some() {
            sqlx::query("DELETE FROM attendees WHERE event_id = ? AND user_id = ?")
                .bind(event_id)
                .bind(user_id)
                .execute(&self.pool)
                .await?;
            Ok(false)
        } else {
            sqlx::query("INSERT INTO attendees (event_id, user_id) VALUES (?, ?)")
                .bind(event_id)
                .bind(user_id)
                .execute(&self.pool)
                .await?;
            Ok(true)
        }
    }

    /// Handles callback queries (e.g., RSVP button clicks)
    async fn handle_callback_query(&self, callback_query: CallbackQuery) -> Result<(), BotError> {
        println!("Received callback query: {:?}", callback_query.data);

        let data = callback_query.data.unwrap_or_default();
        let user_id = callback_query.from.id as i64;

        if data.starts_with("accept_") || data.starts_with("decline_") {
            println!("Processing RSVP/cancel for event");
            let event_id = data
                .split('_')
                .nth(1)
                .ok_or(BotError::MissingDraft)?
                .parse::<i64>()?;
            let is_attending = self.toggle_attendance(event_id, user_id).await?;

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

            self.api.answer_callback_query(&answer_params)?;

            // Update just this event's message
            if let Some(message) = callback_query.message {
                let (chat_id, message_id) = match message {
                    MaybeInaccessibleMessage::Message(msg) => (msg.chat.id, msg.message_id),
                    MaybeInaccessibleMessage::InaccessibleMessage(_) => {
                        return Ok(());
                    }
                };

                // Fetch and update the event message
                let event = self.fetch_event(event_id).await?;

                let edit_params = EditMessageTextParams::builder()
                    .chat_id(chat_id)
                    .message_id(message_id)
                    .text(event.format_message())
                    .reply_markup(event.create_keyboard())
                    .build();

                self.api.edit_message_text(&edit_params)?;
            }
        }

        Ok(())
    }

    /// Main event loop for the bot
    pub async fn run(&mut self) -> Result<(), BotError> {
        let mut update_params = GetUpdatesParams::builder()
            .allowed_updates(vec![AllowedUpdate::Message, AllowedUpdate::CallbackQuery])
            .build();

        loop {
            match self.api.get_updates(&update_params) {
                Ok(response) => {
                    for update in response.result {
                        match update.content {
                            UpdateContent::CallbackQuery(query) => {
                                self.handle_callback_query(query).await?;
                            }
                            UpdateContent::Message(message) => {
                                self.handle_message(message).await?;
                            }
                            _ => {}
                        }
                        update_params = GetUpdatesParams::builder()
                            .offset(update.update_id + 1)
                            .allowed_updates(vec![
                                AllowedUpdate::Message,
                                AllowedUpdate::CallbackQuery,
                            ])
                            .build();
                    }
                }
                Err(e) => eprintln!("Error getting updates: {}", e),
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), BotError> {
    let token = std::env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
    let mut bot = Bot::new(&token).await?;
    bot.run().await
}
