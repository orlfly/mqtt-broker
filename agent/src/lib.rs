//! `agent` library — the building blocks the merged `app`
//! binary needs to start the voice loop with the in-process
//! broker tools wired in.
//!
//! Originally the agent was a single binary; once the MQTT
//! broker was merged into the same process (see the `app`
//! crate), the agent became a library: its `main` moved
//! out, and the bits that other crates need (the voice
//! loop, the broker tools, the follow-up classifier) are
//! re-exported from here.

pub mod broker_tools;
pub mod voice_loop;
pub mod voice_text;

pub use broker_tools::{
    DrainMessagesTool, GetTopicSubscribersTool, ListClientsTool, ListTopicsTool,
    PublishTool, SubscribeTool, UnsubscribeTool,
};
pub use voice_loop::{FollowupClassifier, VoiceLoop, VoiceLoopConfig};
pub use voice_text::transform_for_tts;
