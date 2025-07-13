mod chat;
mod config;
mod discussion;
mod message;

use anyhow::Context as _;
use clap::Parser;
use futures::StreamExt;
use grammers_client::{Client, Config, InitParams, SignInError};
use grammers_mtsender::ReconnectionPolicy;
use grammers_session::Session;
use std::str::FromStr;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
struct Environment {
    #[clap(long, env)]
    group_chat_id: i64,
    #[clap(long, env)]
    bot_chat_id: i64,
}

struct MyPolicy;

impl ReconnectionPolicy for MyPolicy {
    fn should_retry(&self, attempts: usize) -> std::ops::ControlFlow<(), std::time::Duration> {
        let duration = std::cmp::min(u64::pow(2, attempts as _), 10_000);
        std::ops::ControlFlow::Continue(std::time::Duration::from_millis(duration))
    }
}

#[derive(Clone)]
pub struct AppContext {
    client: grammers_client::Client,
    db: sqlx::SqlitePool,
    config: std::sync::Arc<config::Config>,
    chats: std::sync::Arc<scc::HashMap<i64, chat::Chat>>,
    progress: indicatif::MultiProgress,
    download_tx: tokio::sync::mpsc::Sender<message::MessageParsed>,
}

const SESSION_FILE: &str = "cc.session";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let now = time::OffsetDateTime::now_local()
        .unwrap_or(time::OffsetDateTime::now_utc())
        .format(&time::format_description::parse(
            "[year]-[month]-[day]T[hour]_[minute]_[second]",
        )?)?;
    let mut log_path = std::path::PathBuf::from_str("logs")?;
    log_path.push(now);
    std::fs::create_dir_all(&log_path)?;
    let log_file = {
        log_path.push("debug.log");
        std::fs::File::create(&log_path)?
    };
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(tracing_subscriber::fmt::format().pretty())
                .with_ansi(false)
                .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339())
                .with_writer(log_file),
        )
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::Level::DEBUG.into())
                .from_env_lossy(),
        )
        .init();

    let config = config::Config::parse();

    let (shutdown_tx, mut shutdown_rx1) = tokio::sync::broadcast::channel::<()>(1);
    let (download_tx, mut download_rx) =
        tokio::sync::mpsc::channel::<message::MessageParsed>(config.concurrency);
    let (send_tx, mut send_rx) = tokio::sync::mpsc::channel::<(
        grammers_client::types::Chat,
        grammers_client::types::InputMessage,
        i32,
        i64,
    )>(1);
    let mut tasks = tokio::task::JoinSet::<()>::new();

    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(50)
        .connect(&config.database_url)
        .await
        .context("could not connect to database_url")?;

    tracing::info!(target: "cc", "Connecting to Telegram...");
    let client = Client::connect(Config {
        session: Session::load_file_or_create(SESSION_FILE)?,
        api_id: config.api_id,
        api_hash: config.api_hash.clone(),
        params: InitParams {
            reconnection_policy: &MyPolicy,
            ..Default::default()
        },
    })
    .await?;
    tracing::info!(target: "cc", "Connected!");

    let mut sign_out = false;

    if !client.is_authorized().await? {
        tracing::info!(target: "cc", "Signing in...");
        let phone =
            inquire::Text::new("Enter your phone number (international format):").prompt()?;
        let token = client.request_login_code(&phone).await?;
        let mut text = "Enter code you received:";
        loop {
            let code = inquire::Text::new(text).prompt()?;
            let signed_in = client.sign_in(&token, &code).await;
            match signed_in {
                Ok(_) => break,
                Err(err) => match err {
                    SignInError::PasswordRequired(password_token) => {
                        let hint = password_token.hint().unwrap_or("-");
                        let password = inquire::Password::new(&format!(
                            "Enter the password (hint {}): ",
                            &hint
                        ))
                        .prompt()?;

                        client
                            .check_password(password_token, password.trim())
                            .await?;
                        break;
                    }
                    SignInError::InvalidCode => {
                        text = "Invalid code, please try again:";
                        continue;
                    }
                    _ => panic!("{}", err),
                },
            }
        }
        tracing::info!(target: "cc", "Signed in!");
        if let Err(err) = client.session().save_to_file(SESSION_FILE) {
            tracing::warn!(
                target: "cc",
                cause = %err.to_string(),
                "Failed to save the session, will sign out when done,"
            );
            sign_out = true;
        }
    }
    let mut chat_list = Vec::new();
    let mut dialogues = client.iter_dialogs();
    while let Some(dialogue) = dialogues.next().await? {
        let chat = dialogue.chat;
        chat_list.push(Dialogue { chat });
    }

    let chats = scc::HashMap::new();
    for record in chat::Chat::chat_records(&db).await? {
        let chat = chat::Chat::from_db_record(&client, &record).await?;
        chats.insert_async(record.id(), chat).await.ok();
    }

    let ctx = AppContext {
        client,
        db,
        config: std::sync::Arc::new(config),
        chats: std::sync::Arc::new(chats),
        progress: indicatif::MultiProgress::new(),
        download_tx,
    };

    let (mut src_chat, dst_chat) = if let Some(chat_pair) =
        sqlx::query_as!(ChatPair, r#"select * from "chat_pair";"#)
            .fetch_optional(&ctx.db)
            .await?
    {
        (
            ctx.chats.get(&chat_pair.src_id).unwrap().clone(),
            ctx.chats.get(&chat_pair.dst_id).unwrap().clone(),
        )
    } else {
        let src_chat =
            inquire::Select::new("Select chat to copy from:", chat_list.clone()).prompt()?;
        let dst_chat =
            inquire::Select::new("Select chat to copy to:", chat_list.clone()).prompt()?;

        (
            chat::Chat::from_chat(&ctx, src_chat.chat).await?,
            chat::Chat::from_chat(&ctx, dst_chat.chat).await?,
        )
    };

    let ctx2 = ctx.clone();
    tasks.spawn(async move {
        let mut buffer = Vec::with_capacity(ctx2.config.concurrency as usize);
        loop {
            tokio::select! {
                _ = download_rx.recv_many(&mut buffer, ctx2.config.concurrency as usize) => {
                    let mut tasks = futures::stream::FuturesOrdered::new();
                    for mut msg in buffer.drain(..) {
                        if sqlx::query_scalar!(
                            r#"
                                select id from "message"
                                where id = $1
                                  and forwarded = 1;
                            "#,
                            msg.id[0],
                        )
                        .fetch_optional(&ctx2.db)
                        .await
                        .is_ok_and(|d| d.is_some()) {
                            continue;
                        }

                        let client = ctx2.client.clone();
                        let progress = ctx2.progress.clone();
                        tasks.push_back(async move {
                            let pb = progress.add(indicatif::ProgressBar::new_spinner());
                            pb.enable_steady_tick(std::time::Duration::from_millis(500));

                            match msg.data {
                                message::MessageType::MultiMedia(ref items) => {
                                    for (idx, item) in items.iter().enumerate() {
                                        pb.set_message(format!("downloading file {}", item.file_name));
                                        if let Ok(download_path) = download_with_retry(&client, item, &msg, idx).await {
                                            msg.file_paths.insert(idx, download_path);
                                        }
                                    }
                                },
                                message::MessageType::SingleMedia(ref item) => {
                                    pb.set_message(format!("downloading file {}", item.file_name));
                                    if let Ok(download_path) = download_with_retry(&client, item, &msg, 0).await {
                                        msg.file_paths.insert(0, download_path);
                                    }
                                }
                                _ => unreachable!(),
                            }

                            pb.finish_and_clear();
                            progress.remove(&pb);
                            msg
                        });
                    }

                    for msgs in tasks.collect::<Vec<_>>().await {
                        let pb = ctx2.progress.add(indicatif::ProgressBar::new_spinner());
                        pb.enable_steady_tick(std::time::Duration::from_millis(500));

                        for (idx, file_path) in msgs.file_paths.iter().enumerate() {
                            pb.set_message(format!("uploading file {}", file_path));
                            let uploaded_file = match upload_with_retry(&ctx2.client, file_path).await {
                                Ok(f) => f,
                                Err(_) => continue,
                            };
                            tokio::fs::remove_dir_all(file_path).await.ok();
                            pb.finish_and_clear();

                            let msg_media = match msgs.data {
                                message::MessageType::SingleMedia(ref media) => media,
                                message::MessageType::MultiMedia(ref medias) if !medias.is_empty() => &medias[idx],
                                _ => unreachable!(),
                            };
                            let input_message = grammers_client::types::InputMessage::text(&msg_media.caption).reply_to(Some(msgs.post_id)).fmt_entities(msg_media.entities.clone());
                            let input_message = match msg_media.mime {
                                message::MimeType::Photo => input_message.photo(uploaded_file),
                                _ => input_message.document(uploaded_file),
                            };

                            send_tx.send((msgs.dst_chat.clone(), input_message, msgs.id[idx], msgs.src_chat_id)).await.ok();
                            ctx2.progress.remove(&pb);
                        }
                    }
                }
                _ = shutdown_rx1.recv() => {
                    tracing::info!(target: "cc", "Stopping downloading files");
                    break;
                }
            }
        }
    });

    let ctx3 = ctx.clone();
    let mut shutdown_rx2 = shutdown_tx.subscribe();
    tasks.spawn(async move {
        let mut buffer = Vec::with_capacity(5);
        loop {
            tokio::select! {
                _ = send_rx.recv_many(&mut buffer, 5) => {
                    buffer.sort_by(|a: &(_, _, i32, _), b: &(_, _, i32, _)| (a.2).cmp(&b.2));
                    for (chat, msg, msg_id, chat_id) in buffer.drain(..) {
                        match send_with_retry(&ctx3.client, &chat, msg).await {
                            Ok(_) => {
                                sqlx::query!(
                                    r#"
                                        insert into "message"
                                        values ($1, $2, $3, $4)
                                        on conflict do update set forwarded = excluded.forwarded;
                                    "#,
                                    msg_id,
                                    chat_id,
                                    1,
                                    1,
                                )
                                .execute(&ctx3.db)
                                .await
                                .ok();
                            }
                            Err(_) => continue,
                        }
                    }
                }
                _ = shutdown_rx2.recv() => {
                    tracing::info!(target: "cc", "Stopping downloading files");
                    break;
                }
            }
        }
    });

    parse_chat(&ctx, &mut src_chat, &dst_chat).await?;

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::pin!(terminate);

    tracing::info!(target: "cc", "Watching for new messages...");
    // loop {
    tokio::select! {
        // Ok(update) = ctx.client.next_update() => {
        //     handle_update(&ctx, update, /* &from_chat.chat */).await;
        // },
        _ = tokio::signal::ctrl_c() => {
            shutdown_tx.send(()).ok();
            // break;
        },
        _ = &mut terminate => {
            shutdown_tx.send(()).ok();
            // break;
        },
    }
    // }
    println!("_SIGTERM");
    tracing::info!(target: "cc", "Gracefully finishing all tasks...");
    tasks.join_all().await;

    tracing::info!(target: "cc", "Saving session...");
    if let Err(err) = ctx.client.session().save_to_file(SESSION_FILE) {
        tracing::warn!(
            target: "cc",
            cause = %err.to_string(),
            "Failed to save the session, will sign out when done,"
        );
        sign_out = true;
    }

    if sign_out {
        ctx.client.sign_out_disconnect().await?;
    }

    Ok(())
}

