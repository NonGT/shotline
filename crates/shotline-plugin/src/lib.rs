//! The Plugin trait, external process plugins, discovery, a fake plugin for tests and the
//! conformance kit.

use std::path::Path;

use async_trait::async_trait;
pub use shotline_protocol as protocol;
use shotline_protocol::{Description, Outputs, PluginError, Quote, Rates, Request, Status, Usage};

pub mod conformance;
pub mod discovery;
pub mod fake;
mod process;
mod serve;

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

    /// Charges since an RFC 3339 time.
    async fn usage(&self, _since: &str) -> Result<Usage> {
        Err(Error::Unsupported("usage".into()))
    }
}
