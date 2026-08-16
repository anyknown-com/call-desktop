//! Ports of `elevenlabs-stt.ts`, `elevenlabs-tts.ts` and `openai-tts.ts`.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use reqwest::StatusCode;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::TrackType;
use symphonia::core::io::MediaSourceStream;

use crate::http::check_status;
use crate::pcm::{s16le_stream_to_pcm, single_chunk_pcm};
use crate::wav::encode_wav;
use crate::{
    ElevenLabsSttConfig, ElevenLabsTtsConfig, Error, OpenAiTtsConfig, PcmStream, Result, SttClient,
    TtsClient,
};

const ELEVENLABS_BASE: &str = "https://api.elevenlabs.io";
const OPENAI_BASE: &str = "https://api.openai.com";
/// Both ElevenLabs `pcm_24000` and OpenAI `pcm` are 24 kHz s16le mono.
const PCM_RATE: u32 = 24_000;

/// ElevenLabs Scribe batch transcription.
pub struct ElevenLabsStt {
    http: reqwest::Client,
    cfg: ElevenLabsSttConfig,
    base_url: String,
}

impl ElevenLabsStt {
    pub fn new(cfg: ElevenLabsSttConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            cfg,
            base_url: ELEVENLABS_BASE.into(),
        }
    }

    /// Override the API origin — for tests and proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl SttClient for ElevenLabsStt {
    async fn transcribe(&self, audio: &[f32], sample_rate: u32) -> Result<String> {
        const P: &str = "elevenlabs-stt";
        let file = Part::bytes(encode_wav(audio, sample_rate))
            .file_name("utterance.wav")
            .mime_str("audio/wav")
            .map_err(Error::transport(P))?;
        let mut form = Form::new()
            .text("model_id", self.cfg.model.clone())
            .part("file", file)
            .text("tag_audio_events", "false");
        if !self.cfg.language_code.is_empty() {
            form = form.text("language_code", self.cfg.language_code.clone());
        }
        let res = self
            .http
            .post(format!("{}/v1/speech-to-text", self.base_url))
            .header("xi-api-key", &self.cfg.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(Error::transport(P))?;
        let res = check_status(P, res).await?;
        let json: serde_json::Value = res.json().await.map_err(Error::transport(P))?;
        Ok(json
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .trim()
            .to_string())
    }
}

/// ElevenLabs streaming TTS: `pcm_24000` first, mp3 fallback (remembered per instance).
pub struct ElevenLabsTts {
    http: reqwest::Client,
    cfg: ElevenLabsTtsConfig,
    base_url: String,
    /// Some tiers reject PCM output formats; once seen we use mp3 for the rest of the session.
    pcm_refused: AtomicBool,
}

impl ElevenLabsTts {
    pub fn new(cfg: ElevenLabsTtsConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            cfg,
            base_url: ELEVENLABS_BASE.into(),
            pcm_refused: AtomicBool::new(false),
        }
    }

    /// Override the API origin — for tests and proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Whether this instance has fallen back to mp3 output.
    pub fn pcm_refused(&self) -> bool {
        self.pcm_refused.load(Ordering::Relaxed)
    }

    async fn request(&self, text: &str, format: &str) -> Result<reqwest::Response> {
        self.http
            .post(format!(
                "{}/v1/text-to-speech/{}/stream?output_format={format}",
                self.base_url,
                url_encode(&self.cfg.voice_id)
            ))
            .header("xi-api-key", &self.cfg.api_key)
            .header("accept", "audio/*")
            .json(&serde_json::json!({ "text": text, "model_id": self.cfg.model }))
            .send()
            .await
            .map_err(Error::transport("elevenlabs-tts"))
    }
}

