#[derive(Debug)]
pub struct Discussion {
    pub id: i64,
    pub request: grammers_tl_types::functions::messages::GetReplies,
}

impl Discussion {
    pub async fn id_to_discussion_chat(
        client: &grammers_client::Client,
        id: i64,
    ) -> anyhow::Result<grammers_client::types::chat::Chat> {
        // discussion groups are always supergroups so we invoke GetChannels method
        let channels_req = grammers_tl_types::functions::channels::GetChannels {
            id: vec![grammers_tl_types::enums::InputChannel::Channel(
                grammers_tl_types::types::InputChannel {
                    channel_id: id,
                    access_hash: 0,
                },
            )],
        };
        let chat = match client.invoke(&channels_req).await? {
            grammers_tl_types::enums::messages::Chats::Chats(mut data) => {
                match data.chats.pop() {
                    Some(grammers_tl_types::enums::Chat::Channel(channel)) => {
                        grammers_client::types::Chat::Channel(grammers_client::types::Channel {
                            raw: channel,
                        })
                    }
                    // unreachable as we already know that
                    // it cannot be anything other than Channel
                    Some(_) => unreachable!(),
                    // no idea under which circumstances None matches
                    None => {
                        return Err(anyhow::anyhow!("NO_DISCUSSION_CHAT_FOUND"));
                    }
                }
            }
            // unreachable as we are fetching only one chat
            // thus, it cannot be
            grammers_tl_types::enums::messages::Chats::Slice(_) => unreachable!(),
        };

        Ok(chat)
    }

    pub async fn fetch(
        db: &sqlx::SqlitePool,
        packed_discussion: grammers_session::PackedChat,
        chat_id: i64,
        message_id: i32,
    ) -> anyhow::Result<Self> {
        let record = sqlx::query_as!(
            DiscussionRecord,
            r#"
                select *
                from "discussion"
                where chat_id = $1
                  and post_id = $2;
            "#,
            chat_id,
            message_id,
        )
        .fetch_one(db)
        .await?;

        Ok(Self {
            id: record.id,
            request: grammers_tl_types::functions::messages::GetReplies {
                peer: packed_discussion.to_input_peer(),
                msg_id: record.msg_id as i32,
                offset_id: record.offset_id as i32,
                offset_date: record.offset_date as i32,
                add_offset: record.add_offset as i32,
                limit: record.limit as i32,
                max_id: record.max_id as i32,
                min_id: record.min_id as i32,
                hash: record.hash,
            },
        })
    }
}

#[allow(dead_code)]
struct DiscussionRecord {
    id: i64,
    post_id: i64,
    msg_id: i64,
    chat_id: i64,
    offset_id: i64,
    offset_date: i64,
    add_offset: i64,
    limit: i64,
    max_id: i64,
    min_id: i64,
    hash: i64,
}
