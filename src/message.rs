use rand::Rng;

use crate::create_progress_bar;

pub struct Message {
    pub id: i32,
    pub forwarded: bool,
    pub src_chat_id: i64,
    pub post_id: i32,
    pub dst_chat: grammers_client::types::Chat,
    pub data: MessageData,
}

pub struct MessageParsed {
    pub id: Vec<i32>,
    pub forwarded: bool,
    pub src_chat_id: i64,
    pub post_id: i32,
    pub dst_chat: grammers_client::types::Chat,
    pub data: MessageType,
    pub file_paths: Vec<String>,
}

#[derive(Clone)]
pub struct Media {
    pub file_name: String,
    pub grouped_id: Option<i64>,
    pub media: Box<grammers_client::types::Media>,
    pub caption: String,
    pub mime: MimeType,
    pub entities: Vec<grammers_tl_types::enums::MessageEntity>,
}

pub struct Text {
    message: String,
    entities: Vec<grammers_tl_types::enums::MessageEntity>,
}

pub enum MessageData {
    Media(Media),
    Text(Text),
}

pub enum MessageType {
    MultiMedia(Vec<Media>),
    SingleMedia(Media),
    Text(Text),
}

#[derive(Clone)]
pub enum MimeType {
    Photo,
    Doc,
}

