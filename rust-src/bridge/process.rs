use std::io::{BufRead, BufReader};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use tracing::info;

use crate::env_config::apply_codex_child_env;
use crate::error::AppError;
use crate::models::WarningEvent;
use crate::runtime::get_codex_app_server_config_args;

use super::CodexAppServerBridge;

impl CodexAppServerBridge {
    pub async fn stop(&self) -> Result<(), AppError> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        info!(
            cwd = %self.inner.cwd.display(),
            codex_bin = %self.inner.codex_bin,
            "stopping app-server bridge"
        );

        self.fail_pending(format!(
            "{} app-server process is not available",
            self.inner.codex_bin
        ));

        self.inner.stdin.lock().unwrap().take();

        if let Some(mut child) = self.inner.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        Ok(())
    }

    pub(super) fn spawn_child(&self) -> Result<(), AppError> {
        info!(
            codex_bin = %self.inner.codex_bin,
            cwd = %self.inner.cwd.display(),
            debug = self.inner.debug,
            "spawning codex app-server child"
        );
        let mut child = Command::new(&self.inner.codex_bin);
        child
            .arg("app-server")
            .args(get_codex_app_server_config_args())
            .current_dir(&self.inner.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        apply_codex_child_env(&mut child);

        let mut child = child.spawn().map_err(|error| {
            AppError::internal(format!(
                "Failed to start {} app-server: {error}",
                self.inner.codex_bin
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::internal("Failed to capture app-server stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::internal("Failed to capture app-server stdout"))?;

        *self.inner.stdin.lock().unwrap() = Some(stdin);
        *self.inner.child.lock().unwrap() = Some(child);
        self.inner.closed.store(false, Ordering::SeqCst);

        self.spawn_stdout_thread(stdout);
        self.spawn_wait_thread();
        info!("codex app-server child spawned");

        Ok(())
    }

    fn spawn_stdout_thread(&self, stdout: ChildStdout) {
        let bridge = self.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => bridge.handle_line(line),
                    Err(error) => {
                        bridge.emit_warning(WarningEvent {
                            warning_type: "stdio-read-error".to_string(),
                            message: "Failed to read app-server message".to_string(),
                            detail: Some(error.to_string()),
                        });
                        break;
                    }
                }
            }
        });
    }

    fn spawn_wait_thread(&self) {
        let bridge = self.clone();
        thread::spawn(move || {
            loop {
                if bridge.inner.closed.load(Ordering::SeqCst) {
                    break;
                }

                let status = {
                    let mut child_guard = bridge.inner.child.lock().unwrap();
                    match child_guard.as_mut() {
                        Some(child) => child.try_wait(),
                        None => break,
                    }
                };

                match status {
                    Ok(Some(exit_status)) => {
                        bridge.inner.closed.store(true, Ordering::SeqCst);
                        bridge.fail_pending(format!(
                            "{} app-server exited before replying",
                            bridge.inner.codex_bin
                        ));
                        bridge.emit_warning(WarningEvent {
                            warning_type: "process-exit".to_string(),
                            message: format!("{} app-server exited", bridge.inner.codex_bin),
                            detail: Some(format!("code={:?}", exit_status.code())),
                        });
                        break;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(250)),
                    Err(error) => {
                        bridge.emit_warning(WarningEvent {
                            warning_type: "process-error".to_string(),
                            message: format!(
                                "Failed to inspect {} app-server",
                                bridge.inner.codex_bin
                            ),
                            detail: Some(error.to_string()),
                        });
                        break;
                    }
                }
            }
        });
    }
}
