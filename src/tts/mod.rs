mod sea_g2p;

use crate::AppState;
use axum::{
    body::Body,
    extract::{Multipart, Path as AxumPath, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use llama_cpp_2::{
    context::{params::LlamaContextParams, LlamaContext},
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{params::LlamaModelParams, AddBos, LlamaModel, Special},
    sampling::LlamaSampler,
    token::LlamaToken,
};
use once_cell::sync::{Lazy, OnceCell};
use ort::{session::Session as OrtSession, value::Tensor};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Instant,
};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error as SymphoniaError,
    formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use uuid::Uuid;

const SAMPLE_RATE_HZ: u32 = 24_000;
const MAX_INPUT_CHARS: usize = 5_000;
const MAX_SAMPLE_BYTES: usize = 15 * 1024 * 1024;
const DEFAULT_TEMPERATURE: f32 = 0.4;
const DEFAULT_TOP_K: u32 = 50;
const DEFAULT_QUEUE: usize = 32;
const DEFAULT_MAX_CONCURRENT: usize = 1;
const MAX_NEW_TOKENS: usize = 2048;

static VOICE_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$").expect("voice id regex"));
static SPEECH_TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<\|speech_(\d+)\|>").expect("speech token regex"));

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum VoiceSource {
    Base,
    Custom,
}

#[derive(Clone, Debug)]
struct VoiceRecord {
    id: String,
    description: String,
    source: VoiceSource,
    embedding: Vec<f32>,
    transcript: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct VoiceListItem {
    id: String,
    description: String,
    is_default: bool,
    source: VoiceSource,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SpeechRequest {
    input: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    response_format: Option<String>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_k: Option<u32>,
}

#[derive(Clone, Debug)]
struct ValidSpeechRequest {
    input: String,
    voice: Option<String>,
    temperature: f32,
    top_k: u32,
}

#[derive(Clone, Debug)]
pub struct TtsRuntime {
    state: Arc<RuntimeState>,
    tx: mpsc::Sender<SynthesisJob>,
}

#[derive(Debug)]
struct RuntimeState {
    config: RuntimeConfig,
    assets: AssetPaths,
    voices: RwLock<VoiceStore>,
    engine: Mutex<NativeSynthEngine>,
    ready: AtomicBool,
}

#[derive(Debug)]
struct SynthesisJob {
    request: ValidSpeechRequest,
    reply: oneshot::Sender<Result<Vec<i16>, ApiError>>,
}

#[derive(Debug, Clone)]
struct RuntimeConfig {
    max_concurrent: usize,
    queue_capacity: usize,
    threads: usize,
    ort_intra_threads: usize,
}

impl RuntimeConfig {
    fn from_env() -> Self {
        let physical_cores = num_cpus::get_physical().max(1);
        let threads = std::env::var("TTS_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(physical_cores);
        let ort_intra_threads = std::env::var("TTS_ORT_INTRA_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(physical_cores);
        let max_concurrent = std::env::var("TTS_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_CONCURRENT)
            .max(1);
        let queue_capacity = std::env::var("TTS_QUEUE_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_QUEUE)
            .max(1);

        Self {
            max_concurrent,
            queue_capacity,
            threads,
            ort_intra_threads,
        }
    }
}

#[derive(Debug, Clone)]
struct AssetPaths {
    base_dir: PathBuf,
    gguf_model: PathBuf,
    decoder_onnx: PathBuf,
    encoder_onnx: PathBuf,
    sea_g2p_bin: PathBuf,
    base_voices: PathBuf,
    custom_voices: PathBuf,
}

impl AssetPaths {
    fn discover() -> Self {
        let explicit = std::env::var("PRINT_UTIL_TTS_DIR").ok().map(PathBuf::from);
        let local_appdata_tts = std::env::var("LOCALAPPDATA")
            .ok()
            .map(|d| PathBuf::from(d).join("print-util").join("tts"));
        let public_docs_tts = std::env::var("PUBLIC")
            .ok()
            .map(|d| PathBuf::from(d).join("Documents").join("print-util").join("tts"));
        let program_data_tts = std::env::var("ProgramData")
            .ok()
            .map(|d| PathBuf::from(d).join("print-util").join("tts"));
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));

        let mut candidates = Vec::new();
        if let Some(path) = explicit {
            candidates.push(path);
        }
        if let Some(path) = local_appdata_tts {
            candidates.push(path);
        }
        if let Some(path) = public_docs_tts {
            candidates.push(path);
        }
        if let Some(path) = program_data_tts {
            candidates.push(path);
        }
        candidates.push(exe_dir.join("tts"));
        candidates.push(PathBuf::from("tts"));

        let base_dir = candidates
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| exe_dir.join("tts"));

        let base_voices_json = if base_dir.join("voices.base.json").exists() {
            base_dir.join("voices.base.json")
        } else {
            base_dir.join("voices.json")
        };

        Self {
            gguf_model: base_dir.join("vieneu-tts-v2-turbo.gguf"),
            decoder_onnx: base_dir.join("vieneu_decoder_int8.onnx"),
            encoder_onnx: base_dir.join("vieneu_encoder.onnx"),
            sea_g2p_bin: base_dir.join("sea_g2p.bin"),
            base_voices: base_voices_json,
            custom_voices: base_dir.join("voices.custom.json"),
            base_dir,
        }
    }

    fn missing_core_assets(&self) -> Vec<String> {
        let mut missing = Vec::new();
        if !self.gguf_model.exists() {
            missing.push(self.gguf_model.display().to_string());
        }
        if !self.decoder_onnx.exists() {
            missing.push(self.decoder_onnx.display().to_string());
        }
        if !self.encoder_onnx.exists() {
            missing.push(self.encoder_onnx.display().to_string());
        }
        if !self.base_voices.exists() {
            missing.push(self.base_voices.display().to_string());
        }
        missing
    }
}

#[derive(Debug)]
struct VoiceStore {
    voices: BTreeMap<String, VoiceRecord>,
    default_voice: String,
    custom_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct CustomVoiceFile {
    #[serde(default)]
    default_voice: Option<String>,
    #[serde(default)]
    voices: Vec<CustomVoiceEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CustomVoiceEntry {
    id: String,
    description: String,
    transcript: String,
    embedding: Vec<f32>,
}

#[derive(Debug)]
struct NewVoiceInput {
    id: String,
    description: String,
    transcript: String,
    embedding: Vec<f32>,
    set_default: bool,
}

struct NativeSynthEngine {
    is_mock: bool,
    g2p: Option<sea_g2p::SeaG2pCore>,
    llama_model: Option<&'static LlamaModel>,
    llama_context: Option<LlamaContext<'static>>,
    decoder_session: Option<OrtSession>,
    encoder_session: Option<OrtSession>,
}

// Safety:
// - Engine access is serialized by `Mutex<NativeSynthEngine>` in `RuntimeState`.
// - TTS queue is effectively single-worker (`TTS_MAX_CONCURRENT=1` by default).
// - We never share mutable references to llama context/model across threads concurrently.
unsafe impl Send for NativeSynthEngine {}
unsafe impl Sync for NativeSynthEngine {}

impl std::fmt::Debug for NativeSynthEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSynthEngine")
            .field("is_mock", &self.is_mock)
            .field("has_g2p", &self.g2p.is_some())
            .field("has_llama_model", &self.llama_model.is_some())
            .field("has_llama_context", &self.llama_context.is_some())
            .field("has_decoder_session", &self.decoder_session.is_some())
            .field("has_encoder_session", &self.encoder_session.is_some())
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    ServiceUnavailable(String),
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Conflict(_) => "conflict",
            Self::NotFound(_) => "not_found",
            Self::ServiceUnavailable(_) => "service_unavailable",
            Self::Internal(_) => "internal_error",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response<Body> {
        let status = self.status();
        let body = Json(json!({
            "error": self.to_string(),
            "error_code": self.code(),
        }));
        (status, body).into_response()
    }
}

impl VoiceStore {
    fn load(assets: &AssetPaths) -> Result<Self, ApiError> {
        if let Some(parent) = assets.custom_voices.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ApiError::Internal(format!(
                    "failed to create tts directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let mut voices = load_base_voices(&assets.base_voices)?;
        if voices.is_empty() {
            voices.insert(
                "default_voice".to_string(),
                VoiceRecord {
                    id: "default_voice".to_string(),
                    description: "Fallback default voice".to_string(),
                    source: VoiceSource::Base,
                    embedding: vec![0.0; 128],
                    transcript: Some("xin chao".to_string()),
                },
            );
        }

        let mut default_voice = voices
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "default_voice".to_string());

        let custom = load_custom_voices_file(&assets.custom_voices)?;
        for custom_voice in custom.voices {
            if voices.contains_key(&custom_voice.id) {
                continue;
            }
            voices.insert(
                custom_voice.id.clone(),
                VoiceRecord {
                    id: custom_voice.id,
                    description: custom_voice.description,
                    source: VoiceSource::Custom,
                    embedding: fit_embedding_128(&custom_voice.embedding),
                    transcript: Some(custom_voice.transcript),
                },
            );
        }

        if let Some(default_from_file) = custom.default_voice {
            if voices.contains_key(&default_from_file) {
                default_voice = default_from_file;
            }
        }

        Ok(Self {
            voices,
            default_voice,
            custom_path: assets.custom_voices.clone(),
        })
    }

    fn list(&self) -> Vec<VoiceListItem> {
        self.voices
            .values()
            .map(|v| VoiceListItem {
                id: v.id.clone(),
                description: v.description.clone(),
                is_default: v.id == self.default_voice,
                source: v.source,
            })
            .collect()
    }

    fn resolve_voice(&self, requested: Option<&str>) -> Result<VoiceRecord, ApiError> {
        let id = requested.unwrap_or(&self.default_voice);
        self.voices
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::BadRequest(format!("voice '{id}' not found")))
    }

    fn add_custom_voice(&mut self, input: NewVoiceInput) -> Result<(), ApiError> {
        if self.voices.contains_key(&input.id) {
            return Err(ApiError::Conflict(format!(
                "voice '{}' already exists",
                input.id
            )));
        }

        let record = VoiceRecord {
            id: input.id.clone(),
            description: input.description,
            source: VoiceSource::Custom,
            embedding: fit_embedding_128(&input.embedding),
            transcript: Some(input.transcript),
        };
        self.voices.insert(input.id.clone(), record);

        if input.set_default {
            self.default_voice = input.id;
        }
        self.persist_custom()
    }

    fn delete_custom_voice(&mut self, voice_id: &str) -> Result<(), ApiError> {
        let Some(existing) = self.voices.get(voice_id) else {
            return Err(ApiError::NotFound(format!(
                "custom voice '{voice_id}' not found"
            )));
        };
        if existing.source != VoiceSource::Custom {
            return Err(ApiError::NotFound(format!(
                "custom voice '{voice_id}' not found"
            )));
        }

        self.voices.remove(voice_id);
        if self.default_voice == voice_id {
            self.default_voice = self
                .voices
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "default_voice".to_string());
        }
        self.persist_custom()
    }

    fn persist_custom(&self) -> Result<(), ApiError> {
        let mut custom_entries = Vec::new();
        for voice in self.voices.values() {
            if voice.source != VoiceSource::Custom {
                continue;
            }
            custom_entries.push(CustomVoiceEntry {
                id: voice.id.clone(),
                description: voice.description.clone(),
                transcript: voice.transcript.clone().unwrap_or_default(),
                embedding: voice.embedding.clone(),
            });
        }
        let file = CustomVoiceFile {
            default_voice: Some(self.default_voice.clone()),
            voices: custom_entries,
        };
        write_json_atomic(&self.custom_path, &file)
    }
}

impl NativeSynthEngine {
    fn load(assets: &AssetPaths, config: &RuntimeConfig) -> Result<Self, ApiError> {
        configure_ort_dylib_path(assets);

        let g2p = if assets.sea_g2p_bin.exists() {
            match sea_g2p::SeaG2pCore::open(&assets.sea_g2p_bin) {
                Ok(core) => Some(core),
                Err(e) => {
                    warn!(
                        component = "tts",
                        op = "g2p_init",
                        status = "warn",
                        error_code = "g2p_load_failed",
                        "failed to load sea_g2p.bin: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };

        let backend = llama_backend()?;
        let model_params = LlamaModelParams::default()
            .with_n_gpu_layers(0)
            .with_use_mmap(true)
            .with_use_mlock(false);
        let model_box = Box::new(
            LlamaModel::load_from_file(backend, &assets.gguf_model, &model_params)
                .map_err(|e| ApiError::Internal(format!("failed to load gguf model: {e}")))?,
        );
        let llama_model: &'static LlamaModel = Box::leak(model_box);

        let context_params = LlamaContextParams::default()
            .with_n_ctx(std::num::NonZeroU32::new(4096))
            .with_n_threads(config.threads as i32)
            .with_n_threads_batch(config.threads as i32)
            .with_n_batch(2048)
            .with_n_ubatch(2048);
        let llama_context = llama_model
            .new_context(backend, context_params)
            .map_err(|e| ApiError::Internal(format!("failed to create llama context: {e}")))?;

        let decoder_session = build_ort_session(&assets.decoder_onnx, config.ort_intra_threads)
            .map_err(|e| ApiError::Internal(format!("failed to load decoder onnx: {e}")))?;
        let encoder_session = build_ort_session(&assets.encoder_onnx, config.ort_intra_threads)
            .map_err(|e| ApiError::Internal(format!("failed to load encoder onnx: {e}")))?;

        Ok(Self {
            is_mock: false,
            g2p,
            llama_model: Some(llama_model),
            llama_context: Some(llama_context),
            decoder_session: Some(decoder_session),
            encoder_session: Some(encoder_session),
        })
    }

    #[cfg(test)]
    fn mock() -> Self {
        Self {
            is_mock: true,
            g2p: None,
            llama_model: None,
            llama_context: None,
            decoder_session: None,
            encoder_session: None,
        }
    }

    fn warmup(&mut self, voice: &VoiceRecord) -> Result<(), ApiError> {
        let _ = self.synthesize("xin chao", voice, DEFAULT_TEMPERATURE, DEFAULT_TOP_K)?;
        Ok(())
    }

    fn synthesize(
        &mut self,
        input: &str,
        voice: &VoiceRecord,
        temperature: f32,
        top_k: u32,
    ) -> Result<Vec<i16>, ApiError> {
        if self.is_mock {
            let generated =
                generate_mock_speech_tokens(input, &voice.embedding, temperature, top_k);
            let tokens = extract_speech_ids(&generated);
            let mut waveform = decode_mock_tokens(&tokens, &voice.embedding, temperature);
            if waveform.is_empty() {
                waveform.push(0.0);
            }
            return Ok(f32_to_pcm16(&waveform));
        }

        let normalized = self.normalize_and_phonemize(input);
        let prompt = format_turbo_prompt(&normalized);
        let generated = self.generate_speech_tokens(&prompt, temperature, top_k)?;
        let tokens = extract_speech_ids(&generated);
        if tokens.is_empty() {
            return Err(ApiError::Internal(
                "model generated no speech tokens".to_string(),
            ));
        }

        let mut waveform = self.decode_tokens_to_waveform(&tokens, &voice.embedding)?;
        if waveform.is_empty() {
            waveform.push(0.0);
        }
        Ok(f32_to_pcm16(&waveform))
    }

    fn encode_embedding(&mut self, samples_24k_mono: &[f32]) -> Result<Vec<f32>, ApiError> {
        if samples_24k_mono.is_empty() {
            return Err(ApiError::BadRequest("sample audio is empty".to_string()));
        }
        if self.is_mock {
            return Ok(compute_embedding_fallback(samples_24k_mono));
        }

        let encoder = self.encoder_session.as_mut().ok_or_else(|| {
            ApiError::ServiceUnavailable("encoder is not initialized".to_string())
        })?;

        let waveform = Tensor::from_array((
            [1_usize, samples_24k_mono.len()],
            samples_24k_mono.to_vec().into_boxed_slice(),
        ))
        .map_err(|e| ApiError::Internal(format!("failed to create encoder input tensor: {e}")))?;

        let outputs = encoder
            .run(ort::inputs! { "waveform" => waveform })
            .map_err(|e| ApiError::Internal(format!("encoder inference failed: {e}")))?;

        let output = outputs
            .values()
            .next()
            .ok_or_else(|| ApiError::Internal("encoder returned no outputs".to_string()))?;
        let (_shape, data) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| ApiError::Internal(format!("encoder output parse failed: {e}")))?;

        let embedding = fit_embedding_128(data);
        if embedding.iter().all(|x| *x == 0.0) {
            return Err(ApiError::Internal(
                "encoder generated invalid all-zero embedding".to_string(),
            ));
        }
        Ok(embedding)
    }

    fn generate_speech_tokens(
        &mut self,
        prompt: &str,
        temperature: f32,
        top_k: u32,
    ) -> Result<String, ApiError> {
        if self.llama_model.is_none() {
            return Err(ApiError::ServiceUnavailable(
                "llama model is not initialized".to_string(),
            ));
        }
        let model = self.llama_model.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable("llama model is not initialized".to_string())
        })?;
        let context = self.llama_context.as_mut().ok_or_else(|| {
            ApiError::ServiceUnavailable("llama context is not initialized".to_string())
        })?;
        context.clear_kv_cache();

        let prompt_tokens = model
            .str_to_token(prompt, AddBos::Never)
            .map_err(|e| ApiError::Internal(format!("failed to tokenize prompt: {e}")))?;
        if prompt_tokens.is_empty() {
            return Err(ApiError::Internal("prompt produced no tokens".to_string()));
        }

        let mut batch = LlamaBatch::new(1, 1);
        for (pos, token) in prompt_tokens.iter().enumerate() {
            let pos = i32::try_from(pos)
                .map_err(|_| ApiError::Internal("prompt token position overflow".to_string()))?;
            batch.clear();
            batch.add(*token, pos, &[0], true).map_err(|e| {
                ApiError::Internal(format!("failed to add prompt token to llama batch: {e}"))
            })?;
            context
                .decode(&mut batch)
                .map_err(|e| ApiError::Internal(format!("llama decode failed: {e}")))?;
        }

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::penalties(64, 1.15, 0.0, 0.0),
            LlamaSampler::top_k(top_k as i32),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(temperature),
            LlamaSampler::dist(0),
        ]);
        sampler.accept_many(prompt_tokens.iter());

        let mut pos = i32::try_from(prompt_tokens.len())
            .map_err(|_| ApiError::Internal("prompt token length overflow".to_string()))?;
        let mut generated = String::new();
        for _ in 0..MAX_NEW_TOKENS {
            let token = sampler.sample(context, -1);
            if model.is_eog_token(token) || token == model.token_eos() {
                break;
            }
            sampler.accept(token);

            let piece = model
                .token_to_bytes(token, Special::Plaintext)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            generated.push_str(&piece);
            if generated.contains("<|SPEECH_GENERATION_END|>") {
                break;
            }
            if generated.len() > 128 * 1024 {
                break;
            }

            batch.clear();
            batch.add(LlamaToken(token.0), pos, &[0], true).map_err(|e| {
                ApiError::Internal(format!("failed to add generated token to batch: {e}"))
            })?;
            context
                .decode(&mut batch)
                .map_err(|e| ApiError::Internal(format!("llama decode failed: {e}")))?;
            pos = pos.saturating_add(1);
        }

        Ok(generated)
    }

    fn decode_tokens_to_waveform(
        &mut self,
        tokens: &[u32],
        voice_embedding: &[f32],
    ) -> Result<Vec<f32>, ApiError> {
        let decoder = self.decoder_session.as_mut().ok_or_else(|| {
            ApiError::ServiceUnavailable("decoder is not initialized".to_string())
        })?;

        let token_ids: Vec<i64> = tokens.iter().map(|x| *x as i64).collect();
        let content_ids =
            Tensor::from_array(([1_usize, token_ids.len()], token_ids.into_boxed_slice()))
                .map_err(|e| {
                    ApiError::Internal(format!("failed to create content_ids tensor: {e}"))
                })?;
        let voice_embedding = fit_embedding_128(voice_embedding);
        let voice_tensor =
            Tensor::from_array(([1_usize, 128_usize], voice_embedding.into_boxed_slice()))
                .map_err(|e| {
                    ApiError::Internal(format!("failed to create voice_embedding tensor: {e}"))
                })?;

        let outputs = decoder
            .run(ort::inputs! {
                "content_ids" => content_ids,
                "voice_embedding" => voice_tensor
            })
            .map_err(|e| ApiError::Internal(format!("decoder inference failed: {e}")))?;

        let output = outputs
            .values()
            .next()
            .ok_or_else(|| ApiError::Internal("decoder returned no outputs".to_string()))?;
        let (_shape, data) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| ApiError::Internal(format!("decoder output parse failed: {e}")))?;
        Ok(data.to_vec())
    }

    fn normalize_and_phonemize(&self, input: &str) -> String {
        let compact = input
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_lowercase();
        if let Some(g2p) = &self.g2p {
            return g2p.phonemize(&compact);
        }
        compact
    }
}

impl TtsRuntime {
    pub async fn bootstrap() -> Self {
        let config = RuntimeConfig::from_env();
        let assets = AssetPaths::discover();
        let missing_assets = assets.missing_core_assets();
        if !missing_assets.is_empty() {
            warn!(
                component = "tts",
                op = "bootstrap",
                status = "degraded",
                error_code = "assets_missing",
                missing = ?missing_assets,
                "tts core assets are missing; /v1/audio/speech will return 503"
            );
        }

        let voices = VoiceStore::load(&assets).unwrap_or_else(|e| {
            warn!(
                component = "tts",
                op = "voice_store_load",
                status = "degraded",
                error_code = e.code(),
                "failed to load voices: {e}"
            );
            VoiceStore {
                voices: BTreeMap::new(),
                default_voice: "default_voice".to_string(),
                custom_path: assets.custom_voices.clone(),
            }
        });

        let (engine, ready) = if missing_assets.is_empty() {
            match NativeSynthEngine::load(&assets, &config) {
                Ok(engine) => (engine, true),
                Err(e) => {
                    warn!(
                        component = "tts",
                        op = "engine_init",
                        status = "failed",
                        error_code = e.code(),
                        "tts engine init failed: {e}"
                    );
                    (
                        NativeSynthEngine {
                            is_mock: false,
                            g2p: None,
                            llama_model: None,
                            llama_context: None,
                            decoder_session: None,
                            encoder_session: None,
                        },
                        false,
                    )
                }
            }
        } else {
            (
                NativeSynthEngine {
                    is_mock: false,
                    g2p: None,
                    llama_model: None,
                    llama_context: None,
                    decoder_session: None,
                    encoder_session: None,
                },
                false,
            )
        };

        let state = Arc::new(RuntimeState {
            config: config.clone(),
            assets,
            voices: RwLock::new(voices),
            engine: Mutex::new(engine),
            ready: AtomicBool::new(ready),
        });

        if ready {
            let warmup_voice = {
                let voices = state.voices.read().unwrap();
                voices.resolve_voice(None).ok()
            };
            if let Some(voice) = warmup_voice {
                let mut engine_guard = state.engine.lock().unwrap();
                if let Err(e) = engine_guard.warmup(&voice) {
                    warn!(
                        component = "tts",
                        op = "warmup",
                        status = "failed",
                        error_code = e.code(),
                        "tts warmup failed: {e}"
                    );
                    state.ready.store(false, Ordering::SeqCst);
                } else {
                    info!(
                        component = "tts",
                        op = "warmup",
                        status = "ok",
                        "tts warmup completed"
                    );
                }
            }
        }

        let (tx, mut rx) = mpsc::channel::<SynthesisJob>(state.config.queue_capacity);
        let worker_state = state.clone();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let state_ref = worker_state.clone();
                let req = job.request;
                let result = tokio::task::spawn_blocking(move || state_ref.synthesize_sync(req))
                    .await
                    .map_err(|e| ApiError::Internal(format!("tts worker panic: {e}")))
                    .and_then(|x| x);
                let _ = job.reply.send(result);
            }
        });

        info!(
            component = "tts",
            op = "bootstrap",
            status = "ok",
            queue_capacity = state.config.queue_capacity,
            max_concurrent = state.config.max_concurrent,
            threads = state.config.threads,
            ort_intra_threads = state.config.ort_intra_threads,
            tts_dir = %state.assets.base_dir.display(),
            "tts runtime initialized"
        );

        Self { state, tx }
    }

    #[cfg(test)]
    pub async fn bootstrap_for_tests() -> Self {
        let temp_root =
            std::env::temp_dir().join(format!("print-util-tts-test-{}", Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&temp_root);
        let assets = AssetPaths {
            gguf_model: temp_root.join("vieneu-tts-v2-turbo.gguf"),
            decoder_onnx: temp_root.join("vieneu_decoder_int8.onnx"),
            encoder_onnx: temp_root.join("vieneu_encoder.onnx"),
            sea_g2p_bin: temp_root.join("sea_g2p.bin"),
            base_voices: temp_root.join("voices.base.json"),
            custom_voices: temp_root.join("voices.custom.json"),
            base_dir: temp_root.clone(),
        };

        let base = json!({
            "default_voice": "default_voice",
            "presets": {
                "default_voice": {
                    "description": "Default test voice",
                    "text": "xin chao",
                    "codes": vec![0.01_f32; 128],
                }
            }
        });
        let _ = write_json_atomic(&assets.base_voices, &base);
        let _ = std::fs::write(&assets.gguf_model, b"mock");
        let _ = std::fs::write(&assets.decoder_onnx, b"mock");
        let _ = std::fs::write(&assets.encoder_onnx, b"mock");

        let state = Arc::new(RuntimeState {
            config: RuntimeConfig::from_env(),
            assets: assets.clone(),
            voices: RwLock::new(VoiceStore::load(&assets).expect("test voice store")),
            engine: Mutex::new(NativeSynthEngine::mock()),
            ready: AtomicBool::new(true),
        });
        let (tx, mut rx) = mpsc::channel::<SynthesisJob>(8);
        let worker_state = state.clone();
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let state_ref = worker_state.clone();
                let req = job.request;
                let result = tokio::task::spawn_blocking(move || state_ref.synthesize_sync(req))
                    .await
                    .map_err(|e| ApiError::Internal(format!("tts worker panic: {e}")))
                    .and_then(|x| x);
                let _ = job.reply.send(result);
            }
        });
        Self { state, tx }
    }

    async fn synthesize(&self, request: ValidSpeechRequest) -> Result<Vec<i16>, ApiError> {
        if !self.state.ready.load(Ordering::SeqCst) {
            return Err(ApiError::ServiceUnavailable(
                "tts engine is not ready".to_string(),
            ));
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .try_send(SynthesisJob {
                request,
                reply: reply_tx,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => {
                    ApiError::ServiceUnavailable("tts queue is full; retry later".to_string())
                }
                mpsc::error::TrySendError::Closed(_) => {
                    ApiError::ServiceUnavailable("tts queue is unavailable".to_string())
                }
            })?;

        reply_rx
            .await
            .map_err(|_| ApiError::Internal("tts worker dropped response".to_string()))?
    }

    fn list_voices(&self) -> Vec<VoiceListItem> {
        let voices = self.state.voices.read().unwrap();
        voices.list()
    }

    fn create_voice(&self, input: NewVoiceInput) -> Result<(), ApiError> {
        let mut voices = self.state.voices.write().unwrap();
        voices.add_custom_voice(input)
    }

    fn encode_voice_embedding(&self, samples_24k_mono: &[f32]) -> Result<Vec<f32>, ApiError> {
        let mut engine = self.state.engine.lock().unwrap();
        engine.encode_embedding(samples_24k_mono)
    }

    fn delete_voice(&self, voice_id: &str) -> Result<(), ApiError> {
        let mut voices = self.state.voices.write().unwrap();
        voices.delete_custom_voice(voice_id)
    }
}