impl Message {
    pub async fn process_new_messages(
        ctx: &super::AppContext,
        mut src_discussion: crate::discussion::Discussion,
        dst_chat: &grammers_client::types::Chat,
        message_id: i32,
    ) -> anyhow::Result<i32> {
        let dst_discussion = crate::discussion::Discussion::get_post_discussion(
            &ctx.client,
            &ctx.db,
            dst_chat,
            message_id,
        )
        .await?;
        let re = regex::Regex::new(r#"(\\|\/|:|\*|\?|"|>|<|\|)"#).unwrap();
        let discussion_chat_id = src_discussion.chat.id();

        let pb = ctx.progress.add(create_progress_bar(0));

        let mut count = 0;
        let mut spins = 0;
        loop {
            // let mut files = Vec::new();
            spins += 1;
            match ctx.client.invoke(&src_discussion.request).await {
                Ok(grammers_tl_types::enums::messages::Messages::ChannelMessages(replies))
                    if !replies.messages.is_empty() =>
                {
                    let offset_id =
                        replies.messages.first().map(|m| m.id()).unwrap_or_default() + 1;
                    let hash =
                        replies
                            .messages
                            .iter()
                            .map(|m| m.id() as i64)
                            .fold(0, |mut acc, id| {
                                acc ^= acc >> 21;
                                acc ^= acc << 35;
                                acc ^= acc >> 4;
                                acc += id;

                                acc
                            });

                    pb.inc_length(replies.messages.len() as u64);

                    for message in replies.messages.into_iter().filter_map(|msg| {
                        if let grammers_tl_types::enums::Message::Message(msg) = msg {
                            if let Some(grammers_tl_types::enums::MessageMedia::Document(ref d)) =
                                msg.media
                            {
                                if let Some(grammers_tl_types::enums::Document::Document(ref doc)) =
                                    d.document
                                {
                                    for attr in &doc.attributes {
                                        if let grammers_tl_types::enums::DocumentAttribute::Filename(
                                            file,
                                        ) = attr
                                        {
                                            return Some(Message {
                                                id: msg.id,
                                                src_chat_id: src_discussion.id,
                                                dst_chat: dst_discussion.chat.clone(),
                                                post_id: dst_discussion.request.msg_id,
                                                data: MessageData::Media (Media {
                                                    file_name: re.replace_all(&file.file_name, "").to_string(), 
                                                    grouped_id: msg.grouped_id,
                                                    media: Box::new(grammers_client::types::Media::from_raw(msg.media.unwrap()).unwrap()),
                                                    caption: msg.message,
                                                    mime: MimeType::Doc,
                                                    entities: msg.entities.unwrap_or_default(),
                                                }),
                                                forwarded: false,
                                            });
                                        }
                                    }
                                }
                            } else if let Some(grammers_tl_types::enums::MessageMedia::Photo(ref photo)) = msg.media {
                                if let Some(grammers_tl_types::enums::Photo::Photo(photo)) = &photo.photo {
                                    return Some(Message {
                                        id: msg.id,
                                        src_chat_id: src_discussion.id,
                                        dst_chat: dst_discussion.chat.clone(),
                                        post_id: dst_discussion.request.msg_id,
                                        data: MessageData::Media (Media {
                                            file_name: format!("{}.jpeg", photo.id),
                                            grouped_id: msg.grouped_id,
                                            media: Box::new(grammers_client::types::Media::from_raw(msg.media.unwrap()).unwrap()),
                                            caption: msg.message,
                                            mime: MimeType::Photo,
                                            entities: msg.entities.unwrap_or_default(),
                                        }),
                                        forwarded: false,
                                    });
                                }
                            } else if !msg.message.is_empty() {
                                return Some(Message {
                                    id: msg.id,
                                    src_chat_id: src_discussion.id,
                                    dst_chat: dst_discussion.chat.clone(),
                                    post_id: dst_discussion.request.msg_id,
                                    data: MessageData::Text(Text { message: msg.message, entities: msg.entities.unwrap_or_default() }),
                                    forwarded: false,
                                });
                            } else {
                                dbg!(msg);
                            }
                        }

                        None
                    }).rev().fold(std::collections::BTreeMap::<i64, MessageParsed>::new(), |mut acc, item| {
                            match item.data {
                                MessageData::Text(t) => {
                                    acc.insert(item.id as i64, MessageParsed {
                                        id: vec![item.id],
                                        forwarded: item.forwarded,
                                        src_chat_id: item.src_chat_id,
                                        dst_chat: item.dst_chat,
                                        post_id: item.post_id,
                                        data: MessageType::Text(t),
                                        file_paths: Vec::new(),
                                    });
                                },
                                MessageData::Media(media) => {
                                    if let Some(grouped_id) = media.grouped_id {
                                        acc.entry(grouped_id).and_modify(|value| {
                                            value.data.push_to_multi(media.clone());
                                            value.id.push(item.id);
                                        }).or_insert(MessageParsed {
                                            id: vec![item.id],
                                            forwarded: item.forwarded,
                                            src_chat_id: item.src_chat_id,
                                            dst_chat: item.dst_chat,
                                            post_id: item.post_id,
                                            data: MessageType::MultiMedia(vec![media]),
                                            file_paths: Vec::new(),
                                        });
                                    } else {
                                        acc.insert(item.id as i64, MessageParsed {
                                            id: vec![item.id],
                                            forwarded: item.forwarded,
                                            src_chat_id: item.src_chat_id,
                                            dst_chat: item.dst_chat,
                                            post_id: item.post_id,
                                            data: MessageType::SingleMedia(media),
                                            file_paths: Vec::new(),
                                        });
                                    }
                                }
                            }

                            acc
                        }).into_values()
                     {
                        pb.inc(1);

                        if sqlx::query_scalar!(
                            r#"
                                select id from "message"
                                where id = $1
                                  and forwarded = 1;
                            "#,
                            message.id[0],
                        )
                        .fetch_optional(&ctx.db)
                        .await
                        .is_ok_and(|d| d.is_some())
                        {
                            continue;
                        }

                        loop {
                            match message.data
                                .send_message(&ctx.client, &dst_discussion.chat, Some(dst_discussion.request.msg_id))
                                .await
                            {
                                Ok(_) => {
                                    let downloaded =
                                        !matches!(message.data, MessageType::Text(_));
                                    for msg_id in message.id {
                                        sqlx::query!(
                                            r#"
                                                insert into "message"
                                                values ($1, $2, $3, $4)
                                                on conflict do update set forwarded = excluded.forwarded;
                                            "#,
                                            msg_id,
                                            discussion_chat_id,
                                            downloaded,
                                            message.forwarded,
                                        )
                                        .execute(&ctx.db)
                                        .await?;
                                    }

                                    count += 1;
                                    break;
                                }
                                Err(err) => match err {
                                    grammers_client::InvocationError::Rpc(rpc_error)
                                        if rpc_error.name == "FLOOD_WAIT" =>
                                    {
                                        tracing::error!(target: "cc", "Error sending message: {rpc_error}");
                                        let wait_time = rpc_error.value.unwrap_or(60) as u64;

                                        pb.set_message(format!(
                                            "Flood wait encountered - sleeping for {wait_time}s...💤"
                                        ));
                                        tokio::time::sleep(tokio::time::Duration::from_secs(
                                            wait_time,
                                        ))
                                        .await;
                                    }
                                    grammers_client::InvocationError::Rpc(rpc_error)
                                        if rpc_error.name == "CHAT_FORWARDS_RESTRICTED" => {
                                        tracing::info!(target: "cc", "Chat forwards restricted, downloading file...");

                                        ctx.download_tx.send(message).await?;
                                        break;
                                    }
                                    err => {
                                        tracing::error!(target: "cc", "Unrecoverable error sending message: {err}");
                                        sqlx::query!(
                                            r#"
                                                update "discussion"
                                                set offset_id   = $2,
                                                    min_id      = $2,
                                                    hash        = $3
                                                where id = $1;
                                            "#,
                                            src_discussion.id,
                                            message.id[0],
                                            0,
                                        )
                                        .execute(&ctx.db)
                                        .await?;

                                        return Err(anyhow::Error::msg(err));
                                    }
                                },
                            }
                        }
                    }

                    let mut rng = rand::rng();
                    let wait_time = if spins > 0 && spins % 10 == 0 {
                        rng.random_range(1000..=1200) as u64
                    } else {
                        rng.random_range(60..=180) as u64
                    };

                    pb.set_position(pb.length().unwrap_or_default());
                    pb.set_message(format!("Sleeping for {wait_time}s...💤"));
                    // tracing::info!(target: "cc", "Sleeping for {wait_time}s...💤");
                    tokio::time::sleep(tokio::time::Duration::from_secs(wait_time)).await;

                    src_discussion.request.offset_id = offset_id;
                    src_discussion.request.min_id = offset_id;

                    src_discussion.request.hash = hash;

                    sqlx::query!(
                        r#"
                            update "discussion"
                            set offset_id   = $2,
                                min_id      = $3,
                                hash        = $4
                            where id = $1;
                        "#,
                        src_discussion.id,
                        src_discussion.request.offset_id,
                        src_discussion.request.min_id,
                        src_discussion.request.hash,
                    )
                    .execute(&ctx.db)
                    .await?;
                }
                Ok(grammers_tl_types::enums::messages::Messages::ChannelMessages(_))
                | Ok(grammers_tl_types::enums::messages::Messages::NotModified(_)) => {
                    tracing::info!(target: "cc", "All comments were parsed");
                    break;
                }
                Ok(_) => unreachable!(),
                Err(err) => match err {
                    grammers_client::InvocationError::Rpc(rpc_error)
                        if rpc_error.name == "FLOOD_WAIT" =>
                    {
                        tracing::error!(target: "cc", "Error fetching comments: {rpc_error}");
                        let wait_time = rpc_error.value.unwrap_or(60) as u64;

                        tracing::info!(target: "cc", "Sleeping for {wait_time}s...💤");
                        tokio::time::sleep(tokio::time::Duration::from_secs(wait_time)).await;
                    }
                    err => {
                        tracing::error!(target: "cc", "Unrecoverable unknown error fetching comments: {err}");
                        continue;
                    }
                },
            }

            // if spins > 0 && spins % 5 == 0 {
            //     pb.set_message("Sleeping for 10s...💤");
            //     tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            // }
        }

        pb.finish_and_clear();

        Ok(count)
    }
}

impl MessageType {
    async fn send_message(
        &self,
        client: &grammers_client::Client,
        chat: &grammers_client::types::Chat,
        reply_to: Option<i32>,
    ) -> Result<(), grammers_client::InvocationError> {
        match self {
            Self::Text(text) => {
                let message = grammers_client::InputMessage::text(&text.message)
                    .reply_to(reply_to)
                    .fmt_entities(text.entities.clone());
                client.send_message(chat, message).await.map(|_| ())
            }
            Self::SingleMedia(media) => {
                let message = grammers_client::InputMessage::text(&media.caption)
                    .copy_media(&media.media)
                    .reply_to(reply_to)
                    .fmt_entities(media.entities.clone());
                client.send_message(chat, message).await.map(|_| ())
            }
            Self::MultiMedia(medias) => {
                let album = medias
                    .iter()
                    .map(|m| {
                        grammers_client::InputMedia::caption(m.caption.clone())
                            .copy_media(&m.media)
                            .fmt_entities(m.entities.clone())
                    })
                    .collect();
                client.send_album(chat, album).await.map(|_| ())
            }
        }
    }

    fn push_to_multi(&mut self, value: Media) {
        match self {
            Self::MultiMedia(m) => m.push(value),
            _ => panic!(),
        }
    }
}
