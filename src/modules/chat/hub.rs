//! chat - hub.

use tokio::sync::broadcast;
use super::{limits::*, types::*, state::*, helpers::*, module::*, handlers_channels::*, handlers_messages::*, handlers_files::*, handlers_direct::*, handlers_presence::*, handlers_websocket::*, support_config::*, support_sessions::*, support_ratings::*, support_agents::*, support_canned::*, support_offline::*};



#[derive(Debug)]
// WebSocket hub
/// The chat WebSocket hub: a tokio broadcast channel. Handlers publish
/// every new message here; WebSocket sessions subscribe and forward events
/// to their clients.
pub struct ChatHub {
    pub(crate) sender: broadcast::Sender<ChatBroadcast>,
}


impl ChatHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(HUB_CAPACITY);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChatBroadcast> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event: ChatBroadcast) {
        let _ = self.sender.send(event);
    }
}