impl RuntimeState {
    fn synthesize_sync(&self, request: ValidSpeechRequest) -> Result<Vec<i16>, ApiError> {
        let voice = {
            let voices = self.voices.read().unwrap();
            voices.resolve_voice(request.voice.as_deref())?
        };

        let mut engine = self.engine.lock().unwrap();
        engine.synthesize(&request.input, &voice, request.temperature, request.top_k)
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/audio/speech", post(post_speech))
        .route("/v1/voices", get(get_voices).post(post_voice))
        .route("/v1/voices/:voice_id", delete(delete_voice))
}

async fn post_speech(
    State(state): State<AppState>,
    Json(payload): Json<SpeechRequest>,
) -> Result<Response<Body>, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let request = validate_speech_request(payload)?;
    let voice_id_for_log = request
        .voice
        .clone()
        .unwrap_or_else(|| "default_voice".to_string());
    let input_len = request.input.chars().count();

    let synth = state.tts.synthesize(request).await;
    match synth {
        Ok(pcm) => {
            let wav = pcm16_to_wav_bytes(&pcm, SAMPLE_RATE_HZ)?;
            info!(
                component = "tts",
                request_id = %request_id,
                op = "speech",
                voice_id = %voice_id_for_log,
                input_len = input_len,
                latency_ms = started.elapsed().as_millis() as u64,
                status = "ok",
                error_code = "",
                "speech synthesized"
            );

            let mut response = Response::new(Body::from(wav));
            *response.status_mut() = StatusCode::OK;
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
            Ok(response)
        }
        Err(e) => {
            warn!(
                component = "tts",
                request_id = %request_id,
                op = "speech",
                voice_id = %voice_id_for_log,
                input_len = input_len,
                latency_ms = started.elapsed().as_millis() as u64,
                status = "failed",
                error_code = e.code(),
                "speech request failed"
            );
            Err(e)
        }
    }
}

