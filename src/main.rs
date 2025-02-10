use frankenstein::{
    AllowedUpdate, Api, CallbackQuery, ChatMember, EditMessageTextParams, GetUpdatesParams,
    InlineKeyboardButton, InlineKeyboardMarkup, MaybeInaccessibleMessage, Message, ParseMode,
    ReplyMarkup, SendMessageParams, TelegramApi, UpdateContent,
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

/// Represents the context of event creation
#[derive(Clone, Debug)]
struct EventContext {
    origin_chat_id: i64, // The group chat where /create was initiated
    draft: EventDraft,
    state: EventCreationState,
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
    accepted: Vec<(i64, String)>,
    declined: Vec<(i64, String)>,
}

impl Event {
    /// Creates a formatted message for Telegram display
    fn format_message(&self) -> String {
        let mut message = format!(
            "*__{}__*\n{}\n\n⏰ {}\n📍 {}\n",
            Self::escape_markdown(&self.title),
            Self::escape_markdown(&self.description),
            Self::escape_markdown(&self.event_date),
            Self::escape_markdown(&self.location),
        );

        if !self.accepted.is_empty() {
            message.push_str("\n✅ Accepted\n");
            for (_, user_name) in &self.accepted {
                message.push_str(&format!("• {}\n", user_name));
            }
        }

        if !self.declined.is_empty() {
            message.push_str("\n❌ Declined\n");
            for (_, user_name) in &self.declined {
                message.push_str(&format!("• {}\n", user_name));
            }
        }

        message
    }

    /// Creates RSVP keyboard buttons for this event
    fn create_keyboard(&self) -> InlineKeyboardMarkup {
        let accept_button = InlineKeyboardButton::builder()
            .text("✅ Accept")
            .callback_data(format!("accepted_{}", self.id))
            .build();

        let decline_button = InlineKeyboardButton::builder()
            .text("❌ Decline")
            .callback_data(format!("declined_{}", self.id))
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
            accepted: Vec::new(),
            declined: Vec::new(),
        })
    }

    /// Escapes special characters for Telegram MarkdownV2 format
    fn escape_markdown(text: &str) -> String {
        text.replace('_', r"\_")
            .replace('*', r"\*")
            .replace('[', r"\[")
            .replace(']', r"\]")
            .replace('(', r"\(")
            .replace(')', r"\)")
            .replace('~', r"\~")
            .replace('`', r"\`")
            .replace('>', r"\>")
            .replace('#', r"\#")
            .replace('+', r"\+")
            .replace('-', r"\-")
            .replace('=', r"\=")
            .replace('|', r"\|")
            .replace('{', r"\{")
            .replace('}', r"\}")
            .replace('.', r"\.")
            .replace('!', r"\!")
    }
}

/// Manages the bots state and operations
struct Bot {
    api: Api,
    pool: SqlitePool,
    event_contexts: HashMap<i64, EventContext>,
}

