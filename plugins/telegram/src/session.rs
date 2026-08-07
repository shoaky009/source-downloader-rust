use grammers_session::types::{
    ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState,
};
use grammers_session::{BoxFuture, Session, SessionData};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use tokio::sync::Mutex as AsyncMutex;

pub struct FileSession {
    path: PathBuf,
    data: Mutex<SessionData>,
    write_lock: AsyncMutex<()>,
}

#[derive(Serialize, Deserialize)]
struct PersistedSession {
    home_dc: i32,
    dc_options: HashMap<i32, DcOption>,
    peer_infos: HashMap<PeerId, PeerInfo>,
    updates_state: UpdatesState,
}

impl From<SessionData> for PersistedSession {
    fn from(data: SessionData) -> Self {
        Self {
            home_dc: data.home_dc,
            dc_options: data.dc_options,
            peer_infos: data.peer_infos,
            updates_state: data.updates_state,
        }
    }
}

impl From<PersistedSession> for SessionData {
    fn from(data: PersistedSession) -> Self {
        Self {
            home_dc: data.home_dc,
            dc_options: data.dc_options,
            peer_infos: data.peer_infos,
            updates_state: data.updates_state,
        }
    }
}

#[derive(Debug)]
pub struct FileSessionError(String);

impl Display for FileSessionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FileSessionError {}

impl FileSession {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, FileSessionError> {
        let path = path.as_ref().to_path_buf();
        let data = match tokio::fs::read(&path).await {
            Ok(bytes) => postcard::from_bytes::<PersistedSession>(&bytes)
                .map(SessionData::from)
                .map_err(|error| {
                    FileSessionError(format!(
                        "Failed to decode Telegram session '{}': {error}",
                        path.display()
                    ))
                })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SessionData::default()
            }
            Err(error) => {
                return Err(FileSessionError(format!(
                    "Failed to read Telegram session '{}': {error}",
                    path.display()
                )));
            }
        };
        Ok(Self { path, data: Mutex::new(data), write_lock: AsyncMutex::new(()) })
    }

    async fn persist(&self) -> Result<(), FileSessionError> {
        let _guard = self.write_lock.lock().await;
        let bytes = {
            let data = self.data.lock();
            postcard::to_stdvec(&PersistedSession {
                home_dc: data.home_dc,
                dc_options: data.dc_options.clone(),
                peer_infos: data.peer_infos.clone(),
                updates_state: data.updates_state.clone(),
            })
            .map_err(|error| {
                FileSessionError(format!("Failed to encode Telegram session: {error}"))
            })?
        };
        tokio::fs::write(&self.path, bytes).await.map_err(|error| {
            FileSessionError(format!(
                "Failed to persist Telegram session '{}': {error}",
                self.path.display()
            ))
        })
    }
}

impl Session for FileSession {
    type Error = FileSessionError;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        Ok(self.data.lock().home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            self.data.lock().home_dc = dc_id;
            self.persist().await
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        Ok(self.data.lock().dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(
        &self,
        dc_option: &DcOption,
    ) -> BoxFuture<'_, Result<(), Self::Error>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            self.data.lock().dc_options.insert(dc_option.id, dc_option);
            self.persist().await
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move { Ok(self.data.lock().peer_infos.get(&peer).cloned()) })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        let peer = peer.clone();
        Box::pin(async move {
            self.data
                .lock()
                .peer_infos
                .entry(peer.id())
                .or_insert_with(|| peer.clone())
                .extend_info(&peer);
            self.persist().await
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async move { Ok(self.data.lock().updates_state.clone()) })
    }

    fn set_update_state(
        &self,
        update: UpdateState,
    ) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            {
                let mut data = self.data.lock();
                match update {
                    UpdateState::All(state) => data.updates_state = state,
                    UpdateState::Primary { pts, date, seq } => {
                        data.updates_state.pts = pts;
                        data.updates_state.date = date;
                        data.updates_state.seq = seq;
                    }
                    UpdateState::Secondary { qts } => data.updates_state.qts = qts,
                    UpdateState::Channel { id, pts } => {
                        data.updates_state.channels.retain(|state| state.id != id);
                        data.updates_state.channels.push(ChannelState { id, pts });
                    }
                }
            }
            self.persist().await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persists_home_dc() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telegram.session");
        let session = FileSession::open(&path).await.unwrap();
        session.set_home_dc_id(4).await.unwrap();
        drop(session);
        assert_eq!(FileSession::open(path).await.unwrap().home_dc_id().unwrap(), 4);
    }
}