async fn get_voices(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    let data = state.tts.list_voices();
    info!(
        component = "tts",
        request_id = %request_id,
        op = "voices.list",
        voice_id = "",
        input_len = 0_u32,
        latency_ms = started.elapsed().as_millis() as u64,
        status = "ok",
        error_code = "",
        "listed voices"
    );
    Ok((StatusCode::OK, Json(json!({ "data": data }))))
}

async fn post_voice(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();

    let mut name: Option<String> = None;
    let mut transcript: Option<String> = None;
    let mut sample: Option<Vec<u8>> = None;
    let mut sample_name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut set_default = false;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("invalid multipart payload: {e}")))?
    {
        let key = field.name().unwrap_or_default().to_string();
        match key.as_str() {
            "name" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("failed to read name: {e}")))?;
                name = Some(text.trim().to_string());
            }
            "transcript" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("failed to read transcript: {e}")))?;
                transcript = Some(text.trim().to_string());
            }
            "description" => {
                let text = field.text().await.map_err(|e| {
                    ApiError::BadRequest(format!("failed to read description: {e}"))
                })?;
                description = Some(text.trim().to_string());
            }
            "set_default" => {
                let text = field.text().await.map_err(|e| {
                    ApiError::BadRequest(format!("failed to read set_default: {e}"))
                })?;
                set_default = matches!(text.trim().to_lowercase().as_str(), "1" | "true" | "yes");
            }
            "sample" => {
                sample_name = field.file_name().map(ToString::to_string);
                let bytes = field.bytes().await.map_err(|e| {
                    ApiError::BadRequest(format!("failed to read sample audio: {e}"))
                })?;
                if bytes.len() > MAX_SAMPLE_BYTES {
                    return Err(ApiError::BadRequest(format!(
                        "sample is too large (max {} MB)",
                        MAX_SAMPLE_BYTES / (1024 * 1024)
                    )));
                }
                sample = Some(bytes.to_vec());
            }
            _ => {}
        }
    }

    let voice_id =
        name.ok_or_else(|| ApiError::BadRequest("missing required field 'name'".to_string()))?;
    validate_voice_id(&voice_id)?;
    let transcript = transcript
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ApiError::BadRequest("missing required field 'transcript'".to_string()))?;
    let sample = sample
        .ok_or_else(|| ApiError::BadRequest("missing required field 'sample'".to_string()))?;

    let (decoded, sample_rate, channels) = decode_audio_any(&sample, sample_name.as_deref())?;
    let mono = downmix_to_mono(&decoded, channels);
    let mono_24k = resample_linear(&mono, sample_rate, SAMPLE_RATE_HZ);
    let embedding = state.tts.encode_voice_embedding(&mono_24k)?;
    let description = description.unwrap_or_else(|| format!("Custom voice {}", voice_id));

    state.tts.create_voice(NewVoiceInput {
        id: voice_id.clone(),
        description,
        transcript,
        embedding,
        set_default,
    })?;

    info!(
        component = "tts",
        request_id = %request_id,
        op = "voices.create",
        voice_id = %voice_id,
        input_len = mono_24k.len(),
        latency_ms = started.elapsed().as_millis() as u64,
        status = "ok",
        error_code = "",
        "created custom voice"
    );

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": voice_id,
            "status": "created"
        })),
    ))
}