// async fn handle_update(
//     ctx: &AppContext,
//     update: grammers_client::Update,
//     // from_chat: &grammers_client::types::Chat,
// ) {
//     match update {
//         // grammers_client::Update::NewMessage(msg)
//         //     if !msg.outgoing()
//         //         && msg.text().starts_with("/find")
//         //         && msg.chat().id() == from_chat.id() =>
//         // {
//         //     msg.mark_as_read().await.ok();
//         //     tracing::info!(target: "cc", query = %msg.text(), "Find query received,");
//         //     if let Err(e) = commands::Commands::Find(msg).run(ctx).await {
//         //         tracing::error!(target: "cc", "{e}");
//         //     }
//         // }
//         grammers_client::Update::NewMessage(msg)
//             if !msg.outgoing()
//                 && msg
//                     .sender()
//                     .is_some_and(|c| ctx.config.admins.contains(&c.id()))
//                 && msg.text().starts_with("/rm") => {}
//         _ => {}
//     }
// }

async fn parse_chat(
    ctx: &AppContext,
    src_chat: &mut chat::Chat,
    dst_chat: &chat::Chat,
) -> anyhow::Result<()> {
    let mut count = 0;
    let pb = ctx.progress.add(create_progress_bar(0));

    loop {
        match ctx.client.invoke(&src_chat.request).await {
            Ok(grammers_tl_types::enums::messages::Messages::Messages(msgs))
                if !msgs.messages.is_empty() =>
            {
                let messages = msgs
                    .messages
                    .into_iter()
                    .filter(|m| matches!(m, grammers_tl_types::enums::Message::Message(m) if m.via_bot_id.is_none() && matches!(&m.replies, Some(grammers_tl_types::enums::MessageReplies::Replies(r)) if r.replies > 0)))
                    .collect();
                count += copy_messages(ctx, &pb, src_chat, dst_chat, messages).await?;
            }
            Ok(grammers_tl_types::enums::messages::Messages::ChannelMessages(msgs))
                if !msgs.messages.is_empty() =>
            {
                let messages = msgs
                    .messages
                    .into_iter()
                    .filter(|m| matches!(m, grammers_tl_types::enums::Message::Message(m) if m.via_bot_id.is_none() && matches!(&m.replies, Some(grammers_tl_types::enums::MessageReplies::Replies(r)) if r.replies > 0)))
                    .collect();
                count += copy_messages(ctx, &pb, src_chat, dst_chat, messages).await?;
            }
            Ok(_) => {
                tracing::info!(target: "cc", "All messages were parsed");
                break;
            }
            Err(err) => match err {
                grammers_client::InvocationError::Rpc(rpc_error)
                    if rpc_error.name == "FLOOD_WAIT" =>
                {
                    tracing::error!(target: "cc", "Error fetching messages: {rpc_error}");
                    let wait_time = rpc_error.value.unwrap_or(60) as u64;

                    pb.set_message(format!(
                        "Flood wait encountered - sleeping for {wait_time}s...💤"
                    ));
                    tokio::time::sleep(tokio::time::Duration::from_secs(wait_time)).await;
                }
                err => {
                    tracing::error!(target: "cc", "Unrecoverable error fetching messages: {err}");
                    return Ok(());
                }
            },
        }
    }
    pb.finish_with_message(format!(
        "Collected {} messages from chat {}",
        count,
        src_chat.chat.id()
    ));

    Ok(())
}

