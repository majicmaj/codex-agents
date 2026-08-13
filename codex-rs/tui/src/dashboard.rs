//! Agents dashboard entry point.
//!
//! The dashboard deliberately reuses the app-server-backed session inventory and
//! resume lifecycle. Keeping this boundary separate lets the dashboard grow its
//! composer and supervisor without coupling those policies to the resume picker.

use crate::app_event::AppEvent;
use crate::app_event::HistoryBatchEntryResponse;
use crate::app_event::HistoryLookupResponse;
use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerSession;
use crate::bottom_pane::ChatComposer;
use crate::legacy_core::config::Config;
use crate::resume_picker;
use crate::resume_picker::DashboardResumeState;
use crate::resume_picker::SessionSelection;
use crate::tui::Tui;
use codex_app_server_protocol::ThreadActiveFlag;
use codex_app_server_protocol::ThreadStatus;
use codex_protocol::ThreadId;
use color_eyre::eyre::Result;
use tracing::warn;

#[derive(Clone)]
pub(crate) struct DashboardHistory {
    config: codex_message_history::HistoryConfig,
}

impl DashboardHistory {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            config: codex_message_history::HistoryConfig::new(
                config.codex_home.clone(),
                &config.history,
            ),
        }
    }

    pub(crate) async fn initialize(&self, composer: &mut ChatComposer) {
        let (log_id, entry_count) = codex_message_history::history_metadata(&self.config).await;
        composer.set_history_metadata(ThreadId::new(), log_id, entry_count);
    }

    pub(crate) fn handle_event(
        &self,
        event: AppEvent,
        app_event_tx: &AppEventSender,
        composer: &mut ChatComposer,
    ) -> bool {
        match event {
            AppEvent::LookupMessageHistoryEntry {
                thread_id,
                offset,
                log_id,
            } => {
                let history_config = self.config.clone();
                let app_event_tx = app_event_tx.clone();
                tokio::spawn(async move {
                    let entry = tokio::task::spawn_blocking(move || {
                        codex_message_history::lookup(log_id, offset, &history_config)
                    })
                    .await
                    .ok()
                    .flatten()
                    .map(|entry| entry.text);
                    app_event_tx.send(AppEvent::ThreadHistoryEntryResponse {
                        thread_id,
                        event: HistoryLookupResponse::Entry {
                            offset,
                            log_id,
                            entry,
                        },
                    });
                });
                false
            }
            AppEvent::LookupMessageHistoryBatch {
                thread_id,
                cursor,
                log_id,
            } => {
                let history_config = self.config.clone();
                let app_event_tx = app_event_tx.clone();
                tokio::spawn(async move {
                    let event = match tokio::task::spawn_blocking(move || {
                        codex_message_history::lookup_batch(log_id, cursor, &history_config)
                    })
                    .await
                    {
                        Ok(Ok(batch)) => HistoryLookupResponse::Batch {
                            cursor,
                            log_id,
                            entries: batch
                                .entries
                                .into_iter()
                                .map(|entry| HistoryBatchEntryResponse {
                                    offset: entry.offset,
                                    entry: entry.entry.map(|entry| entry.text),
                                })
                                .collect(),
                            next_older_cursor: batch.next_older_cursor,
                        },
                        Ok(Err(err)) => {
                            warn!(error = %err, "dashboard history batch lookup failed");
                            HistoryLookupResponse::BatchError { cursor, log_id }
                        }
                        Err(err) => {
                            warn!(error = %err, "dashboard history batch task failed");
                            HistoryLookupResponse::BatchError { cursor, log_id }
                        }
                    };
                    app_event_tx.send(AppEvent::ThreadHistoryEntryResponse { thread_id, event });
                });
                false
            }
            AppEvent::ThreadHistoryEntryResponse { event, .. } => {
                let updated = match event {
                    HistoryLookupResponse::Entry {
                        offset,
                        log_id,
                        entry,
                    } => composer.on_history_entry_response(log_id, offset, entry),
                    HistoryLookupResponse::Batch {
                        cursor,
                        log_id,
                        entries,
                        next_older_cursor,
                    } => composer.on_history_batch_response(
                        log_id,
                        cursor,
                        entries,
                        next_older_cursor,
                    ),
                    HistoryLookupResponse::BatchError { cursor, log_id } => {
                        composer.on_history_batch_error(log_id, cursor)
                    }
                };
                if updated {
                    composer.sync_popups();
                }
                updated
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum DashboardStatus {
    NeedsInput,
    Working,
    Idle,
    Done,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum DashboardGroupMode {
    #[default]
    Project,
    Status,
}

impl DashboardGroupMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Status => "status",
        }
    }

    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::Project => Self::Status,
            Self::Status => Self::Project,
        }
    }
}

impl DashboardStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "Needs Input",
            Self::Working => "Working",
            Self::Idle => "Idle",
            Self::Done => "Done",
        }
    }
}

pub(crate) fn status(status: &ThreadStatus) -> DashboardStatus {
    match status {
        ThreadStatus::Active { active_flags }
            if active_flags.iter().any(|flag| {
                matches!(
                    flag,
                    ThreadActiveFlag::WaitingOnApproval | ThreadActiveFlag::WaitingOnUserInput
                )
            }) =>
        {
            DashboardStatus::NeedsInput
        }
        ThreadStatus::Active { .. } => DashboardStatus::Working,
        ThreadStatus::Idle => DashboardStatus::Idle,
        ThreadStatus::NotLoaded => DashboardStatus::Done,
        ThreadStatus::SystemError => DashboardStatus::NeedsInput,
    }
}

pub(crate) async fn run(
    tui: &mut Tui,
    config: &Config,
    app_server: AppServerSession,
    resume_state: DashboardResumeState,
) -> Result<SessionSelection> {
    resume_picker::run_agents_dashboard_with_app_server(tui, config, app_server, resume_state).await
}

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod tests;
