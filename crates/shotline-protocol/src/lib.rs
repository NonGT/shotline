//! Messages between shotline and plugins. See docs/PLUGINS.md.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const PROTOCOL_VERSION: u32 = 1;

/// What a model does. The standard ones are understood by shotline; `x.<name>` is anything else.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
    ImageGenerate,
    ImageEdit,
    #[default]
    VideoGenerate,
    VideoLipsync,
    VideoUpscale,
    VideoInterpolate,
    AudioTts,
    AudioMusic,
    AudioSfx,
    AudioTranscribe,
    TextGenerate,
    Custom(String),
}

const STANDARD_OPS: [(Op, &str); 11] = [
    (Op::ImageGenerate, "image.generate"),
    (Op::ImageEdit, "image.edit"),
    (Op::VideoGenerate, "video.generate"),
    (Op::VideoLipsync, "video.lipsync"),
    (Op::VideoUpscale, "video.upscale"),
    (Op::VideoInterpolate, "video.interpolate"),
    (Op::AudioTts, "audio.tts"),
    (Op::AudioMusic, "audio.music"),
    (Op::AudioSfx, "audio.sfx"),
    (Op::AudioTranscribe, "audio.transcribe"),
    (Op::TextGenerate, "text.generate"),
];

impl Op {
    pub fn as_str(&self) -> &str {
        match self {
            Op::Custom(s) => s,
            op => STANDARD_OPS.iter().find(|(o, _)| o == op).unwrap().1,
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for Op {
    type Err = String;

    fn from_str(s: &str) -> Result<Op, String> {
        if let Some((op, _)) = STANDARD_OPS.iter().find(|(_, n)| *n == s) {
            return Ok(op.clone());
        }
        match s.strip_prefix("x.") {
            Some(rest) if !rest.is_empty() => Ok(Op::Custom(s.to_string())),
            _ => Err(format!(
                "unknown operation {s:?}, custom ones start with x."
            )),
        }
    }
}

impl Serialize for Op {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Op, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Reply to `describe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Description {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    /// Environment variables the plugin needs. Only these are passed to it.
    #[serde(default)]
    pub env: Vec<String>,
    pub models: Vec<ModelInfo>,
    /// It links shotline to a video editor: it answers `sequences`, and `open` if it can.
    #[serde(default, skip_serializing_if = "is_false")]
    pub editor: bool,
}

/// One model and what it accepts. Limits that don't apply are left out.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub op: Op,
    /// How a reference image is named in the prompt, `{n}` counting from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_images: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_audio: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub whole_seconds: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolutions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aspects: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub start_frame: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub start_frame_with_refs: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub audio_input: bool,
    /// Audio only goes along with at least one reference image.
    #[serde(default, skip_serializing_if = "is_false")]
    pub audio_needs_image: bool,
    /// The lines go as one soundtrack: shotline mixes them at their times into a single audio
    /// reference. Otherwise each line is its own clip, up to `max_audio`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub audio_mix: bool,
    /// Where the start frame can't go (with references, or alone with audio), it can be sent as
    /// the first reference picture instead, with the prompt saying to open on it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub frame_as_reference: bool,
    /// Name of a prompts/models/<notes>.md template with per-model tweaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// JSON Schema for `params`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params_schema: Option<serde_json::Value>,
    /// JSON Schema for a voice's `settings`, for audio.tts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_schema: Option<serde_json::Value>,
    /// How it's charged, for people choosing a model, e.g. "$0.05 a second at 480P". The quote
    /// is still what counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Data,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub kind: MediaKind,
    pub path: PathBuf,
}

/// A line spoken in the video, sent with the lines' audio for the plugin to put in the prompt
/// the way its model understands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpokenLine {
    /// The speaker's name as the prompt has it.
    pub who: String,
    /// As written, delivery tags like "[shout]" included.
    pub text: String,
    /// The audio reference it's in, counting audio references from 0. With `audio_mix` every
    /// line is in the one mixed clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<usize>,
    /// Seconds into the video when it starts, when that's known: set by the person, or where the
    /// mix put it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    /// How the prompt names the speaker's reference pictures, e.g. `Image 1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shown_in: Vec<String>,
}

/// Input to `quote` and `run`. Which fields matter depends on the operation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Request {
    pub op: Op,
    pub model: String,
    /// Generation prompt, the text to speak for audio.tts, or the whole request for
    /// text.generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// In the order the prompt numbers them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
    /// The shot's lines, when their audio goes along.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<SpokenLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_frame: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_frame: Option<PathBuf>,
    /// Named input files for processing ops: `video`, `audio`, `image`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, PathBuf>,
    /// Plugin-specific voice settings for audio.tts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<serde_json::Value>,
    /// Passed through from the shot or command line untouched.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
}

/// A price. `unit` is a currency code or the provider's own credit name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub amount: f64,
    pub unit: String,
    /// What was counted, e.g. 8 seconds or 120 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunParams {
    #[serde(flatten)]
    pub request: Request,
    /// Where `fetch` will put outputs. Plugins may write there early too.
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRef {
    pub job: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl State {
    pub fn is_finished(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    /// 0.0 to 1.0 when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginError>,
    /// What was actually charged, once known. Failed jobs can be charged too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Quote>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchParams {
    pub job: String,
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// File name inside the output folder.
    pub file: String,
    pub kind: MediaKind,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Outputs {
    pub outputs: Vec<Output>,
    /// Anything else the job produced, e.g. a transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub unit: String,
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Rates {
    pub rates: Vec<Rate>,
}

/// An amount of money or credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Amount {
    pub amount: f64,
    pub unit: String,
}

/// What's left on the account, one line per currency or kind of credit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Balance {
    pub balances: Vec<Amount>,
}

/// A voice on the account, for audio.tts.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A URL to a short recording of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Voices {
    pub voices: Vec<VoiceInfo>,
}

/// Params of `sequences`: the film's folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequencesParams {
    pub film: PathBuf,
}

/// Params of `open`: a project from `sequences` to open in its editor, at a sequence if it can.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenParams {
    pub file: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<String>,
}

/// Reply to `sequences`: the editor's projects for the film.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Sequences {
    pub edits: Vec<EditInfo>,
}

