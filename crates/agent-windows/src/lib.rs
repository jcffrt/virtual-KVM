//! RawKVM agent for Windows: library part (driver interface, sink, service),
//! so the platform-independent pieces are tested on every OS.

#[cfg(windows)]
pub mod driver;
pub mod ioctl;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod setup;
pub mod sink;

use std::path::PathBuf;

/// `%ProgramData%\rawkvm`: shared by the service (LocalSystem) and the
/// installer, unlike a per-user directory.
pub fn data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("rawkvm")
}

pub fn config_path() -> PathBuf {
    data_dir().join("agent.toml")
}

/// Run the agent until `stop` resolves or a fatal error, then release and
/// remove every virtual device.
#[cfg(windows)]
pub fn run_agent(stop: impl std::future::Future<Output = ()>) -> anyhow::Result<()> {
    use agent_core::{AgentConfig, AgentOptions, HidSink};
    use tracing::info;

    let path = config_path();
    let cfg = AgentConfig::load_or_init(&path)?;
    info!(node = %cfg.node_id, name = %cfg.node_name, config = %path.display(), "starting");
    let mut sink = sink::VhfSink::new(driver::DriverOpener, cfg.node_id);
    let opts = AgentOptions {
        config_path: path,
        os: proto::Os::Windows,
    };
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                r = agent_core::run(opts, &mut sink) => r,
                _ = stop => {
                    info!("stop requested");
                    Ok(())
                }
            }
        });
    sink.destroy_all();
    result
}
