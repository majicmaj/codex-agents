//! Dashboard-specific composer, selection, grouping, and project context policy.

use super::*;
use std::time::Duration;
use std::time::Instant;

const EXIT_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(3);

fn default_footer_hints() -> Vec<(String, String)> {
    vec![
        (String::from("enter"), String::from("start agent")),
        (String::from("ctrl+g"), String::from("group")),
        (String::from("ctrl+↑/↓"), String::from("select project")),
    ]
}

pub(super) struct DashboardComposer {
    pub(super) composer: ChatComposer,
}

impl PickerState {
    pub(super) fn dashboard_resume_state(&self) -> DashboardResumeState {
        DashboardResumeState {
            draft: self
                .dashboard_composer
                .as_ref()
                .map(|dashboard| dashboard.composer.current_text_with_pending())
                .unwrap_or_default(),
            group_mode: self.dashboard_group_mode,
            selected_thread_id: self
                .filtered_rows
                .get(self.selected)
                .and_then(|row| row.thread_id),
            selected_cwd: self
                .filtered_rows
                .get(self.selected)
                .and_then(|row| row.cwd.clone()),
        }
    }

    pub(super) fn initialize_dashboard_composer(
        &mut self,
        enhanced_keys_supported: bool,
        disable_paste_burst: bool,
        app_event_tx: AppEventSender,
        fallback_cwd: &Path,
        keymap: &RuntimeKeymap,
    ) {
        if !self.is_agents_dashboard() {
            return;
        }
        let mut composer = ChatComposer::new(
            /*has_input_focus*/ true,
            app_event_tx,
            enhanced_keys_supported,
            String::from("Start a new agent in the selected project"),
            disable_paste_burst,
        );
        composer.set_frame_requester(self.requester.clone());
        composer.set_keymap_bindings(keymap);
        // A dashboard submission starts a session before it can be acted on. Queueing lets the
        // fully configured ChatWidget parse slash commands and shell input after startup, using
        // the same deferred-input path as commands entered while an existing session is busy.
        composer.set_queue_submissions(/*queue_submissions*/ true);
        composer.set_footer_hint_override(Some(default_footer_hints()));
        self.dashboard_composer = Some(DashboardComposer { composer });
        self.dashboard_fallback_cwd = Some(fallback_cwd.to_path_buf());
    }

    pub(super) fn handle_dashboard_exit_shortcut(&mut self) -> Option<SessionSelection> {
        let now = Instant::now();
        if self
            .dashboard_exit_confirmation_expires_at
            .take()
            .is_some_and(|expires_at| now < expires_at)
        {
            return Some(SessionSelection::Exit);
        }

        self.dashboard_exit_confirmation_expires_at = Some(now + EXIT_CONFIRMATION_TIMEOUT);
        if let Some(dashboard) = self.dashboard_composer.as_mut() {
            dashboard.composer.set_footer_hint_override(Some(vec![(
                String::from("ctrl+c"),
                String::from("again to exit"),
            )]));
        }
        self.requester.schedule_frame_in(EXIT_CONFIRMATION_TIMEOUT);
        self.request_frame();
        None
    }

    pub(super) fn expire_dashboard_exit_confirmation(&mut self) {
        if self
            .dashboard_exit_confirmation_expires_at
            .is_none_or(|expires_at| Instant::now() < expires_at)
        {
            return;
        }

        self.dashboard_exit_confirmation_expires_at = None;
        if let Some(dashboard) = self.dashboard_composer.as_mut() {
            dashboard
                .composer
                .set_footer_hint_override(Some(default_footer_hints()));
        }
    }

    pub(super) fn selected_project_cwd(&self, fallback: &Path) -> PathBuf {
        self.filtered_rows
            .get(self.selected)
            .and_then(|row| row.cwd.clone())
            .unwrap_or_else(|| fallback.to_path_buf())
    }

    pub(super) fn dashboard_project_cwd(&self) -> PathBuf {
        self.selected_project_cwd(
            self.dashboard_fallback_cwd
                .as_deref()
                .unwrap_or_else(|| Path::new(".")),
        )
    }

    pub(super) fn is_agents_dashboard(&self) -> bool {
        matches!(
            self.launch_context,
            SessionPickerLaunchContext::AgentsDashboard
        )
    }

