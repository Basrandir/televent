use frankenstein::{InlineKeyboardButton, InlineKeyboardMarkup};
use sqlx::Row;

/// Represents the state of event creation for a user
#[derive(Debug, Clone, PartialEq)]
pub enum EventCreationState {
    AwaitingTitle,
    AwaitingDescription,
    AwaitingLocation,
    AwaitingTime,
}

/// Represents an event being created
#[derive(Clone, Debug, Default)]
pub struct EventDraft {
    pub title: String,
    pub description: String,
    pub location: String,
    pub datetime: String,
}

/// Represents the context of event creation
#[derive(Clone, Debug)]
pub struct EventContext {
    pub origin_chat_id: i64, // The group chat where /create was initiated
    pub draft: EventDraft,
    pub state: EventCreationState,
}

/// Represents a fully formed event from the database
#[derive(Debug)]
pub struct Event {
    id: i64,
    title: String,
    description: String,
    location: String,
    event_date: String,
    pub creator: i64,
    pub accepted: Vec<(i64, String)>,
    pub declined: Vec<(i64, String)>,
}

impl Event {
    /// Creates a formatted message for Telegram display
    pub fn format_message(&self) -> String {
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
    pub fn create_keyboard(&self, viewer_id: i64, public: bool) -> InlineKeyboardMarkup {
        let accept_button = InlineKeyboardButton::builder()
            .text("✅ Accept")
            .callback_data(format!("accepted_{}", self.id))
            .build();

        let decline_button = InlineKeyboardButton::builder()
            .text("❌ Decline")
            .callback_data(format!("declined_{}", self.id))
            .build();

        let mut keyboard = vec![vec![accept_button, decline_button]];

        if !public && self.creator == viewer_id {
            let delete_button = InlineKeyboardButton::builder()
                .text("🗑️ Delete")
                .callback_data(format!("deleted_{}", self.id))
                .build();

            keyboard.push(vec![delete_button]);
        }

        InlineKeyboardMarkup::builder()
            .inline_keyboard(keyboard)
            .build()
    }

    /// Creates an Event from a database row
    pub fn from_row(row: sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
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
        let special_chars = [
            '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.',
            '!',
        ];
        let mut escaped = String::with_capacity(text.len());

        for ch in text.chars() {
            if special_chars.contains(&ch) {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
        escaped
    }
}
