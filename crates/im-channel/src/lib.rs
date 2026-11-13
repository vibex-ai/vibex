//! IM chat channel host.
//!
//! The service owns channel configuration, pairing, session bindings, approval
//! actions, and the Telegram transport. It talks to the authoritative Agent
//! runtime through [`ImAgentPort`] so it can be tested without ACP processes.

mod adapter;
mod agent_port;
mod error;
mod i18n;
mod lark;
mod render;
mod service;
mod telegram;
mod types;

pub use adapter::ImAdapter;
pub use agent_port::ImAgentPort;
pub use error::{ImError, ImResult};
pub use i18n::ImLocale;
pub use lark::LarkAdapter;
pub use service::{ImChannelService, ImChannelServiceConfig};
pub use telegram::TelegramAdapter;
pub use types::{
    ImButton, ImCallback, ImChatType, ImInboundEvent, ImSentMessage, ImTarget, ImThreadTarget,
};
