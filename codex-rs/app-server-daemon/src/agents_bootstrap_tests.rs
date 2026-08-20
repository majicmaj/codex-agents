use std::os::unix::fs::PermissionsExt;

use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_uds::UnixListener;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio_tungstenite::accept_async;

use super::Daemon;
use crate::BackendKind;
use crate::BootstrapOptions;
use crate::BootstrapOutput;
use crate::BootstrapSource;
use crate::BootstrapStatus;
use crate::backend;
use crate::settings::DaemonSettings;

#[tokio::test]
async fn agents_bootstrap_reuses_running_daemon() {
    let temp_dir = TempDir::new().expect("temp dir");
    let fake_codex_bin = temp_dir.path().join("codex");
    tokio::fs::write(
        &fake_codex_bin,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo \"codex-cli 1.2.3\"\n  exit 0\nfi\nexec sleep 60\n",
    )
    .await
    .expect("write fake codex executable");
    let mut permissions = tokio::fs::metadata(&fake_codex_bin)
        .await
        .expect("fake codex metadata")
        .permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&fake_codex_bin, permissions)
        .await
        .expect("make fake codex executable");

    let daemon = Daemon {
        socket_path: temp_dir.path().join("app-server-control.sock"),
        pid_file: temp_dir.path().join("app-server.pid"),
        update_pid_file: temp_dir.path().join("app-server-updater.pid"),
        operation_lock_file: temp_dir.path().join("daemon.lock"),
        settings_file: temp_dir.path().join("settings.json"),
        managed_codex_bin: temp_dir.path().join("managed-codex"),
    };
    let settings = DaemonSettings {
        remote_control_enabled: false,
    };
    settings
        .save(&daemon.settings_file)
        .await
        .expect("save daemon settings");
    let backend = backend::pid_backend(daemon.backend_paths_with_bin(&settings, &fake_codex_bin));
    let original_pid = backend
        .start()
        .await
        .expect("start fake managed daemon")
        .expect("new fake managed daemon pid");

    let mut listener = UnixListener::bind(&daemon.socket_path)
        .await
        .expect("bind fake app-server socket");
    let codex_home = temp_dir.path().to_path_buf();
    let server_task = tokio::spawn(async move {
        let stream = listener.accept().await?;
        let mut websocket = accept_async(stream).await?;
        let initialize = crate::client::read_message(&mut websocket).await?;
        let JSONRPCMessage::Request(initialize) = initialize else {
            panic!("expected initialize request");
        };
        crate::client::send_message(
            &mut websocket,
            &JSONRPCMessage::Response(JSONRPCResponse {
                id: initialize.id,
                result: serde_json::json!({
                    "userAgent": "codex_app_server/1.2.3",
                    "codexHome": codex_home,
                    "platformFamily": "unix",
                    "platformOs": "macos",
                }),
            }),
        )
        .await?;
        let initialized = crate::client::read_message(&mut websocket).await?;
        let JSONRPCMessage::Notification(initialized) = initialized else {
            panic!("expected initialized notification");
        };
        assert_eq!(initialized.method, "initialized");
        Ok::<(), anyhow::Error>(())
    });

    let output = daemon
        .bootstrap_for_agents(
            BootstrapOptions {
                remote_control_enabled: false,
            },
            BootstrapSource::PackageManaged(fake_codex_bin.clone()),
        )
        .await
        .expect("reuse running daemon");
    server_task
        .await
        .expect("fake app-server task")
        .expect("serve fake app-server connection");
    let original_pid = libc::pid_t::try_from(original_pid).expect("pid fits in pid_t");
    let original_process_is_alive = unsafe { libc::kill(original_pid, 0) == 0 };
    backend.stop().await.expect("stop fake managed daemon");

    assert!(original_process_is_alive);
    assert_eq!(
        output,
        BootstrapOutput {
            status: BootstrapStatus::Bootstrapped,
            backend: BackendKind::Pid,
            auto_update_enabled: false,
            remote_control_enabled: false,
            managed_codex_path: fake_codex_bin,
            managed_codex_version: Some("1.2.3".to_string()),
            socket_path: daemon.socket_path,
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            app_server_version: "1.2.3".to_string(),
        }
    );
}
