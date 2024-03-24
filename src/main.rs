use frankenstein::AllowedUpdate;
use frankenstein::Api;
use frankenstein::GetUpdatesParams;
use frankenstein::ReplyParameters;
use frankenstein::SendMessageParams;
use frankenstein::TelegramApi;
use frankenstein::UpdateContent;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct Event {
    pub name: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub time: Option<String>,
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

fn send_message(api: &Api, chat_id: i64, text: &str) {
    let send_message_params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text)
        .build();

    if let Err(err) = api.send_message(&send_message_params) {
        println!("Failed to send message: {err:?}");
    }
}

pub fn main() {
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
                                            event.name = Some(text.clone());
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
                                            event.description = Some(text.clone());
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
                                            event.location = Some(text.clone());
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
                                            event.time = Some(text.clone());

                                            send_message(
                                                &api,
                                                chat_id,
                                                "The Event has been saved.",
                                            );
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