async fn copy_messages(
    ctx: &AppContext,
    pb: &indicatif::ProgressBar,
    src_chat: &mut chat::Chat,
    dst_chat: &chat::Chat,
    msgs: Vec<grammers_tl_types::enums::Message>,
) -> anyhow::Result<i32> {
    pb.inc_length(msgs.len() as u64);

    let mut count = 0;
    let offset_id = msgs.first().map(|m| m.id()).unwrap_or_default() + 1;
    let hash = msgs.iter().map(|m| m.id() as i64).fold(0, |mut acc, id| {
        acc ^= acc >> 21;
        acc ^= acc << 35;
        acc ^= acc >> 4;
        acc += id;

        acc
    });

    let chat_id = src_chat.chat.id();
    let mut iter = msgs.into_iter();
    while let Some(msg) = iter.next_back() {
        if let grammers_tl_types::enums::Message::Message(msg) = msg {
            let discussion = match discussion::Discussion::get_post_discussion(
                &ctx.client,
                &ctx.db,
                &src_chat.chat,
                msg.id,
            )
            .await
            {
                Ok(d) => d,
                Err(_) => {
                    pb.inc(1);

                    continue;
                }
            };

            let downloaded = msg.media.is_some();
            let input_message = if let Some(media) = msg.media {
                let media = grammers_client::types::Media::from_raw(media).unwrap();
                grammers_client::InputMessage::text(msg.message)
                    .copy_media(&media)
                    .fmt_entities(msg.entities.unwrap_or_default())
            } else {
                grammers_client::InputMessage::text(msg.message)
                    .fmt_entities(msg.entities.unwrap_or_default())
            };

            match send_with_retry(&ctx.client, &dst_chat.chat, input_message.clone()).await {
                Ok(sent_message) => {
                    pb.inc(1);

                    sqlx::query!(
                        r#"
                            insert into "message"
                            values ($1, $2, $3, $4)
                            on conflict do update set forwarded = excluded.forwarded;
                        "#,
                        msg.id,
                        chat_id,
                        downloaded,
                        1,
                    )
                    .execute(&ctx.db)
                    .await?;

                    count += 1;
                    message::Message::process_new_messages(
                        ctx,
                        discussion,
                        // src_chat.chat.id(),
                        &dst_chat.chat,
                        sent_message.id(),
                    )
                    .await?;
                }
                Err(err) => return Err(err),
            }
        }
    }

    src_chat.request.offset_id = offset_id;
    src_chat.request.min_id = offset_id;

    src_chat.request.hash = hash;

    sqlx::query!(
        r#"
            update "chat"
            set offset_id   = $2,
                min_id      = $3,
                hash        = $4
            where id = $1;
        "#,
        chat_id,
        src_chat.request.offset_id,
        src_chat.request.min_id,
        src_chat.request.hash,
    )
    .execute(&ctx.db)
    .await?;

    Ok(count)
}

