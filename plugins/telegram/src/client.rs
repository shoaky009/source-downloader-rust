use crate::session::FileSession;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use grammers_client::Client;
use grammers_client::sender::{ConnectionParams, SenderPool, SenderPoolHandle};
use grammers_client::session::Session;
use grammers_client::tl;
use qrcode::QrCode;
use serde::Deserialize;
use source_downloader_sdk::component::{ComponentError, ProcessingError};
use source_downloader_sdk::instance::InstanceFactory;
use source_downloader_sdk::serde_json::{Map, Value};
use std::any::{Any, TypeId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TelegramClientConfig {
    pub api_id: i32,
    pub api_hash: String,
    pub metadata_path: PathBuf,
    pub proxy: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_timeout() -> u64 {
    5
}

pub struct TelegramClientInstanceFactory;
pub const INSTANCE_FACTORY: TelegramClientInstanceFactory = TelegramClientInstanceFactory;

impl InstanceFactory for TelegramClientInstanceFactory {
    fn create_instance(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn Any + Send + Sync>, ComponentError> {
        let config =
            source_downloader_sdk::serde_json::from_value::<TelegramClientConfig>(
                Value::Object(props.clone()),
            )
            .map_err(|error| {
                ComponentError::new(format!("Invalid Telegram client config: {error}"))
            })?;
        if config.api_hash.trim().is_empty() {
            return Err(ComponentError::new("Telegram 'api-hash' must not be empty"));
        }
        if let Some(proxy) = &config.proxy
            && !proxy.starts_with("socks5://")
        {
            return Err(ComponentError::new(
                "grammers supports only socks5:// Telegram proxies",
            ));
        }
        Ok(Arc::new(TelegramClientInstance { config, connected: OnceCell::new() }))
    }

    fn instance_type_id(&self) -> TypeId {
        TypeId::of::<TelegramClientInstance>()
    }
}

pub struct TelegramClientInstance {
    config: TelegramClientConfig,
    connected: OnceCell<ConnectedClient>,
}

impl TelegramClientInstance {
    pub async fn client(&self) -> Result<Client, ProcessingError> {
        let connected = self
            .connected
            .get_or_try_init(|| ConnectedClient::connect(&self.config))
            .await?;
        Ok(connected.client.clone())
    }

    pub async fn chat(
        &self,
        chat_id: i64,
    ) -> Result<(grammers_client::session::types::PeerRef, String), ProcessingError> {
        let connected = self
            .connected
            .get_or_try_init(|| ConnectedClient::connect(&self.config))
            .await?;
        connected.chat(chat_id).await
    }

    #[cfg(test)]
    pub(crate) fn disconnected(config: TelegramClientConfig) -> Self {
        Self { config, connected: OnceCell::new() }
    }
}

struct ConnectedClient {
    client: Client,
    session: Arc<FileSession>,
    handle: SenderPoolHandle,
    runner_task: JoinHandle<()>,
    updates_task: JoinHandle<()>,
}

impl ConnectedClient {
    async fn connect(config: &TelegramClientConfig) -> Result<Self, ProcessingError> {
        tokio::fs::create_dir_all(&config.metadata_path).await?;
        let session_path = config.metadata_path.join("telegram.session");
        let session =
            Arc::new(FileSession::open(session_path).await.map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Failed to open Telegram session: {error}"
                ))
            })?);
        let connection = ConnectionParams {
            proxy_url: config.proxy.clone(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            ..Default::default()
        };
        let SenderPool { runner, handle, mut updates } =
            SenderPool::with_configuration(session.clone(), config.api_id, connection);
        let thin_handle = handle.thin.clone();
        let client = Client::new(handle);
        let runner_task = tokio::spawn(runner.run());

        let authorized = tokio::time::timeout(
            Duration::from_secs(config.timeout),
            client.is_authorized(),
        )
        .await
        .map_err(|_| {
            ProcessingError::retryable("Telegram authorization check timed out")
        })?
        .map_err(telegram_error)?;

        if !authorized {
            qr_login(&client, session.as_ref(), &thin_handle, config, &mut updates)
                .await?;
        }
        let updates_task =
            tokio::spawn(async move { while updates.recv().await.is_some() {} });
        tracing::info!("Telegram client connected and authorized");
        Ok(Self { client, session, handle: thin_handle, runner_task, updates_task })
    }

    async fn chat(
        &self,
        chat_id: i64,
    ) -> Result<(grammers_client::session::types::PeerRef, String), ProcessingError> {
        use grammers_client::session::types::PeerId;

        let id = if chat_id < 0 {
            PeerId::channel(chat_id.unsigned_abs() as i64)
        } else {
            PeerId::chat(chat_id)
        }
        .ok_or_else(|| ProcessingError::non_retryable("Invalid Telegram chat ID"))?;
        if let Some(reference) = self.session.peer_ref(id).await.map_err(session_error)? {
            let peer =
                self.client.resolve_peer(reference).await.map_err(telegram_error)?;
            return Ok((reference, peer.name().unwrap_or(&id.to_string()).to_string()));
        }

        let mut dialogs = self.client.iter_dialogs();
        while let Some(dialog) = dialogs.next().await.map_err(telegram_error)? {
            if dialog.peer_id() == id {
                return Ok((
                    dialog.peer_ref(),
                    dialog.peer().name().unwrap_or(&id.to_string()).to_string(),
                ));
            }
        }
        Err(ProcessingError::non_retryable(format!(
            "Telegram chat {chat_id} is not present in the account dialogs",
        )))
    }
}

