use crate::error::BotError;
use crate::event::{
    Event, EventContext, EventCreationState, EventDraft, DATETIME_FORMAT, DB_DATETIME_FORMAT,
};
use chrono::{NaiveDateTime, ParseError};
use frankenstein::{
    AllowedUpdate, Api, CallbackQuery, ChatMember, EditMessageTextParams, GetUpdatesParams,
    MaybeInaccessibleMessage, Message, ParseMode, ReplyMarkup, SendMessageParams, TelegramApi,
    UpdateContent,
};
use sqlx::Row;
use sqlx::SqlitePool;
use std::collections::HashMap;

pub struct Bot {
    api: Api,
    pool: SqlitePool,
    event_contexts: HashMap<i64, EventContext>,
}

impl Bot {
    pub async fn new(token: &str, pool: SqlitePool) -> Result<Self, BotError> {
        Ok(Self {
            api: Api::new(token),
            pool,
            event_contexts: HashMap::new(),
        })
    }

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

    /// Handles callback queries (e.g., RSVP button clicks)
    async fn handle_callback_query(&self, callback_query: CallbackQuery) -> Result<(), BotError> {
        let data = callback_query.data.unwrap_or_default();
        let user_id = callback_query.from.id as i64;

        if data.starts_with("accepted_") || data.starts_with("declined_") {
            let (status, event_id) = data.split_once('_').ok_or(BotError::MissingDraft)?;
            let event_id: i64 = event_id.parse()?;

            let _ = match self.fetch_event(event_id).await {
                Ok(event) => event,
                Err(e) => {
                    if let sqlx::Error::RowNotFound = e {
                        return Ok(());
                    } else {
                        return Err(BotError::Database(e));
                    }
                }
            };

            self.update_attendance(event_id, user_id, status).await?;

            // Update just this event's message
            if let Some(message) = callback_query.message {
                let (chat_id, message_id, public) = match message {
                    MaybeInaccessibleMessage::Message(msg) => (
                        msg.chat.id,
                        msg.message_id,
                        msg.chat.type_field != frankenstein::ChatType::Private,
                    ),
                    MaybeInaccessibleMessage::InaccessibleMessage(_) => {
                        return Ok(());
                    }
                };

                let event = self.fetch_event(event_id).await?;

                let edit_params = EditMessageTextParams::builder()
                    .chat_id(chat_id)
                    .message_id(message_id)
                    .text(event.format_message())
                    .parse_mode(ParseMode::MarkdownV2)
                    .reply_markup(event.create_keyboard(user_id, public))
                    .build();

                return match self.api.edit_message_text(&edit_params) {
                    Ok(_) => Ok(()),
                    Err(frankenstein::Error::Api(ref e))
                        if e.error_code == 400
                            && e.description.contains("message is not modified") =>
                    {
                        Ok(())
                    }
                    Err(e) => Err(BotError::Telegram(e)),
                };
            }
        } else if data.starts_with("deleted_") {
            let event_id: i64 = data
                .split_once('_')
                .ok_or(BotError::MissingDraft)?
                .1
                .parse()?;

            let event = match self.fetch_event(event_id).await {
                Ok(event) => event,
                Err(e) => {
                    if let sqlx::Error::RowNotFound = e {
                        return Ok(());
                    } else {
                        return Err(BotError::Database(e));
                    }
                }
            };

            if event.creator != user_id {
                return Ok(()); // Silently ignore if not event creator (others should not even see the Delete button)
            }

            if let Some(message) = callback_query.message {
                let (chat_id, message_id) = match message {
                    MaybeInaccessibleMessage::Message(msg) => (msg.chat.id, msg.message_id),
                    MaybeInaccessibleMessage::InaccessibleMessage(_) => {
                        return Ok(());
                    }
                };

                // Delete the event from database
                self.delete_event(event_id).await?;

                // Delete the Telegram message
                let delete_params = frankenstein::DeleteMessageParams::builder()
                    .chat_id(chat_id)
                    .message_id(message_id)
                    .build();

                if let Err(err) = self.api.delete_message(&delete_params) {
                    match err {
                        frankenstein::Error::Api(ref e) if e.error_code == 400 => { /* ignore */ }
                        err => return Err(BotError::Telegram(err)),
                    }
                }

                self.send_message(chat_id, "Event has been deleted.")
                    .await?;
            }
        }
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
            "/list" => self.list_events(chat_id, user_id).await?,
            "/cancel" => self.handle_cancel(user_id, chat_id).await?,
            "/myevents" => self.list_my_events(user_id).await?,
            "/help" => self.handle_help(chat_id).await?,
            _ if is_private && self.event_contexts.contains_key(&user_id) => {
                self.handle_event_creation(user_id, chat_id, &text).await?
            }
            _ => {}
        }

