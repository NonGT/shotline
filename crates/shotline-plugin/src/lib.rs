//! The Plugin trait, external process plugins, discovery, a fake plugin for tests and the
//! conformance kit.

use std::path::Path;

use async_trait::async_trait;
pub use shotline_protocol as protocol;
use shotline_protocol::{
    Balance, Description, EditParams, Edited, ExportParams, Exported, ImportParams, Imported,
    OpenParams, Outputs, PluginError, Quote, Rates, Request, Sequences, Status, Usage, Voices,
};

pub mod conformance;
pub mod discovery;
pub mod fake;
mod process;
mod serve;
pub mod util;

pub use process::ProcessPlugin;
pub use serve::serve;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Plugin(#[from] PluginError),
    #[error("plugin doesn't support {0}")]
    Unsupported(String),
    /// Crashed, hung, spoke garbage, or couldn't be started.
    #[error("plugin {plugin}: {message}")]
    Broken { plugin: String, message: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Built-in plugins implement this directly; external ones go through [`ProcessPlugin`].
#[async_trait]
pub trait Plugin: Send + Sync {
    async fn describe(&self) -> Result<Description>;
    async fn quote(&self, req: &Request) -> Result<Quote>;
    /// Starts a job and returns the plugin's id for it.
    async fn run(&self, req: &Request, output_dir: &Path) -> Result<String>;
    async fn poll(&self, job: &str) -> Result<Status>;
    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs>;
    async fn cancel(&self, job: &str) -> Result<()>;

    async fn rates(&self) -> Result<Rates> {
        Err(Error::Unsupported("rates".into()))
    }

    /// What's left on the account. Must be free to call; shotline reads it before and after jobs
    /// to see what they really cost.
    async fn balance(&self) -> Result<Balance> {
        Err(Error::Unsupported("balance".into()))
    }

    /// The voices audio.tts can use. Must be free to call.
    async fn voices(&self) -> Result<Voices> {
        Err(Error::Unsupported("voices".into()))
    }

    /// An editor plugin's projects for the film in `film`, read only. Must be free to call.
    async fn sequences(&self, _film: &Path) -> Result<Sequences> {
        Err(Error::Unsupported("sequences".into()))
    }

    /// Opens the film's project in the editor, making it from the cut if there's none yet.
    async fn open(&self, _ask: &OpenParams) -> Result<()> {
        Err(Error::Unsupported("open".into()))
    }

    /// Changes one clip of an editor's sequence.
    async fn edit(&self, _ask: &EditParams) -> Result<Edited> {
        Err(Error::Unsupported("edit".into()))
    }

    /// Renders a sequence of an editor's project to a video file.
    async fn export(&self, _ask: &ExportParams) -> Result<Exported> {
        Err(Error::Unsupported("export".into()))
    }

    /// Hands finished takes to the editor, or keeps them until it's open.
    async fn import(&self, _ask: &ImportParams) -> Result<Imported> {
        Err(Error::Unsupported("import".into()))
    }

    /// Charges since an RFC 3339 time.
    async fn usage(&self, _since: &str) -> Result<Usage> {
        Err(Error::Unsupported("usage".into()))
    }
}