impl Drop for ConnectedClient {
    fn drop(&mut self) {
        self.handle.quit();
        self.updates_task.abort();
        self.runner_task.abort();
    }
}

async fn qr_login(
    client: &Client,
    session: &FileSession,
    handle: &SenderPoolHandle,
    config: &TelegramClientConfig,
    updates: &mut tokio::sync::mpsc::UnboundedReceiver<
        grammers_client::session::updates::UpdatesLike,
    >,
) -> Result<(), ProcessingError> {
    loop {
        let result = client
            .invoke(&tl::functions::auth::ExportLoginToken {
                api_id: config.api_id,
                api_hash: config.api_hash.clone(),
                except_ids: Vec::new(),
            })
            .await
            .map_err(telegram_error)?;
        match result {
            tl::enums::auth::LoginToken::Token(token) => {
                print_qr(&token.token)?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let wait = Duration::from_secs(
                    (i64::from(token.expires) - now as i64).max(1) as u64,
                );
                tracing::info!(
                    expires_in = wait.as_secs(),
                    "Scan Telegram login QR code"
                );
                let _ = tokio::time::timeout(wait, updates.recv()).await;
            }
            tl::enums::auth::LoginToken::MigrateTo(migration) => {
                let old_dc = session.home_dc_id().map_err(session_error)?;
                let imported = client
                    .invoke_in_dc(
                        migration.dc_id,
                        &tl::functions::auth::ImportLoginToken { token: migration.token },
                    )
                    .await
                    .map_err(telegram_error)?;
                session.set_home_dc_id(migration.dc_id).await.map_err(session_error)?;
                handle.disconnect_from_dc(old_dc);
                if finish_qr_login(imported)? {
                    return Ok(());
                }
            }
            tl::enums::auth::LoginToken::Success(success) => {
                if finish_qr_login(success.into())? {
                    return Ok(());
                }
            }
        }
    }
}

fn finish_qr_login(result: tl::enums::auth::LoginToken) -> Result<bool, ProcessingError> {
    let tl::enums::auth::LoginToken::Success(success) = result else {
        return Ok(false);
    };
    if !matches!(success.authorization, tl::enums::auth::Authorization::Authorization(_))
    {
        return Err(ProcessingError::non_retryable(
            "Telegram sign-up must be completed in an official client",
        ));
    }
    tracing::info!("Telegram QR login completed");
    Ok(true)
}

fn print_qr(token: &[u8]) -> Result<(), ProcessingError> {
    let url = format!("tg://login?token={}", URL_SAFE_NO_PAD.encode(token));
    let code = QrCode::new(url.as_bytes()).map_err(|error| {
        ProcessingError::non_retryable(format!(
            "Failed to render Telegram QR code: {error}"
        ))
    })?;
    println!(
        "{}",
        code.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build()
    );
    Ok(())
}

pub fn telegram_error(error: grammers_client::InvocationError) -> ProcessingError {
    match &error {
        grammers_client::InvocationError::Rpc(rpc) if rpc.code == 420 => {
            ProcessingError::retryable(format!("Telegram flood wait: {rpc}"))
        }
        _ => ProcessingError::retryable(format!("Telegram request failed: {error}")),
    }
}

fn session_error(error: impl std::fmt::Display) -> ProcessingError {
    ProcessingError::non_retryable(format!("Telegram session failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_validates_required_configuration_and_proxy() {
        assert!(INSTANCE_FACTORY.create_instance(&Map::new()).is_err());
        let props = Map::from_iter([
            ("api-id".into(), Value::from(1)),
            ("api-hash".into(), Value::String("hash".into())),
            ("metadata-path".into(), Value::String("session".into())),
            ("proxy".into(), Value::String("http://localhost:8080".into())),
        ]);
        assert!(INSTANCE_FACTORY.create_instance(&props).is_err());
    }
}
