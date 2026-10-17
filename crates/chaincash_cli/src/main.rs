//! ChainCash server CLI.
use anyhow::Result;
use chaincash_app::{ChainCashApp, ChainCashConfig};
use clap::{Parser, Subcommand};
use directories::BaseDirs;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Subcommand)]
enum Command {
    /// Runs the chaincash server
    Run,
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// The level of logging to use for the server
    #[clap(long, global = true, default_value = tracing::Level::INFO.as_str())]
    log_level: tracing::Level,
}

// Returns the OS-dependent directory for log files. See: https://github.com/dirs-dev/directories-rs/issues/81
fn get_log_directory() -> Option<std::path::PathBuf> {
    let base_dirs = BaseDirs::new()?;
    if cfg!(target_os = "linux") {
        Some(base_dirs.state_dir()?.to_owned())
    } else if cfg!(target_os = "macos") {
        Some(base_dirs.home_dir().join("Library/Logs/"))
    } else if cfg!(target_os = "windows") {
        Some(base_dirs.data_dir().to_owned())
    } else {
        None
    }
}

impl Cli {
    pub async fn execute(&self) -> Result<()> {
        let mut log_directory = get_log_directory().unwrap_or(std::env::current_dir()?);
        log_directory.push("chaincash.log.d");
        let appender = tracing_appender::rolling::daily(log_directory, "chaincash.log");
        let (file_writer, _guard) = tracing_appender::non_blocking(appender);
        let tracing_filter =
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // axum logs rejections from built-in extractors with the `axum::rejection`
                // target, at `TRACE` level. `axum::rejection=trace` enables showing those events
                format!("{},axum::rejection=trace", self.log_level).into()
            });
        tracing_subscriber::registry()
            .with(tracing_filter)
            .with(tracing_subscriber::fmt::layer().with_writer(file_writer))
            .with(tracing_subscriber::fmt::layer())
            .init();

        info!("started with {:?}", self);

        match &self.command {
            Command::Run => Ok(ChainCashApp::new(ChainCashConfig::new()?).run().await?),
        }
    }
}

#[allow(deprecated)]
#[tokio::main]
async fn main() -> Result<()> {
    // Setup human panic
    human_panic::setup_panic!();

    Cli::parse().execute().await
}
