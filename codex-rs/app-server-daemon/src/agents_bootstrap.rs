use anyhow::Result;
use anyhow::anyhow;

use super::BootstrapOptions;
use super::BootstrapOutput;
use super::BootstrapSource;
use super::Daemon;
use super::backend;
use super::client;

impl Daemon {
    pub(super) async fn bootstrap_for_agents(
        &self,
        options: BootstrapOptions,
        source: BootstrapSource,
    ) -> Result<BootstrapOutput> {
        let _operation_lock = self.acquire_operation_lock().await?;
        let settings = self.load_settings().await?;
        if let Ok(info) = client::probe(&self.socket_path).await {
            if self.running_backend(&settings).await?.is_none() {
                return Err(anyhow!(
                    "app server is running but is not managed by codex app-server daemon"
                ));
            }

            let (codex_bin, _) = self.bootstrap_binary(&source)?;
            let updater = backend::pid_update_loop_backend(self.backend_paths(&settings));
            let auto_update_enabled = updater.is_starting_or_running().await?;
            return Ok(self
                .bootstrap_output(
                    &settings,
                    codex_bin,
                    auto_update_enabled,
                    info.app_server_version,
                )
                .await);
        }

        self.bootstrap_locked(options, source).await
    }
}

#[cfg(all(test, unix))]
#[path = "agents_bootstrap_tests.rs"]
mod tests;