/// Percent-encode a path segment (port of `encodeURIComponent` for the voice id).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[async_trait]
impl TtsClient for ElevenLabsTts {
    async fn synthesize(&self, text: &str) -> Result<PcmStream> {
        const P: &str = "elevenlabs-tts";
        if !self.pcm_refused.load(Ordering::Relaxed) {
            let res = self.request(text, &format!("pcm_{PCM_RATE}")).await?;
            let status = res.status();
            if status.is_success() {
                return Ok(s16le_stream_to_pcm(res.bytes_stream(), PCM_RATE, P));
            }
            if matches!(
                status,
                StatusCode::BAD_REQUEST | StatusCode::PAYMENT_REQUIRED | StatusCode::FORBIDDEN
            ) {
                tracing::info!(%status, "elevenlabs refused pcm output; falling back to mp3 for this session");
                self.pcm_refused.store(true, Ordering::Relaxed);
            } else {
                check_status(P, res).await?;
            }
        }
        let res = self.request(text, "mp3_44100_128").await?;
        let res = check_status(P, res).await?;
        let bytes = res.bytes().await.map_err(Error::transport(P))?;
        let (samples, rate) =
            decode_mp3_mono(&bytes).map_err(|e| Error::protocol(P, format!("mp3 decode: {e}")))?;
        Ok(single_chunk_pcm(samples, rate))
    }
}

/// Decode a whole compressed audio file (mp3) to mono f32 with `symphonia`.
pub fn decode_mp3_mono(
    data: &[u8],
) -> std::result::Result<(Vec<f32>, u32), symphonia::core::errors::Error> {
    use symphonia::core::errors::Error as SErr;
    let mss = MediaSourceStream::new(Box::new(Cursor::new(data.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3").mime_type("audio/mpeg");
    let mut format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        Default::default(),
        Default::default(),
    )?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or(SErr::Unsupported("no audio track"))?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = &track.codec_params else {
        return Err(SErr::Unsupported("no audio codec parameters"));
    };
    let mut decoder =
        symphonia::default::get_codecs().make_audio_decoder(params, &Default::default())?;
    let mut mono = Vec::new();
    let mut interleaved = Vec::new();
    let mut rate = 0u32;
    while let Some(packet) = match format.next_packet() {
        Ok(p) => p,
        Err(SErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => None,
        Err(e) => return Err(e),
    } {
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SErr::DecodeError(_)) => continue, // skip a corrupt frame, like most players
            Err(e) => return Err(e),
        };
        let channels = buf.spec().channels().count().max(1);
        rate = buf.spec().rate();
        interleaved.clear();
        buf.copy_to_vec_interleaved::<f32>(&mut interleaved);
        mono.extend(
            interleaved
                .chunks_exact(channels)
                .map(|f| f.iter().sum::<f32>() / channels as f32),
        );
    }
    if rate == 0 {
        return Err(SErr::DecodeError("no audio frames decoded"));
    }
    Ok((mono, rate))
}

/// OpenAI `/v1/audio/speech` with `response_format: "pcm"` (24 kHz s16le mono).
pub struct OpenAiTts {
    http: reqwest::Client,
    cfg: OpenAiTtsConfig,
    base_url: String,
}

impl OpenAiTts {
    pub fn new(cfg: OpenAiTtsConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            cfg,
            base_url: OPENAI_BASE.into(),
        }
    }

    /// Override the API origin — for tests and proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl TtsClient for OpenAiTts {
    async fn synthesize(&self, text: &str) -> Result<PcmStream> {
        const P: &str = "openai-tts";
        let res = self
            .http
            .post(format!("{}/v1/audio/speech", self.base_url))
            .bearer_auth(&self.cfg.api_key)
            .json(&serde_json::json!({
                "model": self.cfg.model,
                "input": text,
                "voice": self.cfg.voice,
                "response_format": "pcm",
            }))
            .send()
            .await
            .map_err(Error::transport(P))?;
        let res = check_status(P, res).await?;
        Ok(s16le_stream_to_pcm(res.bytes_stream(), PCM_RATE, P))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_voice_id_like_encode_uri_component() {
        assert_eq!(url_encode("21m00Tcm4TlvDq8ikWAM"), "21m00Tcm4TlvDq8ikWAM");
        assert_eq!(url_encode("a b/c?d=é"), "a%20b%2Fc%3Fd%3D%C3%A9");
    }
}