        Ok(())
    }

    /// Creates a new event in the database
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

    /// List a single event in chat with RSVP buttons
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

                let prompt = format!(
                    "Please enter the Date and Time of the event in the following format YYYY-MM-DD HH:MM (e.g., 2025-08-15 19:00)"
                );
                self.send_message(chat_id, &prompt).await?;
            }
            EventCreationState::AwaitingTime => {
                match parse_datetime_string(text) {
                    Ok(parsed_datetime) => {
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

                        self.create_event(user_id, origin_chat_id, &draft, parsed_datetime)
                            .await?;
                        self.send_message(
                            chat_id,
                            "The Event has been created and posted to the group!",
                        )
                        .await?;
                    }
                    Err(_) => {
                        let error_msg = format!(
                            "Sorry, that doesn't look like a valid date/time. Please use the format YYYY-MM-DD HH:MM (e.g., 2025-08-15 19:00)."
                        );
                        self.send_message(chat_id, &error_msg).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Fetches a user's full name from Telegram
    async fn handle_cancel(&mut self, user_id: i64, chat_id: i64) -> Result<(), BotError> {
        if self.event_contexts.remove(&user_id).is_some() {
            self.send_message(chat_id, "Event creation cancelled.")
                .await?;
        }
        Ok(())
    }

    /// List all events created by user in private chat
    async fn handle_help(&self, chat_id: i64) -> Result<(), BotError> {
        let help_text = r#"
Available commands:
    /create - Start creating a new event
    /cancel - Cancel event creation in progress
    /list - Show all events in this chat
    /myevents - Show me all the events I've created
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

    /// Handles event creation state machine
    async fn list_event(
        &self,
        chat_id: i64,
        event: &Event,
        viewer_id: i64,
        public: bool,
    ) -> Result<(), BotError> {
        let params = SendMessageParams::builder()
            .chat_id(chat_id)
            .text(event.format_message())
            .parse_mode(ParseMode::MarkdownV2)
            .reply_markup(ReplyMarkup::InlineKeyboardMarkup(
                event.create_keyboard(viewer_id, public),
            ))
            .build();

        self.api.send_message(&params)?;
        Ok(())
    }

    /// Lists all events in a chat
    async fn list_events(&self, chat_id: i64, viewer_id: i64) -> Result<(), BotError> {
        let events = self.fetch_events(chat_id).await?;

        if events.is_empty() {
            self.send_message(chat_id, "No events scheduled.").await?;
            return Ok(());
        }

        for event in events {
            self.list_event(chat_id, &event, viewer_id, true).await?;
        }

        Ok(())
    }

    /// Cancels ongoing event creation
    async fn list_my_events(&self, user_id: i64) -> Result<(), BotError> {
        let event_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM events WHERE creator = ?")
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?;

        if event_ids.is_empty() {
            self.send_message(user_id, "You have not created any events.")
                .await?;
            return Ok(());
        }

        for id in event_ids {
            let event = self.fetch_event(id).await?;
            self.list_event(user_id, &event, user_id, false).await?;
        }

        Ok(())
    }

    /// Shows help message
    async fn create_event(
        &self,
        creator: i64,
        chat_id: i64,
        draft: &EventDraft,
        datetime: NaiveDateTime,
    ) -> Result<(), BotError> {
        let db_datetime_str = datetime.format(DB_DATETIME_FORMAT).to_string();

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
        .bind(&db_datetime_str)
        .bind(chat_id)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        let event = self.fetch_event(event_id).await?;
        self.list_event(chat_id, &event, creator, true).await?; // Post to group chat
        self.list_event(creator, &event, creator, false).await?; // Post to creator's private chat

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
    async fn delete_event(&self, event_id: i64) -> Result<(), BotError> {
        // First delete attendees due to foreign key constraint
        sqlx::query("DELETE FROM attendees WHERE event_id = ?")
            .bind(event_id)
            .execute(&self.pool)
            .await?;

        sqlx::query("DELETE FROM events WHERE id = ?")
            .bind(event_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Sends a message to a chat
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

    /// Deletes an event
    async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), BotError> {
        let params = SendMessageParams::builder()
            .chat_id(chat_id)
            .text(text)
            .build();
        self.api.send_message(&params)?;
        Ok(())
    }

    /// Handles the /create command, redirecting to private chat if needed
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
}

/// Helper function to parse date string
pub fn parse_datetime_string(datetime_str: &str) -> Result<NaiveDateTime, ParseError> {
    NaiveDateTime::parse_from_str(datetime_str, DATETIME_FORMAT)
}