async fn delete_voice(
    State(state): State<AppState>,
    AxumPath(voice_id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let started = Instant::now();
    validate_voice_id(&voice_id)?;
    state.tts.delete_voice(&voice_id)?;
    info!(
        component = "tts",
        request_id = %request_id,
        op = "voices.delete",
        voice_id = %voice_id,
        input_len = 0_u32,
        latency_ms = started.elapsed().as_millis() as u64,
        status = "ok",
        error_code = "",
        "deleted custom voice"
    );
    Ok((StatusCode::OK, Json(json!({ "status": "ok" }))))
}

fn validate_speech_request(payload: SpeechRequest) -> Result<ValidSpeechRequest, ApiError> {
    let input_len = payload.input.chars().count();
    if input_len == 0 || input_len > MAX_INPUT_CHARS {
        return Err(ApiError::BadRequest(format!(
            "input must be between 1 and {} characters",
            MAX_INPUT_CHARS
        )));
    }

    if let Some(voice) = payload.voice.as_deref() {
        validate_voice_id(voice)?;
    }

    if let Some(fmt) = payload.response_format.as_deref() {
        if !fmt.eq_ignore_ascii_case("wav") {
            return Err(ApiError::BadRequest(
                "response_format only supports 'wav'".to_string(),
            ));
        }
    }

    let temperature = payload
        .temperature
        .unwrap_or(DEFAULT_TEMPERATURE)
        .clamp(0.0, 2.0);
    let top_k = payload.top_k.unwrap_or(DEFAULT_TOP_K).max(1);

    Ok(ValidSpeechRequest {
        input: payload.input,
        voice: payload.voice,
        temperature,
        top_k,
    })
}

fn validate_voice_id(voice_id: &str) -> Result<(), ApiError> {
    if !VOICE_ID_RE.is_match(voice_id) {
        return Err(ApiError::BadRequest(format!(
            "invalid voice id '{}'",
            voice_id
        )));
    }
    Ok(())
}

fn load_base_voices(path: &Path) -> Result<BTreeMap<String, VoiceRecord>, ApiError> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ApiError::Internal(format!("failed to read base voices file: {e}")))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|e| ApiError::Internal(format!("invalid base voices json: {e}")))?;

    let mut voices = BTreeMap::new();
    let presets = value
        .get("presets")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for (voice_id, entry) in presets {
        let codes = entry
            .get("codes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let embedding = fit_embedding_128(
            &codes
                .into_iter()
                .filter_map(|v| v.as_f64().map(|x| x as f32))
                .collect::<Vec<f32>>(),
        );

        let description = entry
            .get("description")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| voice_id.clone());
        let transcript = entry
            .get("text")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        voices.insert(
            voice_id.clone(),
            VoiceRecord {
                id: voice_id,
                description,
                source: VoiceSource::Base,
                embedding,
                transcript,
            },
        );
    }

    let default_voice = value
        .get("default_voice")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    if let Some(default_voice) = default_voice {
        if let Some(default_record) = voices.get(&default_voice).cloned() {
            voices.insert(default_voice, default_record);
        }
    }

    Ok(voices)
}