fn create_progress_bar(len: u64) -> indicatif::ProgressBar {
    indicatif::ProgressBar::new(len).with_style(
        indicatif::ProgressStyle::default_bar()
            .template("{msg}\n[{elapsed_precise}] [{wide_bar:.cyan/blue}] ({pos:>7}/{len:7}, ETA: {eta_precise})").unwrap()
            .progress_chars("#>-")
    )
}

#[derive(Clone)]
struct Dialogue {
    chat: grammers_client::types::Chat,
}

impl std::fmt::Display for Dialogue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}) [{}]",
            self.chat.name().unwrap_or("-"),
            self.chat.username().unwrap_or("-"),
            self.chat.id(),
        )
    }
}

struct ChatPair {
    src_id: i64,
    dst_id: i64,
}

pub async fn invoke_with_retry<R>(
    client: &grammers_client::Client,
    request: &R,
) -> Result<R::Return, grammers_client::InvocationError>
where
    R: grammers_tl_types::RemoteCall,
{
    loop {
        let res: Result<R::Return, grammers_client::InvocationError> = match client
            .invoke(request)
            .await
        {
            Ok(r) => Ok(r),
            Err(err) => match err {
                grammers_client::InvocationError::Rpc(rpc_error)
                    if rpc_error.name == "FLOOD_WAIT" =>
                {
                    let wait = rpc_error.value.unwrap_or(60) as u64;
                    tracing::warn!(target: "cc", "FLOOD_WAIT {wait}s while invoking request; retrying");

                    tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
                    continue;
                }
                err => {
                    tracing::error!(target: "cc", "Unrecoverable error invoking request: {err}");

                    return Err(err);
                }
            },
        };

        return res;
    }
}

