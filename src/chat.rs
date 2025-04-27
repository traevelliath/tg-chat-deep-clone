use crate::discussion::Discussion;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Chat {
    pub id: i64,
    pub chat: grammers_client::types::Chat,
    pub discussion_group: Option<grammers_client::types::Chat>,
}

impl Chat {
    pub async fn from_chat_id(
        client: &grammers_client::Client,
        db: &sqlx::SqlitePool,
        chat_id: i64,
    ) -> anyhow::Result<Self> {
        let record = sqlx::query_as!(
            ChatRecord,
            r#"
                select * from "chat"
                where id = $1;
            "#,
            chat_id,
        )
        .fetch_one(db)
        .await?;
        let packed_chat = grammers_session::PackedChat::from_hex(&record.channel)?;
        let chat = client.unpack_chat(packed_chat).await?;

        Self::from_db_record(client, chat, record).await
    }

    pub async fn from_chat(
        ctx: &super::AppContext,
        chat: grammers_client::types::Chat,
    ) -> anyhow::Result<Self> {
        let request = grammers_tl_types::functions::channels::GetFullChannel {
            channel: grammers_tl_types::enums::InputChannel::Channel(
                grammers_tl_types::types::InputChannel {
                    channel_id: chat.id(),
                    access_hash: 0,
                },
            ),
        };
        let grammers_tl_types::enums::messages::ChatFull::Full(mut data) =
            ctx.client.invoke(&request).await?;
        match data.chats.pop() {
            Some(grammers_tl_types::enums::Chat::Channel(channel)) => {}
            // unreachable as we already know that
            // it cannot be anything other than Channel
            Some(_) => unreachable!(),
            // no idea under which circumstances None matches
            None => {
                return Err(anyhow::anyhow!("NO_DISCUSSION_CHAT_FOUND"));
            }
        }
        let discussion_group = Discussion::id_to_discussion_chat(&ctx.client, chat.id()).await?;

        let packed_chat = chat.pack();
        let packed_discussion = discussion_group.pack();

        let chat_id = chat.id();
        let channel_hex = packed_chat.to_hex();
        let discussion_hex = packed_discussion.to_hex();

        sqlx::query!(
            r#"
                insert into "chat"
                values ($1, $2, $3);
            "#,
            chat_id,
            channel_hex,
            discussion_hex,
        )
        .execute(&ctx.db)
        .await?;

        Ok(Self {
            id: chat_id,
            chat,
            discussion_group,
        })
    }

    async fn from_db_record(
        client: &grammers_client::Client,
        chat: grammers_client::types::Chat,
        record: ChatRecord,
    ) -> anyhow::Result<Self> {
        let discussion_group = if let Some(ref g) = record.discussion_group {
            let packed_discussion = grammers_session::PackedChat::from_hex(g)?;
            client.unpack_chat(packed_discussion).await.ok()
        } else {
            None
        };

        Ok(Self {
            id: record.id,
            chat,
            discussion_group,
        })
    }
}

struct ChatRecord {
    id: i64,
    channel: String,
    discussion_group: Option<String>,
}