fn load_custom_voices_file(path: &Path) -> Result<CustomVoiceFile, ApiError> {
    if !path.exists() {
        return Ok(CustomVoiceFile::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ApiError::Internal(format!("failed to read custom voices file: {e}")))?;
    serde_json::from_str(&raw)
        .map_err(|e| ApiError::Internal(format!("invalid custom voices json: {e}")))
}

fn write_json_atomic<T: Serialize>(path: &Path, payload: &T) -> Result<(), ApiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ApiError::Internal(format!(
                "failed to create voice directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    let mut tmp = path.to_path_buf();
    tmp.set_extension("tmp");
    let content = serde_json::to_vec_pretty(payload)
        .map_err(|e| ApiError::Internal(format!("failed to serialize voice file: {e}")))?;
    std::fs::write(&tmp, content)
        .map_err(|e| ApiError::Internal(format!("failed to write temp voice file: {e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| ApiError::Internal(format!("failed to replace voice file: {e}")))?;
    Ok(())
}

fn fit_embedding_128(values: &[f32]) -> Vec<f32> {
    if values.is_empty() {
        return vec![0.0; 128];
    }
    if values.len() == 128 {
        return values.to_vec();
    }
    let mut out = vec![0.0_f32; 128];
    let stride = values.len() as f32 / 128.0;
    for (idx, slot) in out.iter_mut().enumerate() {
        let start = (idx as f32 * stride).floor() as usize;
        let end = (((idx + 1) as f32 * stride).ceil() as usize).min(values.len());
        if end <= start {
            *slot = values[start.min(values.len() - 1)];
            continue;
        }
        let sum: f32 = values[start..end].iter().sum();
        *slot = sum / (end - start) as f32;
    }
    out
}

fn decode_audio_any(
    bytes: &[u8],
    filename: Option<&str>,
) -> Result<(Vec<f32>, u32, usize), ApiError> {
    let mut hint = Hint::new();
    if let Some(filename) = filename {
        if let Some((_, ext)) = filename.rsplit_once('.') {
            hint.with_extension(ext);
        }
    }
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| ApiError::BadRequest(format!("unsupported audio format: {e}")))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| ApiError::BadRequest("missing default audio track".to_string()))?;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| ApiError::BadRequest("cannot detect sample rate".to_string()))?;
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(1);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| ApiError::BadRequest(format!("unsupported codec: {e}")))?;

    let mut out = Vec::<f32>::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(_)) => break,
            Err(SymphoniaError::ResetRequired) => {
                return Err(ApiError::BadRequest(
                    "audio stream reset is not supported".to_string(),
                ))
            }
            Err(e) => return Err(ApiError::BadRequest(format!("audio read error: {e}"))),
        };
        let decoded = decoder
            .decode(&packet)
            .map_err(|e| ApiError::BadRequest(format!("audio decode error: {e}")))?;
        let mut sample_buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        sample_buf.copy_interleaved_ref(decoded);
        out.extend_from_slice(sample_buf.samples());
    }
    if out.is_empty() {
        return Err(ApiError::BadRequest(
            "audio sample has no frames".to_string(),
        ));
    }
    Ok((out, sample_rate, channels))
}