/// One editor project, or why it couldn't be read.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EditInfo {
    /// Its file; shotline reads the sequences again once it changes.
    pub file: PathBuf,
    pub name: String,
    /// The editor's name for people, e.g. "FilmCraft".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
    /// The plugin that answered; filled in by shotline.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plugin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub sequences: Vec<SequenceInfo>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SequenceInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// V1 first.
    pub video: Vec<TrackInfo>,
    /// A1 first.
    pub audio: Vec<TrackInfo>,
}

impl SequenceInfo {
    /// How long it runs: to the end of its last clip.
    pub fn length(&self) -> f64 {
        self.video
            .iter()
            .chain(&self.audio)
            .flat_map(|t| &t.clips)
            .map(|c| c.start + c.length)
            .fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TrackInfo {
    pub name: String,
    /// Shown (video) or not muted (audio).
    pub enabled: bool,
    pub clips: Vec<ClipInfo>,
}

/// A clip on a track. Times in seconds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClipInfo {
    pub name: String,
    /// None for what isn't a file: a nested sequence, a title, a generator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    pub start: f64,
    pub length: f64,
    /// Where in the file it starts.
    #[serde(default)]
    pub from: f64,
    pub speed: f64,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageParams {
    /// RFC 3339
    pub since: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Charge {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    pub time: String,
    pub amount: f64,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub charges: Vec<Charge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Validation,
    ContentPolicy,
    Auth,
    Quota,
    NotFound,
    Unsupported,
    Other,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(match self {
            ErrorKind::Validation => "validation",
            ErrorKind::ContentPolicy => "content policy",
            ErrorKind::Auth => "auth",
            ErrorKind::Quota => "quota",
            ErrorKind::NotFound => "not found",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Other => "error",
        })
    }
}

/// The one error shape every plugin uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginError {
    pub kind: ErrorKind,
    pub message: String,
    /// Path into the request this is about: `prompt`, `references[2]`, `inputs.audio`, `params.seed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl PluginError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            field: None,
        }
    }

    pub fn field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(field) = &self.field {
            write!(f, " ({field})")?;
        }
        Ok(())
    }
}

impl std::error::Error for PluginError {}

pub mod rpc {
    //! JSON-RPC 2.0 envelopes, one JSON object per line.

    use serde::{Deserialize, Serialize};

    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const PARSE_ERROR: i64 = -32700;
    /// Application error; `data` holds a PluginError.
    pub const PLUGIN_ERROR: i64 = 1;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Request {
        pub jsonrpc: String,
        pub id: u64,
        pub method: String,
        #[serde(default)]
        pub params: serde_json::Value,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Response {
        pub jsonrpc: String,
        pub id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub result: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub error: Option<Error>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Error {
        pub code: i64,
        pub message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub data: Option<serde_json::Value>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ops_round_trip() {
        for (op, name) in STANDARD_OPS {
            assert_eq!(serde_json::to_value(&op).unwrap(), json!(name));
            assert_eq!(name.parse::<Op>().unwrap(), op);
        }
        let x: Op = serde_json::from_value(json!("x.critic")).unwrap();
        assert_eq!(x, Op::Custom("x.critic".into()));
        assert!("video.dance".parse::<Op>().is_err());
        assert!("x.".parse::<Op>().is_err());
    }

    #[test]
    fn request_leaves_out_empty_fields() {
        let req = Request {
            op: Op::AudioTts,
            model: "eleven_v3".into(),
            prompt: Some("สวัสดี".into()),
            ..Request::default()
        };
        assert_eq!(
            serde_json::to_value(&req).unwrap(),
            json!({"op": "audio.tts", "model": "eleven_v3", "prompt": "สวัสดี"})
        );
    }

    #[test]
    fn run_params_flatten_the_request() {
        let v = json!({"op": "video.upscale", "model": "m", "inputs": {"video": "/a.mp4"}, "output_dir": "/out"});
        let p: RunParams = serde_json::from_value(v).unwrap();
        assert_eq!(p.request.inputs["video"], PathBuf::from("/a.mp4"));
        assert_eq!(p.output_dir, PathBuf::from("/out"));
    }

    #[test]
    fn model_info_defaults() {
        let m: ModelInfo = serde_json::from_value(json!({"id": "x", "op": "audio.tts"})).unwrap();
        assert!(!m.start_frame);
        assert_eq!(m.max_images, None);
    }

    #[test]
    fn error_display() {
        let e = PluginError::new(ErrorKind::Validation, "too many images").field("references[9]");
        assert_eq!(e.to_string(), "validation: too many images (references[9])");
    }
}
