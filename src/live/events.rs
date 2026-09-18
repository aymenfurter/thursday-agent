//! Server events of the GPT-Live WebSocket protocol that this app acts on.
//! Everything else parses as `Other`.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct Delegation {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum LiveEvent {
    #[serde(rename = "session.started")]
    SessionStarted,
    #[serde(rename = "session.output_audio.delta")]
    OutputAudioDelta { delta: String },
    #[serde(rename = "session.input_transcript.delta")]
    InputTranscriptDelta {
        delta: String,
        #[serde(default)]
        start_ms: Option<u64>,
        #[serde(default)]
        end_ms: Option<u64>,
    },
    #[serde(rename = "session.output_transcript.delta")]
    OutputTranscriptDelta {
        delta: String,
        #[serde(default)]
        start_ms: Option<u64>,
        #[serde(default)]
        end_ms: Option<u64>,
    },
    #[serde(rename = "session.delegation.created")]
    DelegationCreated { delegation: Delegation },
    #[serde(rename = "session.usage.updated")]
    UsageUpdated {
        #[serde(default)]
        usage: Value,
    },
    #[serde(rename = "session.closed")]
    SessionClosed {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        usage: Option<Value>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        error: Value,
    },
    #[serde(other)]
    Other,
}

impl LiveEvent {
    pub fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delegation_created() {
        let raw = r#"{"type":"session.delegation.created","event_id":"e","offset_ms":1000,
            "delegation":{"id":"item_1","type":"delegation","target":"client"}}"#;
        match LiveEvent::parse(raw).unwrap() {
            LiveEvent::DelegationCreated { delegation } => assert_eq!(delegation.id, "item_1"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_events_do_not_fail() {
        let raw = r#"{"type":"session.something_new","foo":1}"#;
        assert!(matches!(LiveEvent::parse(raw).unwrap(), LiveEvent::Other));
    }

    #[test]
    fn parses_audio_delta() {
        let raw = r#"{"type":"session.output_audio.delta","delta":"AAAA","start_ms":0,"end_ms":20}"#;
        assert!(matches!(
            LiveEvent::parse(raw).unwrap(),
            LiveEvent::OutputAudioDelta { .. }
        ));
    }
}