    pub(super) fn handle_dashboard_composer_key(&mut self, key: KeyEvent) -> bool {
        if is_paste_image_shortcut(key) {
            let Some(dashboard_composer) = self.dashboard_composer.as_mut() else {
                return false;
            };
            match paste_image_to_temp_png() {
                Ok((path, info)) => {
                    tracing::debug!(
                        "pasted dashboard image size={}x{} format={}",
                        info.width,
                        info.height,
                        info.encoded_format.label()
                    );
                    dashboard_composer.composer.attach_image(path);
                    self.inline_error = None;
                }
                Err(err) => {
                    tracing::warn!("failed to paste dashboard image: {err}");
                    self.inline_error = Some(format!("Failed to paste image: {err}"));
                }
            }
            self.request_frame();
            return true;
        }

        let composer_is_empty = self
            .dashboard_composer
            .as_ref()
            .is_none_or(|dashboard| dashboard.composer.is_empty());
        let popup_active = self
            .dashboard_composer
            .as_ref()
            .is_some_and(|dashboard| dashboard.composer.popup_active());
        if composer_is_empty && !popup_active {
            if self.list_keymap.move_up.is_pressed(key) {
                self.move_dashboard_selection(/*down*/ false);
                return true;
            }
            if self.list_keymap.move_down.is_pressed(key) {
                self.move_dashboard_selection(/*down*/ true);
                return true;
            }
            if self.list_keymap.move_right.is_pressed(key)
                || self.list_keymap.accept.is_pressed(key)
            {
                return false;
            }
        }
        let Some(dashboard_composer) = self.dashboard_composer.as_mut() else {
            return false;
        };
        if self.list_keymap.accept.is_pressed(key) {
            let text = dashboard_composer.composer.current_text_with_pending();
            match text.trim() {
                "/group project" | "/group status" => {
                    let group_mode = if text.trim().ends_with("project") {
                        DashboardGroupMode::Project
                    } else {
                        DashboardGroupMode::Status
                    };
                    dashboard_composer.composer.set_text_content(
                        String::new(),
                        Vec::new(),
                        Vec::new(),
                    );
                    self.set_dashboard_group_mode(group_mode);
                    return true;
                }
                "/vim" => {
                    dashboard_composer.composer.set_text_content(
                        String::new(),
                        Vec::new(),
                        Vec::new(),
                    );
                    dashboard_composer.composer.toggle_vim_enabled();
                    self.request_frame();
                    return true;
                }
                "/mention" => {
                    dashboard_composer.composer.set_text_content(
                        "@".to_string(),
                        Vec::new(),
                        Vec::new(),
                    );
                    self.request_frame();
                    return true;
                }
                _ => {}
            }
        }
        let (result, _) = dashboard_composer.composer.handle_key_event(key);
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                let user_message = dashboard_user_message_from_submission(
                    &mut dashboard_composer.composer,
                    text,
                    text_elements,
                );
                self.pending_dashboard_submission = Some(user_message.into());
            }
            InputResult::Queued {
                text,
                text_elements,
                action,
                pending_pastes,
            } => {
                let user_message = dashboard_user_message_from_submission(
                    &mut dashboard_composer.composer,
                    text,
                    text_elements,
                );
                self.pending_dashboard_submission = Some(crate::chatwidget::QueuedUserMessage {
                    user_message,
                    action,
                    pending_pastes,
                });
            }
            InputResult::Command(_)
            | InputResult::ServiceTierCommand(_)
            | InputResult::CommandWithArgs(_, _, _) => {
                tracing::error!("dashboard composer dispatched input instead of queueing it");
            }
            InputResult::ParentOwnedInputBlocked | InputResult::None => {}
        }
        if dashboard_composer.composer.is_in_paste_burst() {
            self.requester
                .schedule_frame_in(ChatComposer::recommended_paste_flush_delay());
        }
        self.request_frame();
        true
    }

    pub(super) fn move_dashboard_selection(&mut self, down: bool) {
        self.dashboard_restore_thread_id = None;
        self.dashboard_restore_cwd = None;
        if down {
            if self.selected + 1 < self.filtered_rows.len() {
                self.selected += 1;
            }
            self.maybe_load_more_for_scroll();
        } else if self.selected > 0 {
            self.selected -= 1;
        }
        self.ensure_selected_visible();
        self.load_dashboard_composer_inventory();
        self.request_frame();
    }

    pub(super) fn load_dashboard_composer_inventory(&mut self) {
        if !self.is_agents_dashboard() {
            return;
        }
        let cwd = self.dashboard_project_cwd();
        if self
            .dashboard_inventory_cwd
            .as_ref()
            .is_some_and(|loaded| paths_match(loaded, &cwd))
        {
            return;
        }
        self.dashboard_inventory_cwd = Some(cwd.clone());
        (self.picker_loader)(PickerLoadRequest::DashboardComposerInventory { cwd });
    }

    pub(super) fn take_dashboard_submission(&mut self) -> Option<SessionSelection> {
        let user_message = self.pending_dashboard_submission.take()?;
        let text = user_message.text.trim();
        if text == "/group project" || text == "/group status" {
            self.set_dashboard_group_mode(if text.ends_with("project") {
                DashboardGroupMode::Project
            } else {
                DashboardGroupMode::Status
            });
            return None;
        }
        Some(SessionSelection::StartFreshIn {
            cwd: self.dashboard_project_cwd(),
            user_message,
        })
    }

    pub(super) fn handle_dashboard_command(&mut self) -> bool {
        if !self.is_agents_dashboard() {
            return false;
        }
        let group_mode = match self.query.trim() {
            "/group project" => DashboardGroupMode::Project,
            "/group status" => DashboardGroupMode::Status,
            _ => return false,
        };
        self.query.clear();
        self.search_state = SearchState::Idle;
        self.set_dashboard_group_mode(group_mode);
        true
    }

    pub(super) fn toggle_dashboard_group_mode(&mut self) {
        self.set_dashboard_group_mode(self.dashboard_group_mode.toggle());
    }

    pub(super) fn set_dashboard_group_mode(&mut self, group_mode: DashboardGroupMode) {
        let selected_key = self
            .filtered_rows
            .get(self.selected)
            .and_then(Row::seen_key);
        self.dashboard_group_mode = group_mode;
        self.sort_dashboard_rows();
        if let Some(selected_key) = selected_key
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.seen_key().as_ref() == Some(&selected_key))
        {
            self.selected = index;
        }
        self.scroll_top = self.selected;
        self.ensure_selected_visible();
        self.request_frame();
    }

    pub(super) fn sort_dashboard_rows(&mut self) {
        if !self.is_agents_dashboard() {
            return;
        }
        let group_mode = self.dashboard_group_mode;
        let project_recency = self
            .filtered_rows
            .iter()
            .filter_map(|row| Some((row.cwd.clone()?, row.recency_timestamp())))
            .fold(HashMap::new(), |mut recency, (cwd, updated_at)| {
                recency
                    .entry(cwd)
                    .and_modify(|current: &mut i64| *current = (*current).max(updated_at))
                    .or_insert(updated_at);
                recency
            });
        self.filtered_rows.sort_by(|left, right| {
            let group_order = match group_mode {
                DashboardGroupMode::Project => project_recency
                    .get(right.cwd.as_deref().unwrap_or_else(|| Path::new("")))
                    .copied()
                    .unwrap_or_default()
                    .cmp(
                        &project_recency
                            .get(left.cwd.as_deref().unwrap_or_else(|| Path::new("")))
                            .copied()
                            .unwrap_or_default(),
                    )
                    .then_with(|| left.cwd.cmp(&right.cwd)),
                DashboardGroupMode::Status => left
                    .dashboard_status
                    .unwrap_or(DashboardStatus::Done)
                    .cmp(&right.dashboard_status.unwrap_or(DashboardStatus::Done)),
            };
            group_order
                .then_with(|| right.recency_timestamp().cmp(&left.recency_timestamp()))
                .then_with(|| {
                    left.thread_id
                        .map(|thread_id| thread_id.to_string())
                        .cmp(&right.thread_id.map(|thread_id| thread_id.to_string()))
                })
        });
    }
}

fn dashboard_user_message_from_submission(
    composer: &mut ChatComposer,
    text: String,
    text_elements: Vec<codex_protocol::user_input::TextElement>,
) -> crate::chatwidget::UserMessage {
    crate::chatwidget::UserMessage {
        text,
        local_images: composer.take_recent_submission_images_with_placeholders(),
        remote_image_urls: composer.take_remote_image_urls(),
        text_elements,
        mention_bindings: composer.take_mention_bindings(),
    }
}