impl Bot {
    /// Creates a new Televent instance
    async fn new(token: &str) -> Result<Self, BotError> {
        const DB_URL: &str = "sqlite://events_bot.db";
        let pool = Self::init_db(DB_URL).await?;

        Ok(Self {
            api: Api::new(token),
            pool,
            event_contexts: HashMap::new(),
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
        let is_private = message.chat.type_field == frankenstein::ChatType::Private;

        let text = match message.text {
            Some(text) => text,
            None => return Ok(()),
        };

        match text.as_str() {
            "/create" => self.handle_create(user_id, chat_id, is_private).await?,
            "/list" => self.list_events(chat_id).await?,
            "/cancel" => self.handle_cancel(user_id, chat_id).await?,
            "/help" => self.handle_help(chat_id).await?,
            _ if is_private && self.event_contexts.contains_key(&user_id) => {
                self.handle_event_creation(user_id, chat_id, &text).await?
            }
            _ => {}
        }

        Ok(())
    }

    /// Handles the /create command, redirecting to private chat if needed
    async fn handle_create(
        &mut self,
        user_id: i64,
        chat_id: i64,
        is_private: bool,
    ) -> Result<(), BotError> {
        if is_private {
            return self.send_message(
                chat_id,
                "Please initiate event creation in a group chat. Ask the group admin to invite me to the group chat."
            ).await;
        }

        // Try to message the user privately
        let private_msg_result = self
            .send_message(
                user_id,
                "Please enter the Title of the event. To exit, type /cancel.",
            )
            .await;

        match private_msg_result {
            Ok(()) => {
                self.event_contexts.insert(
                    user_id,
                    EventContext {
                        origin_chat_id: chat_id,
                        draft: EventDraft::default(),
                        state: EventCreationState::AwaitingTitle,
                    },
                );
                Ok(())
            }
            Err(BotError::Telegram(frankenstein::Error::Api(e))) if e.error_code == 403 => {
                const HELP_MESSAGE: &str = concat!(
                    "To create an event, you need to start a private chat with me first.\n\n",
                    "1. Click here: @Mississauga_Maybes_Bot\n",
                    "2. Click 'Start' or send any message\n",
                    "3. Come back to this group and try /create again"
                );
                self.send_message(chat_id, HELP_MESSAGE).await
            }
            Err(e) => Err(e),
        }
    }

    /// Handles event creation state machine
    async fn handle_event_creation(
        &mut self,
        user_id: i64,
        chat_id: i64,
        text: &str,
    ) -> Result<(), BotError> {
        let context = match self.event_contexts.get_mut(&user_id) {
            Some(context) => context,
            None => return Ok(()),
        };

        // Update draft based on state
        match context.state {
            EventCreationState::AwaitingTitle => {
                context.draft.title = text.to_string();
                context.state = EventCreationState::AwaitingDescription;
                self.send_message(chat_id, "Please enter an Event description.")
                    .await?;
            }
            EventCreationState::AwaitingDescription => {
                context.draft.description = text.to_string();
                context.state = EventCreationState::AwaitingLocation;
                self.send_message(chat_id, "Please enter the Location of the event.")
                    .await?;
            }
            EventCreationState::AwaitingLocation => {
                context.draft.location = text.to_string();
                context.state = EventCreationState::AwaitingTime;
                self.send_message(
                    chat_id,
                    "Please enter the Date and Time the event takes place.",
                )
                .await?;
            }
            EventCreationState::AwaitingTime => {
                context.draft.datetime = text.to_string();

                // Get the context before removing it
                let EventContext {
                    origin_chat_id,
                    draft,
                    ..
                } = self
                    .event_contexts
                    .remove(&user_id)
                    .ok_or(BotError::MissingDraft)?;

                // Create the event
                self.create_event(user_id, origin_chat_id, &draft).await?;

                // Confirm in private chat
                self.send_message(
                    chat_id,
                    "The Event has been created and posted to the group!",
                )
                .await?;
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

        self.list_event(chat_id, &event).await?;

        Ok(())
    }

    /// Cancels ongoing event creation
    async fn handle_cancel(&mut self, user_id: i64, chat_id: i64) -> Result<(), BotError> {
        if self.event_contexts.remove(&user_id).is_some() {
            self.send_message(chat_id, "Event creation cancelled.")
                .await?;
        }
        Ok(())
    }

    /// List a single event in chat with RSVP buttons
    async fn list_event(&self, chat_id: i64, event: &Event) -> Result<(), BotError> {
        let params = SendMessageParams::builder()
            .chat_id(chat_id)
            .text(event.format_message())
            .parse_mode(ParseMode::MarkdownV2)
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

    /// Fetches a user's full name from Telegram
    async fn get_user_name(&self, chat_id: i64, user_id: i64) -> Result<String, BotError> {
        let params = frankenstein::GetChatMemberParams::builder()
            .chat_id(chat_id)
            .user_id(user_id as u64)
            .build();

        let response = self.api.get_chat_member(&params)?;
        let user = match response.result {
            ChatMember::Member(member) => member.user,
            ChatMember::Administrator(admin) => admin.user,
            ChatMember::Creator(creator) => creator.user,
            ChatMember::Restricted(restricted) => restricted.user,
            ChatMember::Left(left) => left.user,
            ChatMember::Kicked(kicked) => kicked.user,
        };

        Ok(if let Some(last_name) = user.last_name {
            format!("{} {}", user.first_name, last_name)
        } else {
            user.first_name
        })
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
                chat_id
            FROM events 
            WHERE id = ?
            "#,
        )
        .bind(event_id)
        .fetch_one(&self.pool)
        .await?;

        let chat_id: i64 = row.get("chat_id");
        let mut event = Event::from_row(row)?;

        let attendees = sqlx::query("SELECT user_id, status FROM attendees WHERE event_id = ?")
            .bind(event_id)
            .fetch_all(&self.pool)
            .await?;

        for attendee in attendees {
            let user_id: i64 = attendee.get("user_id");
            let status: String = attendee.get("status");

            let name = match self.get_user_name(chat_id, user_id).await {
                Ok(name) => name,
                Err(e) => {
                    eprintln!("Failed to fetch user info for {}: {}", user_id, e);
                    format!("User {}", user_id)
                }
            };

            match status.as_str() {
                "accepted" => event.accepted.push((user_id, name)),
                "declined" => event.declined.push((user_id, name)),
                _ => {} // Should never happen due to CHECK constraint
            }
        }

        Ok(event)
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
    async fn update_attendance(
        &self,
        event_id: i64,
        user_id: i64,
        status: &str,
    ) -> Result<(), sqlx::Error> {
        let exists = sqlx::query("SELECT status FROM attendees WHERE event_id = ? AND user_id = ?")
            .bind(event_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

        match exists {
            Some(row) => {
                let current_status: String = row.get("status");
                // If clicking same status, remove the status
                if current_status == status {
                    sqlx::query("DELETE FROM attendees WHERE event_id = ? AND user_id = ?")
                        .bind(event_id)
                        .bind(user_id)
                        .execute(&self.pool)
                        .await?;
                } else {
                    // Otherwise update to new status
                    sqlx::query(
                        "UPDATE attendees SET status = ? WHERE event_id = ? AND user_id = ?",
                    )
                    .bind(status)
                    .bind(event_id)
                    .bind(user_id)
                    .execute(&self.pool)
                    .await?;
                }
            }
            None => {
                sqlx::query("INSERT INTO attendees (event_id, user_id, status) VALUES (?, ?, ?)")
                    .bind(event_id)
                    .bind(user_id)
                    .bind(status)
                    .execute(&self.pool)
                    .await?;
            }
        }
        Ok(())
    }

    /// Handles callback queries (e.g., RSVP button clicks)
    async fn handle_callback_query(&self, callback_query: CallbackQuery) -> Result<(), BotError> {
        let data = callback_query.data.unwrap_or_default();
        let user_id = callback_query.from.id as i64;

        if data.starts_with("accepted_") || data.starts_with("declined_") {
            let (status, event_id) = data.split_once('_').ok_or(BotError::MissingDraft)?;
            let event_id: i64 = event_id.parse()?;
            self.update_attendance(event_id, user_id, status).await?;

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
                    .parse_mode(ParseMode::MarkdownV2)
                    .reply_markup(event.create_keyboard())
                    .build();

                return match self.api.edit_message_text(&edit_params) {
                    Ok(_) => Ok(()),
                    Err(frankenstein::Error::Api(e))
                        if e.error_code == 400
                            && e.description.contains("message is not modified") =>
                    {
                        Ok(())
                    }
                    Err(e) => Err(BotError::Telegram(e)),
                };
            }
        }
        Ok(())
    }

    /// Shows help message
    async fn handle_help(&self, chat_id: i64) -> Result<(), BotError> {
        let help_text = r#"
Available commands:
    /create - Start creating a new event
    /cancel - Cancel event creation in progress
    /list - Show all events in this chat
    /help - Show this help message

To create an event:
    1. Use /create in a group chat
    2. Bot will message you privately
    3. Follow the prompts to create the event
    4. Event will be posted in the group chat where you started
            "#;

        self.send_message(chat_id, help_text).await?;
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