fn downmix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let mut mono = Vec::with_capacity(samples.len() / channels.max(1));
    for frame in samples.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        mono.push(sum / channels as f32);
    }
    mono
}

fn resample_linear(samples: &[f32], src_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    if src_rate == target_rate {
        return samples.to_vec();
    }
    let ratio = src_rate as f64 / target_rate as f64;
    let target_len = ((samples.len() as f64) / ratio).max(1.0) as usize;
    let mut out = Vec::with_capacity(target_len);
    for i in 0..target_len {
        let src_pos = i as f64 * ratio;
        let left = src_pos.floor() as usize;
        let right = (left + 1).min(samples.len() - 1);
        let frac = (src_pos - left as f64) as f32;
        let v = samples[left] * (1.0 - frac) + samples[right] * frac;
        out.push(v);
    }
    out
}

fn compute_embedding_fallback(samples: &[f32]) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; 128];
    }
    let mut embedding = vec![0.0_f32; 128];
    let frame = (samples.len() / 128).max(1);
    for (idx, value) in embedding.iter_mut().enumerate() {
        let start = idx * frame;
        let end = ((idx + 1) * frame).min(samples.len());
        if start >= end {
            *value = 0.0;
            continue;
        }
        let slice = &samples[start..end];
        let mean_abs = slice.iter().map(|x| x.abs()).sum::<f32>() / slice.len() as f32;
        *value = mean_abs;
    }
    let norm = embedding
        .iter()
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt()
        .max(1e-8);
    embedding.iter_mut().for_each(|x| *x /= norm);
    embedding
}

