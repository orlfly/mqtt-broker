use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::traits::AsrTranscriber;

pub struct AsrEngine {
    #[allow(dead_code)]
    model_path: String,
    #[allow(dead_code)]
    sample_rate: u32,
}

impl AsrEngine {
    pub fn new(model_path: &str, sample_rate: u32) -> Self {
        Self {
            model_path: model_path.to_string(),
            sample_rate,
        }
    }

    pub fn init(model_path: &str) -> anyhow::Result<Self> {
        tracing::info!("Initializing ASR engine with model: {}", model_path);
        Ok(Self {
            model_path: model_path.to_string(),
            sample_rate: 16000,
        })
    }

    pub fn config(&self) -> AsrConfig {
        AsrConfig {
            model_path: self.model_path.clone(),
            sample_rate: self.sample_rate,
            language: "zh".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AsrConfig {
    pub model_path: String,
    pub sample_rate: u32,
    pub language: String,
}

pub type SharedAsrEngine = Arc<AsrEngine>;

/// Scripted ASR that cycles through a fixed list of utterances. Lets us
/// rehearse the full state machine (follow-up classifier, timeout path,
/// agent tool call) without running a real speech model.
///
/// Replace with a sherpa-onnx (Paraformer / Zipformer) wrapper that calls
/// the ONNX runtime on `samples`.
pub struct ScriptedAsr {
    utterances: Vec<String>,
    cursor: Mutex<usize>,
}

impl ScriptedAsr {
    pub fn new(utterances: Vec<String>) -> Self {
        Self {
            utterances,
            cursor: Mutex::new(0),
        }
    }
}

#[async_trait]
impl AsrTranscriber for ScriptedAsr {
    async fn transcribe(&self, samples: &[f32]) -> anyhow::Result<String> {
        if self.utterances.is_empty() {
            return Ok(String::new());
        }
        let text = {
            let mut cursor = self.cursor.lock().unwrap();
            let idx = *cursor % self.utterances.len();
            *cursor = (*cursor + 1) % self.utterances.len();
            self.utterances[idx].clone()
        };
        tracing::info!(
            "[asr stub] transcribed {} samples -> {:?}",
            samples.len(),
            text
        );
        Ok(text)
    }
}
