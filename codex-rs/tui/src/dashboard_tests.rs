use super::*;
use pretty_assertions::assert_eq;

#[test]
fn maps_app_server_statuses_to_dashboard_statuses() {
    let statuses = [
        ThreadStatus::NotLoaded,
        ThreadStatus::Idle,
        ThreadStatus::SystemError,
        ThreadStatus::Active {
            active_flags: Vec::new(),
        },
        ThreadStatus::Active {
            active_flags: vec![ThreadActiveFlag::WaitingOnApproval],
        },
        ThreadStatus::Active {
            active_flags: vec![ThreadActiveFlag::WaitingOnUserInput],
        },
    ];

    assert_eq!(
        statuses.map(|status| super::status(&status)),
        [
            DashboardStatus::Done,
            DashboardStatus::Idle,
            DashboardStatus::NeedsInput,
            DashboardStatus::Working,
            DashboardStatus::NeedsInput,
            DashboardStatus::NeedsInput,
        ]
    );
}

#[test]
fn status_order_prioritizes_attention_then_activity() {
    assert_eq!(
        [
            DashboardStatus::NeedsInput,
            DashboardStatus::Working,
            DashboardStatus::Idle,
            DashboardStatus::Done,
        ]
        .map(DashboardStatus::label),
        ["Needs Input", "Working", "Idle", "Done"]
    );
}

#[test]
fn group_mode_toggles_between_project_and_status() {
    assert_eq!(
        DashboardGroupMode::Project.toggle(),
        DashboardGroupMode::Status
    );
    assert_eq!(
        DashboardGroupMode::Status.toggle(),
        DashboardGroupMode::Project
    );
}
