#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Chat {
    pub chat: grammers_client::types::Chat,
    pub request: grammers_tl_types::functions::messages::GetHistory,
}

impl Chat {
    pub async fn from_chat(
        ctx: &crate::AppContext,
        chat: grammers_client::types::Chat,
    ) -> anyhow::Result<Self> {
        let chat_id = chat.id();
        if let Some(record) = sqlx::query_as!(
            ChatRecord,
            r#"
                select *
                from "chat"
                where id = $1;
            "#,
            chat_id,
        )
        .fetch_optional(&ctx.db)
        .await?
        {
            Self::from_db_record(&ctx.client, &record).await
        } else {
            Ok(Self::new(chat))
        }
    }

    fn new(chat: grammers_client::types::Chat) -> Self {
        let peer = chat.pack().to_input_peer();
        Self {
            chat,
            request: grammers_tl_types::functions::messages::GetHistory {
                peer,
                offset_id: 1,
                offset_date: 0,
                add_offset: -30,
                limit: 30,
                max_id: 0,
                min_id: 0,
                hash: 0,
            },
        }
    }

    pub async fn from_db_record(
        client: &grammers_client::Client,
        record: &ChatRecord,
    ) -> anyhow::Result<Self> {
        let packed_chat = grammers_client::session::PackedChat::from_hex(&record.channel)?;
        let peer = packed_chat.to_input_peer();
        let chat = client.unpack_chat(packed_chat).await?;

        Ok(Self {
            chat,
            request: grammers_tl_types::functions::messages::GetHistory {
                peer,
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

    // pub async fn chat_record(db: &sqlx::SqlitePool, id: i64) -> anyhow::Result<ChatRecord> {
    //     sqlx::query_as!(
    //         ChatRecord,
    //         r#"
    //             select *
    //             from "chat"
    //             where id = $1;
    //         "#,
    //         id,
    //     )
    //     .fetch_one(db)
    //     .await
    //     .map_err(anyhow::Error::msg)
    // }

    pub async fn chat_records(db: &sqlx::SqlitePool) -> anyhow::Result<Vec<ChatRecord>> {
        sqlx::query_as!(
            ChatRecord,
            r#"
                select *
                from "chat";
            "#,
        )
        .fetch_all(db)
        .await
        .map_err(anyhow::Error::msg)
    }
}

pub struct ChatRecord {
    id: i64,
    channel: String,
    offset_id: i64,
    min_id: i64,
    hash: i64,
}

impl ChatRecord {
    pub fn id(&self) -> i64 {
        self.id
    }
}
