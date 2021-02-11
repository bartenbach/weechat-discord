use crate::{
    buffer::{channel::Channel, ext::BufferExt},
    config::Config,
    discord::discord_connection::DiscordConnection,
    instance::Instance,
};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use twilight_model::id::{ChannelId, GuildId};
use weechat::{
    hooks::{SignalData, SignalHook},
    ReturnCode, Weechat,
};

pub struct Signals {
    _buffer_switch_hook: SignalHook,
    _buffer_typing_hook: SignalHook,
}

impl Signals {
    pub fn hook_all(connection: DiscordConnection, instance: Instance, config: Config) -> Signals {
        let inner_connection = connection.clone();
        let _buffer_switch_hook = SignalHook::new("buffer_switch", {
            move |_: &Weechat, _: &str, data: Option<SignalData>| {
                if let Some(SignalData::Buffer(buffer)) = data {
                    let loaded = buffer.history_loaded();

                    let guild_id = buffer.guild_id();

                    let channel_id = match buffer.channel_id() {
                        Some(channel_id) => channel_id,
                        None => {
                            return ReturnCode::Ok;
                        },
                    };

                    if let Some(channel) = instance.search_buffer(guild_id, channel_id) {
                        if loaded {
                            Weechat::spawn(async move {
                                Signals::ack(guild_id, channel_id, &channel).await;
                            })
                            .detach();
                        } else {
                            buffer.set_history_loaded();
                            let connection = inner_connection.clone();
                            Weechat::spawn(async move {
                                tracing::trace!(
                                    ?guild_id,
                                    ?channel_id,
                                    "Sending channel subscription"
                                );
                                if let Some(guild_id) = guild_id {
                                    connection
                                        .send_guild_subscription(guild_id, channel_id)
                                        .await;
                                }
                            })
                            .detach();
                            Weechat::spawn({
                                let channel = channel.clone();
                                async move {
                                    tracing::trace!(?guild_id, ?channel_id, "Loading history");
                                    if let Err(e) = channel.load_history().await {
                                        tracing::error!(
                                            ?guild_id,
                                            ?channel_id,
                                            "Error loading channel history: {}",
                                            e
                                        );
                                    }

                                    Signals::ack(guild_id, channel_id, &channel).await;
                                }
                            })
                            .detach();
                            if let Err(e) = channel.load_users() {
                                tracing::error!(
                                    ?guild_id,
                                    ?channel_id,
                                    "Error loading channel member list: {}",
                                    e
                                );
                            }
                        }
                    }
                }
                ReturnCode::Ok
            }
        })
        .expect("Unable to hook buffer_switch signal");

        let _buffer_typing_hook = SignalHook::new(
            "input_text_changed",
            move |_: &Weechat, _: &str, data: Option<SignalData>| {
                static LAST_TYPING_TIMESTAMP: Lazy<Arc<Mutex<u64>>> =
                    Lazy::new(|| Arc::new(Mutex::new(0)));

                if !config.send_typing() {
                    return ReturnCode::Ok;
                }

                if let Some(SignalData::Buffer(buffer)) = data {
                    if buffer.input().len() < 2 || buffer.input().starts_with('/') {
                        return ReturnCode::Ok;
                    }

                    // TODO: Wait for user to type for 3 seconds
                    let now = SystemTime::now();
                    let timestamp_now = now
                        .duration_since(UNIX_EPOCH)
                        .expect("Time went backwards")
                        .as_secs() as u64;

                    if *LAST_TYPING_TIMESTAMP.lock() + 9 < timestamp_now {
                        *LAST_TYPING_TIMESTAMP.lock() = timestamp_now;

                        if let Some(channel_id) = buffer.channel_id() {
                            if let Some(conn) = connection.borrow().as_ref() {
                                let http = conn.http.clone();
                                conn.rt.spawn(async move {
                                    tracing::trace!(?channel_id, "Sending typing event");
                                    if let Err(e) = http.create_typing_trigger(channel_id).await {
                                        tracing::error!("Sending typing start failed: {:#?}", e);
                                    };
                                });
                            }
                        }
                    }
                }
                ReturnCode::Ok
            },
        )
        .expect("Unable to hook input_text_changed signal");

        Signals {
            _buffer_switch_hook,
            _buffer_typing_hook,
        }
    }

    async fn ack(guild_id: Option<GuildId>, channel_id: ChannelId, channel: &Channel) {
        tracing::trace!(?guild_id, ?channel_id, "Acking history");

        if let Err(e) = channel.ack().await {
            tracing::error!(
                ?guild_id,
                ?channel_id,
                "Error acking channel history: {}",
                e
            );
        }
    }
}