pub async fn download_with_retry(
    client: &grammers_client::Client,
    item: &crate::message::Media,
    msg: &message::MessageParsed,
    idx: usize,
) -> anyhow::Result<String> {
    loop {
        let dir_path = format!(".tmp/{}/{}", msg.src_chat_id, msg.post_id);
        tracing::debug!(target: "cc", "Start downloading file {}", msg.id[idx]);
        let download_path = format!("{}/{}", dir_path, item.file_name);
        match client.download_media(&*item.media, &download_path).await {
            Ok(_) => {
                tracing::debug!(target: "cc", "Finished downloading file {}", download_path);

                return Ok(download_path);
            }
            Err(cause) => {
                if let Some(inv) = cause
                    .get_ref()
                    .and_then(|boxed| boxed.downcast_ref::<grammers_client::InvocationError>())
                {
                    if let grammers_client::InvocationError::Rpc(rpc) = inv {
                        if rpc.name == "FLOOD_WAIT" {
                            let wait = rpc.value.unwrap_or(60) as u64;
                            tracing::warn!(target:"cc", "FLOOD_WAIT {wait}s while downloading; retrying");
                            tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
                        }
                    } else {
                        tracing::error!(target: "cc", "Unrecoverable error downloading file: {cause}");
                        return Err(anyhow::Error::msg(cause));
                    }
                } else {
                    tracing::error!(target: "cc", "Unrecoverable error downloading file: {cause}");
                    return Err(anyhow::Error::msg(cause));
                }
            }
        }
    }
}

pub async fn upload_with_retry(
    client: &grammers_client::Client,
    file_path: &str,
) -> anyhow::Result<grammers_client::types::media::Uploaded> {
    loop {
        match client.upload_file(file_path).await {
            Ok(media) => return Ok(media),
            Err(cause) => {
                if let Some(inv) = cause
                    .get_ref()
                    .and_then(|boxed| boxed.downcast_ref::<grammers_client::InvocationError>())
                {
                    if let grammers_client::InvocationError::Rpc(rpc) = inv {
                        if rpc.name == "FLOOD_WAIT" {
                            let wait = rpc.value.unwrap_or(60) as u64;
                            tracing::warn!(target:"cc", "FLOOD_WAIT {wait}s while uploading; retrying");
                            tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
                        }
                    } else {
                        tracing::error!(target: "cc", "Unrecoverable error uploading file: {cause}");
                        return Err(anyhow::Error::msg(cause));
                    }
                }
            }
        }
    }
}

pub async fn send_with_retry(
    client: &grammers_client::Client,
    chat: &grammers_client::types::Chat,
    msg: grammers_client::types::InputMessage,
) -> anyhow::Result<grammers_client::types::Message> {
    loop {
        match client.send_message(chat, msg.clone()).await {
            Ok(msg) => return Ok(msg),
            Err(err) => match err {
                grammers_client::InvocationError::Rpc(rpc_error)
                    if rpc_error.name == "FLOOD_WAIT" =>
                {
                    let wait = rpc_error.value.unwrap_or(60) as u64;
                    tracing::warn!(target:"cc", "FLOOD_WAIT {wait}s while sending message; retrying");
                    tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;
                }
                err => {
                    tracing::error!(target: "cc", "Unrecoverable error sending message: {err}");

                    return Err(anyhow::Error::msg(err));
                }
            },
        }
    }
}
