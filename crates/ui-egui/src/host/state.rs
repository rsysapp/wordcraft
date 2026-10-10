//! The host page's side of a running app.

use super::bridge::HostBridge;

pub struct HostState {
    pub bridge: HostBridge,
}

impl HostState {
    pub fn new(bridge: HostBridge) -> Self {
        HostState { bridge }
    }
}