pub(crate) fn format_turbo_prompt(phonemes: &str) -> String {
    format!(
        "<|speaker_16|><|TEXT_PROMPT_START|>{phonemes}<|TEXT_PROMPT_END|><|SPEECH_GENERATION_START|>"
    )
}

pub(crate) fn extract_speech_ids(generated: &str) -> Vec<u32> {
    SPEECH_TOKEN_RE
        .captures_iter(generated)
        .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse::<u32>().ok()))
        .collect()
}

fn build_ort_session(model_path: &Path, intra_threads: usize) -> Result<OrtSession, ApiError> {
    let mut builder = OrtSession::builder()
        .map_err(|e| ApiError::Internal(format!("onnx builder init failed: {e}")))?;
    builder = builder
        .with_intra_threads(intra_threads)
        .map_err(|e| ApiError::Internal(format!("onnx set intra threads failed: {e}")))?;
    builder = builder
        .with_inter_threads(1)
        .map_err(|e| ApiError::Internal(format!("onnx set inter threads failed: {e}")))?;
    builder = builder
        .with_parallel_execution(false)
        .map_err(|e| ApiError::Internal(format!("onnx set execution mode failed: {e}")))?;
    builder
        .commit_from_file(model_path)
        .map_err(|e| ApiError::Internal(format!("onnx commit failed: {e}")))
}

fn llama_backend() -> Result<&'static LlamaBackend, ApiError> {
    static BACKEND: OnceCell<&'static LlamaBackend> = OnceCell::new();
    BACKEND.get_or_try_init(|| {
        let backend = LlamaBackend::init()
            .map_err(|e| ApiError::Internal(format!("failed to init llama backend: {e}")))?;
        Ok(Box::leak(Box::new(backend)))
    }).copied()
}

fn configure_ort_dylib_path(assets: &AssetPaths) {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));

    let mut candidates = Vec::new();
    candidates.push(assets.base_dir.join("onnxruntime.dll"));
    if let Some(dir) = exe_dir {
        candidates.push(dir.join("onnxruntime.dll"));
    }
    candidates.push(PathBuf::from("onnxruntime.dll"));

    for candidate in candidates {
        if candidate.exists() {
            std::env::set_var("ORT_DYLIB_PATH", &candidate);
            info!(
                component = "tts",
                op = "ort_dylib",
                status = "configured",
                dylib = %candidate.display(),
                "configured ORT_DYLIB_PATH"
            );
            return;
        }
    }

    warn!(
        component = "tts",
        op = "ort_dylib",
        status = "missing",
        error_code = "onnxruntime_dll_not_found",
        "onnxruntime.dll not found in tts dir or executable dir"
    );
}

fn generate_mock_speech_tokens(
    prompt: &str,
    embedding: &[f32],
    temperature: f32,
    top_k: u32,
) -> String {
    let mut seed = [0_u8; 32];
    for (idx, b) in prompt.as_bytes().iter().enumerate() {
        seed[idx % seed.len()] ^= *b;
    }
    for (idx, v) in embedding.iter().take(128).enumerate() {
        let bytes = v.to_le_bytes();
        for (bi, bb) in bytes.iter().enumerate() {
            seed[(idx + bi) % seed.len()] ^= *bb;
        }
    }
    for (idx, b) in temperature.to_le_bytes().iter().enumerate() {
        seed[idx % seed.len()] ^= *b;
    }
    for (idx, b) in top_k.to_le_bytes().iter().enumerate() {
        seed[(idx + 7) % seed.len()] ^= *b;
    }

    let token_count = (prompt.len() / 6).clamp(96, 512);
    let mut out = String::with_capacity(token_count * 16);
    for idx in 0..token_count {
        let b1 = seed[(idx * 2) % seed.len()] as u32;
        let b2 = seed[(idx * 2 + 1) % seed.len()] as u32;
        let token = ((b1 << 8) | b2).wrapping_add((idx as u32 + 1) * top_k.max(1)) % 65535;
        out.push_str("<|speech_");
        out.push_str(&token.to_string());
        out.push_str("|>");
    }
    out
}

