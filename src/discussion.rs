use crate::invoke_with_retry;

#[derive(Debug)]
pub struct Discussion {
    pub id: i64,
    pub chat: grammers_client::types::Chat,
    pub request: grammers_tl_types::functions::messages::GetReplies,
}

impl Discussion {
    pub async fn get_post_discussion(
        client: &grammers_client::Client,
        db: &sqlx::SqlitePool,
        chat: &grammers_client::types::Chat,
        post_id: i32,
    ) -> anyhow::Result<Self> {
        let msgs = client.get_messages_by_id(chat, &[post_id]).await?;
        let message = match msgs.first() {
            Some(Some(msg)) => msg,
            _ => {
                return Err(anyhow::anyhow!(
                    "Specified message with id {post_id} cannot be found"
                ))
            }
        };

        let request = grammers_tl_types::functions::messages::GetDiscussionMessage {
            peer: chat.pack().to_input_peer(),
            msg_id: message.id(),
        };
        let grammers_tl_types::enums::messages::DiscussionMessage::Message(discussion_msg) =
            invoke_with_retry(client, &request).await?;
        // client.invoke(&request).await?;
        let (msg_id, discussion_group) =
            if let Some(grammers_tl_types::enums::Message::Message(msg)) =
                discussion_msg.messages.first()
            {
                let channel_id = match &msg.peer_id {
                    grammers_tl_types::enums::Peer::Channel(peer_channel) => {
                        peer_channel.channel_id
                    }
                    // unreachable as we have already established that
                    // the message should be from a channel
                    _ => unreachable!(),
                };
                let chat = discussion_msg
                    .chats
                    .into_iter()
                    .find(|c| c.id() == channel_id)
                    .expect("Chat with id msg.peer_id should be present");

                (msg.id, grammers_client::types::Chat::from_raw(chat))
            } else {
                return Err(anyhow::anyhow!("Cannot find start message thread"));
            };

        let chat_id = chat.id();
        let record = sqlx::query_as!(
            DiscussionRecord,
            r#"
                insert into "discussion" (chat_id, post_id, msg_id)
                values ($1, $2, $3)
                on conflict do nothing
                returning *;
            "#,
            chat_id,
            post_id,
            msg_id,
        )
        .fetch_one(db)
        .await?;

        let peer = discussion_group.pack().to_input_peer();

        Ok(Self {
            id: record.id,
            chat: discussion_group,
            request: grammers_tl_types::functions::messages::GetReplies {
                peer,
                msg_id: record.msg_id as i32,
                offset_id: record.offset_id as i32,
                offset_date: 0,
                add_offset: -30,
                limit: 30,
                max_id: 0,
                min_id: record.min_id as i32,
                hash: record.hash,
            },
        })
    }

    // pub async fn id_to_discussion_chat(
    //     client: &grammers_client::Client,
    //     id: i64,
    // ) -> anyhow::Result<grammers_client::types::chat::Chat> {
    //     // discussion groups are always supergroups so we invoke GetChannels method
    //     let channels_req = grammers_tl_types::functions::channels::GetChannels {
    //         id: vec![grammers_tl_types::enums::InputChannel::Channel(
    //             grammers_tl_types::types::InputChannel {
    //                 channel_id: id,
    //                 access_hash: 0,
    //             },
    //         )],
    //     };
    //     let chat = match client.invoke(&channels_req).await? {
    //         grammers_tl_types::enums::messages::Chats::Chats(mut data) => {
    //             match data.chats.pop() {
    //                 Some(grammers_tl_types::enums::Chat::Channel(channel)) => {
    //                     grammers_client::types::Chat::Channel(grammers_client::types::Channel {
    //                         raw: channel,
    //                     })
    //                 }
    //                 // unreachable as we already know that
    //                 // it cannot be anything other than Channel
    //                 Some(_) => unreachable!(),
    //                 // no idea under which circumstances None matches
    //                 None => {
    //                     return Err(anyhow::anyhow!("NO_DISCUSSION_CHAT_FOUND"));
    //                 }
    //             }
    //         }
    //         // unreachable as we are fetching only one chat
    //         // thus, it cannot be
    //         grammers_tl_types::enums::messages::Chats::Slice(_) => unreachable!(),
    //     };

    //     Ok(chat)
    // }

    // pub async fn fetch(
    //     client: &grammers_client::Client,
    //     db: &sqlx::SqlitePool,
    //     packed_discussion: grammers_session::PackedChat,
    //     chat_id: i64,
    //     message_id: i32,
    // ) -> anyhow::Result<Self> {
    //     let record = sqlx::query_as!(
    //         DiscussionRecord,
    //         r#"
    //             select *
    //             from "discussion"
    //             where chat_id = $1
    //               and post_id = $2;
    //         "#,
    //         chat_id,
    //         message_id,
    //     )
    //     .fetch_one(db)
    //     .await?;

    //     let peer = packed_discussion.to_input_peer();
    //     let chat = client.unpack_chat(packed_discussion).await?;

    //     Ok(Self {
    //         id: record.id,
    //         chat,
    //         request: grammers_tl_types::functions::messages::GetReplies {
    //             peer,
    //             msg_id: record.msg_id as i32,
    //             offset_id: record.offset_id as i32,
    //             offset_date: 0,
    //             add_offset: -100,
    //             limit: 100,
    //             max_id: 0,
    //             min_id: record.min_id as i32,
    //             hash: record.hash,
    //         },
    //     })
    // }
}

#[allow(dead_code)]
struct DiscussionRecord {
    id: i64,
    chat_id: i64,
    post_id: i64,
    msg_id: i64,
    offset_id: i64,
    min_id: i64,
    hash: i64,
}
