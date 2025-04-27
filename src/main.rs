mod chat;
mod config;
mod discussion;

use anyhow::Context as _;
use clap::Parser;
use grammers_client::types::dialog;
use grammers_client::{Client, Config, InitParams, SignInError};
use grammers_mtsender::ReconnectionPolicy;
use grammers_session::Session;
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
}

const SESSION_FILE: &str = "cc.session";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::Level::DEBUG.into())
                .from_env_lossy(),
        )
        .init();
    let config = config::Config::parse();

    let (shutdown_tx, mut shutdown_rx1) = tokio::sync::broadcast::channel::<()>(1);
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
    for chat_id in sqlx::query_scalar!(r#"select id from "chat";"#)
        .fetch_all(&db)
        .await?
    {
        let chat = chat::Chat::from_chat_id(&client, &db, chat_id).await?;
        chats.insert_async(chat_id, chat).await.ok();
    }

    let ctx = AppContext {
        client,
        db,
        config: std::sync::Arc::new(config),
        chats: std::sync::Arc::new(chats),
    };

    let (src_chat, dst_chat) = if let Some(chat_pair) =
        sqlx::query_as!(ChatPair, r#"select * from "chat_pair";"#)
            .fetch_optional(&ctx.db)
            .await?
    {
        (
            chat::Chat::from_chat_id(&ctx.client, &ctx.db, chat_pair.src_id).await?,
            chat::Chat::from_chat_id(&ctx.client, &ctx.db, chat_pair.dst_id).await?,
        )
    } else {
        let src_chat =
            inquire::Select::new("Select chat to copy from:", chat_list.clone()).prompt()?;
        let dst_chat = inquire::Select::new("Select chat to copy to:", chat_list).prompt()?;

        (
            chat::Chat::from_chat(&ctx, src_chat.chat).await?,
            chat::Chat::from_chat(&ctx, dst_chat.chat).await?,
        )
    };
    dbg!(src_chat, dst_chat);

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
    loop {
        tokio::select! {
            Ok(update) = ctx.client.next_update() => {
                handle_update(&ctx, update, /* &from_chat.chat */).await;
            },
            _ = tokio::signal::ctrl_c() => {
                shutdown_tx.send(()).ok();
                break;
            },
            _ = &mut terminate => {
                shutdown_tx.send(()).ok();
                break;
            },
        }
    }
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

async fn handle_update(
    ctx: &AppContext,
    update: grammers_client::Update,
    // from_chat: &grammers_client::types::Chat,
) {
    match update {
        // grammers_client::Update::NewMessage(msg)
        //     if !msg.outgoing()
        //         && msg.text().starts_with("/find")
        //         && msg.chat().id() == from_chat.id() =>
        // {
        //     msg.mark_as_read().await.ok();
        //     tracing::info!(target: "cc", query = %msg.text(), "Find query received,");
        //     if let Err(e) = commands::Commands::Find(msg).run(ctx).await {
        //         tracing::error!(target: "cc", "{e}");
        //     }
        // }
        grammers_client::Update::NewMessage(msg)
            if !msg.outgoing()
                && msg
                    .sender()
                    .is_some_and(|c| ctx.config.admins.contains(&c.id()))
                && msg.text().starts_with("/rm") => {}
        _ => {}
    }
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