fn decode_mock_tokens(tokens: &[u32], embedding: &[f32], temperature: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(tokens.len() * 180);
    let energy = embedding.iter().map(|x| x.abs()).sum::<f32>() / embedding.len().max(1) as f32;
    let amp = (0.06 + energy * 0.12).clamp(0.05, 0.24);
    let mut phase = 0.0_f32;
    let two_pi = std::f32::consts::PI * 2.0;
    for (idx, token) in tokens.iter().enumerate() {
        let freq = 120.0 + (*token % 700) as f32 + temperature * 10.0;
        let samples_per_token = 160_usize;
        for _ in 0..samples_per_token {
            out.push(phase.sin() * amp);
            phase += two_pi * freq / SAMPLE_RATE_HZ as f32;
            if phase > two_pi {
                phase -= two_pi;
            }
        }
        if idx % 48 == 47 {
            out.extend(std::iter::repeat_n(0.0, 320));
        }
    }
    out
}

fn f32_to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&v| {
            let clamped = v.clamp(-1.0, 1.0);
            (clamped * i16::MAX as f32) as i16
        })
        .collect()
}

fn pcm16_to_wav_bytes(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>, ApiError> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = WavWriter::new(&mut cursor, spec)
            .map_err(|e| ApiError::Internal(format!("wav writer init failed: {e}")))?;
        for sample in samples {
            writer
                .write_sample(*sample)
                .map_err(|e| ApiError::Internal(format!("wav write failed: {e}")))?;
        }
        writer
            .finalize()
            .map_err(|e| ApiError::Internal(format!("wav finalize failed: {e}")))?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn tiny_wav_16k() -> Vec<u8> {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).expect("wav init");
            for i in 0..16_000 {
                let phase = (i as f32 / 16_000.0) * std::f32::consts::PI * 2.0 * 220.0;
                let sample = (phase.sin() * i16::MAX as f32 * 0.2) as i16;
                writer.write_sample(sample).expect("wav sample");
            }
            writer.finalize().expect("wav finalize");
        }
        cursor.into_inner()
    }

    fn multipart_with_voice(name: &str) -> (String, Vec<u8>) {
        let boundary = "----printutilttsboundary";
        let sample = tiny_wav_16k();
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n{name}\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(
            "------printutilttsboundary\r\nContent-Disposition: form-data; name=\"transcript\"\r\n\r\nxin chao\r\n"
                .as_bytes(),
        );
        body.extend_from_slice(
            "------printutilttsboundary\r\nContent-Disposition: form-data; name=\"description\"\r\n\r\nTest voice\r\n"
                .as_bytes(),
        );
        body.extend_from_slice(
            "------printutilttsboundary\r\nContent-Disposition: form-data; name=\"sample\"; filename=\"sample.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
                .as_bytes(),
        );
        body.extend_from_slice(&sample);
        body.extend_from_slice("\r\n".as_bytes());
        body.extend_from_slice("------printutilttsboundary--\r\n".as_bytes());
        (boundary.to_string(), body)
    }

    #[test]
    fn prompt_and_token_parser() {
        let prompt = format_turbo_prompt("x i n c h a o");
        assert!(prompt.contains("<|SPEECH_GENERATION_START|>"));
        let parsed = extract_speech_ids("<|speech_1|><|speech_22|><|speech_333|>");
        assert_eq!(parsed, vec![1, 22, 333]);
    }

    #[test]
    fn speech_request_validation() {
        let req = SpeechRequest {
            input: "xin chao".to_string(),
            voice: Some("voice_1".to_string()),
            response_format: Some("wav".to_string()),
            temperature: None,
            top_k: None,
        };
        let validated = validate_speech_request(req).expect("valid req");
        assert_eq!(validated.top_k, DEFAULT_TOP_K);
        assert!((validated.temperature - DEFAULT_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[test]
    fn voice_store_merge_and_delete_default() {
        let temp_root =
            std::env::temp_dir().join(format!("print-util-voice-store-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_root).expect("mkdir");
        let assets = AssetPaths {
            gguf_model: temp_root.join("m.gguf"),
            decoder_onnx: temp_root.join("d.onnx"),
            encoder_onnx: temp_root.join("e.onnx"),
            sea_g2p_bin: temp_root.join("sea_g2p.bin"),
            base_voices: temp_root.join("voices.base.json"),
            custom_voices: temp_root.join("voices.custom.json"),
            base_dir: temp_root.clone(),
        };
        let base = json!({
            "default_voice": "base_a",
            "presets": {
                "base_a": {"description": "Base A", "text": "a", "codes": vec![0.1_f32; 128]},
                "base_b": {"description": "Base B", "text": "b", "codes": vec![0.2_f32; 128]}
            }
        });
        write_json_atomic(&assets.base_voices, &base).expect("write base");
        let custom = CustomVoiceFile {
            default_voice: Some("custom_a".to_string()),
            voices: vec![CustomVoiceEntry {
                id: "custom_a".to_string(),
                description: "Custom A".to_string(),
                transcript: "xin chao".to_string(),
                embedding: vec![0.4; 128],
            }],
        };
        write_json_atomic(&assets.custom_voices, &custom).expect("write custom");

        let mut store = VoiceStore::load(&assets).expect("load store");
        assert_eq!(store.default_voice, "custom_a");
        assert_eq!(store.list().len(), 3);
        store
            .delete_custom_voice("custom_a")
            .expect("delete custom");
        assert_ne!(store.default_voice, "custom_a");
    }

    #[tokio::test]
    async fn integration_routes_tts() {
        let runtime = Arc::new(TtsRuntime::bootstrap_for_tests().await);
        let app = crate::build_router(crate::AppState { tts: runtime });

        let list_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/voices")
                    .method("GET")
                    .body(Body::empty())
                    .expect("list req"),
            )
            .await
            .expect("list resp");
        assert_eq!(list_resp.status(), StatusCode::OK);

        let speech_payload = json!({
            "input": "xin chao moi nguoi",
            "response_format": "wav"
        });
        let speech_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/audio/speech")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(speech_payload.to_string()))
                    .expect("speech req"),
            )
            .await
            .expect("speech resp");
        assert_eq!(speech_resp.status(), StatusCode::OK);

        let (boundary, body) = multipart_with_voice("custom_new");
        let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/voices")
                    .method("POST")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("create req"),
            )
            .await
            .expect("create resp");
        assert_eq!(create_resp.status(), StatusCode::CREATED);

        let delete_resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/voices/custom_new")
                    .method("DELETE")
                    .body(Body::empty())
                    .expect("delete req"),
            )
            .await
            .expect("delete resp");
        assert_eq!(delete_resp.status(), StatusCode::OK);
    }
}
