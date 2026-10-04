use crate::{client::Client, platform, tui, update, Result};
use std::path::PathBuf;

/// The executable that received the command line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// The console manager: the TUI, installer hooks and user commands.
    Console,
    /// The windowless build that Task Scheduler starts for scheduled updates
    /// and detached helpers.
    Background,
}

pub async fn main(entry: Entry) {
    if let Err(error) = run(entry).await {
        eprintln!("{} ({})", tui::error_message(&error), error);
        std::process::exit(1);
    }
}

fn background_command(command: &[&str]) -> bool {
    matches!(
        command,
        ["update", "scheduled"]
            | [
                "update",
                "helper" | "recover-helper",
                _,
                _,
                "--scheduled-task",
                _
            ]
            | ["installer", "guard", _, "--scheduled-task", _]
            | ["installer", "guard", _, "--owner", _, "--scheduled-task", _]
    )
}

async fn run(entry: Entry) -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut root = platform::default_data_dir()?;
    let mut executable = platform::server_executable()?;
    let mut command = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => root = PathBuf::from(args.next().ok_or("data-dir-required")?),
            "--server" => executable = PathBuf::from(args.next().ok_or("server-path-required")?),
            "--help" | "-h" => {
                println!("risunest-sync-manager [--data-dir PATH] [--server EXECUTABLE] [status|start|stop|autostart install|autostart remove|update status|update policy automatic|notify|off|update check|uninstall [--delete-data] [--dry-run] [--yes]]\n명령을 생략하면 관리 메뉴를 엽니다.");
                return Ok(());
            }
            _ => command.push(arg),
        }
    }
    if entry == Entry::Background
        && !background_command(&command.iter().map(String::as_str).collect::<Vec<_>>())
    {
        return Err("invalid-command".into());
    }
    if !root.is_absolute() {
        return Err("absolute-data-dir-required".into());
    }
    root = platform::resolve_manager_root(&root)?;
    let client = Client::new(root.clone())?;
    let manager = platform::manager_executable()?;
    match command
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => tui::run(&root, &executable).await,
        ["status"] => {
            println!("{}", client.status().await?);
            Ok(())
        }
        ["start"] => start_server(&root, &executable, &client).await,
        ["start", "lock-held"] => start_server(&root, &executable, &client).await,
        ["stop"] => {
            crate::lifecycle::stop(&root, &client).await?;
            println!("서버 중지됨");
            Ok(())
        }
        ["prepare-update"] => {
            let _lock = update::try_lock(&root)?;
            if root.join("manager-update/transaction.json").exists() {
                return Err("update-recovery-required".into());
            }
            crate::lifecycle::stop(&root, &client).await
        }
        ["prepare-update", "lock-held"] => {
            if root.join("manager-update/transaction.json").exists() {
                return Err("update-recovery-required".into());
            }
            crate::lifecycle::stop(&root, &client).await
        }
        ["installer", "prepare"] => {
            println!(
                "{}",
                update::begin_installer_guard(&root, &executable, None)?
            );
            Ok(())
        }
        ["installer", "prepare", owner] => {
            println!(
                "{}",
                update::begin_installer_guard(
                    &root,
                    &executable,
                    Some(owner.parse().map_err(|_| "invalid-installer-owner")?)
                )?
            );
            Ok(())
        }
        ["installer", "swap", "lock-held", staged, was_running] => {
            let was_running = match *was_running {
                "true" => true,
                "false" => false,
                _ => return Err("installer-running-intent-invalid".into()),
            };
            update::installer_swap_while_locked(
                &root,
                &executable,
                &PathBuf::from(staged),
                was_running,
            )
        }
        ["installer", "sync-stage", "lock-held", staged] => {
            update::installer_sync_stage_while_locked(&PathBuf::from(staged))
        }
        ["installer", "commit-swap", "lock-held"] => {
            update::installer_commit_swap_while_locked(&root, &executable)
        }
        ["installer", "rollback-swap", "lock-held", was_running] => {
            let was_running = match *was_running {
                "true" => true,
                "false" => false,
                _ => return Err("installer-running-intent-invalid".into()),
            };
            update::installer_rollback_swap_while_locked(&root, &executable, was_running).await
        }
        ["installer", "start-and-verify", "lock-held"] => {
            update::installer_start_and_verify_while_locked(&root, &executable).await
        }
        ["installer", "guard", nonce] => {
            update::run_installer_guard(&root, &executable, nonce, None).await
        }
        ["installer", "guard", nonce, "--scheduled-task", task] => {
            let result = update::run_installer_guard(&root, &executable, nonce, None).await;
            let cleanup = platform::finish_update_helper(&root, task);
            result?;
            cleanup
        }
        ["installer", "guard", nonce, "--owner", owner, "--scheduled-task", task] => {
            let (pid, started) = owner.split_once(':').ok_or("invalid-installer-owner")?;
            let identity = (
                pid.parse().map_err(|_| "invalid-installer-owner")?,
                started.parse().map_err(|_| "invalid-installer-owner")?,
            );
            let result =
                update::run_installer_guard(&root, &executable, nonce, Some(identity)).await;
            let cleanup = platform::finish_update_helper(&root, task);
            result?;
            cleanup
        }
        ["installer", "finish", nonce] => update::finish_installer_guard(&root, nonce),
        #[cfg(not(windows))]
        ["removal-helper", parent, mode @ ("delete" | "preserve")] => {
            crate::removal::finish_after_exit(
                &root,
                &executable,
                parent.parse().map_err(|_| "invalid-removal-parent")?,
                *mode == "delete",
            )
            .await
        }
        ["uninstall", "lock-held"] => crate::removal::cleanup_services(&root, &executable).await,
        ["installer", "forget-removal"] => crate::removal::forget_registration(&root, &executable),
        ["installer", "delete-data"] => crate::removal::delete_registered_data(&root, &executable),
        ["uninstall", ..] => crate::removal::cli(&root, &executable, &command[1..]).await,
        ["autostart", "status"] => {
            let status = platform::startup(&root, &executable, "status")?;
            println!(
                "{}",
                serde_json::to_string(&status).map_err(|_| "startup-status-unavailable")?
            );
            Ok(())
        }
        ["autostart", action @ ("install" | "remove")] => {
            update::set_autostart(&root, &manager, &executable, *action == "install")?;
            println!("자동 실행 설정을 변경했습니다.");
            Ok(())
        }
        ["autostart", action @ ("install" | "remove"), "lock-held"] => {
            update::installer_set_autostart_while_locked(
                &root,
                &manager,
                &executable,
                *action == "install",
            )?;
            println!("자동 실행 설정을 변경했습니다.");
            Ok(())
        }
        ["update", "status"] => {
            let settings = update::load_settings(&root)?;
            let status = update::load_status(&root)?;
            let schedule =
                platform::update_schedule(&root, &manager, &executable, settings.policy, "status")?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "settings":settings,
                    "status":status,
                    "schedule":schedule,
                }))
                .map_err(|_| "update-status-unavailable")?
            );
            Ok(())
        }
        ["update", "schedule", "reconcile"] => {
            update::reconcile_schedule(&root, &manager, &executable)
        }
        ["update", "schedule", "reconcile", "lock-held"] => {
            update::reconcile_schedule_while_locked(&root, &manager, &executable)
        }
        ["update", "schedule", "remove", "lock-held"] => {
            update::remove_schedule_while_locked(&root, &manager, &executable)
        }
        ["update", "policy", policy] => {
            let policy = update::UpdatePolicy::parse(policy)?;
            let value = update::set_policy(&root, &manager, &executable, policy)?;
            println!(
                "{}",
                serde_json::to_string(&value).map_err(|_| "update-settings-unavailable")?
            );
            Ok(())
        }
        ["update", "policy", policy, "lock-held"] => {
            let policy = update::UpdatePolicy::parse(policy)?;
            let value =
                update::installer_set_policy_while_locked(&root, &manager, &executable, policy)?;
            println!(
                "{}",
                serde_json::to_string(&value).map_err(|_| "update-settings-unavailable")?
            );
            Ok(())
        }
        ["update", mode @ ("check" | "scheduled")] => {
            let mode = if *mode == "scheduled" {
                update::RunMode::Scheduled
            } else {
                update::RunMode::Manual
            };
            let outcome = update::run(&root, &executable, mode).await?;
            println!(
                "{}",
                serde_json::to_string(&outcome).map_err(|_| "update-status-unavailable")?
            );
            Ok(())
        }
        ["update", "helper", parent, install] => {
            let parent = parent.parse::<u32>().map_err(|_| "update-parent-invalid")?;
            let install = PathBuf::from(install);
            if !install.is_absolute() {
                return Err("managed-install-path-invalid".into());
            }
            let outcome = update::run_helper(&root, &executable, parent, &install).await?;
            println!(
                "{}",
                serde_json::to_string(&outcome).map_err(|_| "update-status-unavailable")?
            );
            Ok(())
        }
        ["update", "helper", parent, install, "--scheduled-task", task] => {
            let parent = parent.parse::<u32>().map_err(|_| "update-parent-invalid")?;
            let install = PathBuf::from(install);
            if !install.is_absolute() {
                return Err("managed-install-path-invalid".into());
            }
            let result = update::run_helper(&root, &executable, parent, &install).await;
            let cleanup = platform::finish_update_helper(&root, task);
            let outcome = result?;
            cleanup?;
            println!(
                "{}",
                serde_json::to_string(&outcome).map_err(|_| "update-status-unavailable")?
            );
            Ok(())
        }
        ["update", "recover-helper", parent, install] => {
            let parent = parent.parse::<u32>().map_err(|_| "update-parent-invalid")?;
            let install = PathBuf::from(install);
            if !install.is_absolute() {
                return Err("managed-install-path-invalid".into());
            }
            let outcome = update::run_recovery_helper(&root, &executable, parent, &install).await?;
            println!(
                "{}",
                serde_json::to_string(&outcome).map_err(|_| "update-status-unavailable")?
            );
            Ok(())
        }
        ["update", "recover-helper", parent, install, "--scheduled-task", task] => {
            let parent = parent.parse::<u32>().map_err(|_| "update-parent-invalid")?;
            let install = PathBuf::from(install);
            if !install.is_absolute() {
                return Err("managed-install-path-invalid".into());
            }
            let result = update::run_recovery_helper(&root, &executable, parent, &install).await;
            let cleanup = platform::finish_update_helper(&root, task);
            let outcome = result?;
            cleanup?;
            println!(
                "{}",
                serde_json::to_string(&outcome).map_err(|_| "update-status-unavailable")?
            );
            Ok(())
        }
        _ => Err("invalid-command".into()),
    }
}

async fn start_server(
    root: &std::path::Path,
    executable: &std::path::Path,
    client: &Client,
) -> Result<()> {
    if client.status().await.is_ok() {
        return Ok(());
    }
    platform::start(root, executable)?;
    for _ in 0..50 {
        if client.status().await.is_ok() {
            println!("서버 실행 중");
            return Ok(());
        }
        if let Some(error) = platform::startup_error(&root) {
            return Err(error);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(platform::startup_error(&root).unwrap_or_else(|| "server-not-ready".into()))
}

#[cfg(test)]
mod tests {
    use super::background_command;

    #[test]
    fn the_windowless_build_accepts_only_task_scheduler_commands() {
        let accepted: [&[&str]; 5] = [
            &["update", "scheduled"],
            &[
                "update",
                "helper",
                "1",
                "install",
                "--scheduled-task",
                "task",
            ],
            &[
                "update",
                "recover-helper",
                "1",
                "install",
                "--scheduled-task",
                "task",
            ],
            &["installer", "guard", "nonce", "--scheduled-task", "task"],
            &[
                "installer",
                "guard",
                "nonce",
                "--owner",
                "1:2",
                "--scheduled-task",
                "task",
            ],
        ];
        for command in accepted {
            assert!(background_command(command), "{command:?}");
        }
        let refused: [&[&str]; 8] = [
            &[],
            &["status"],
            &["start"],
            &["update", "check"],
            &["update", "helper", "1", "install"],
            &["installer", "prepare"],
            &["autostart", "install"],
            &["uninstall", "--yes"],
        ];
        for command in refused {
            assert!(!background_command(command), "{command:?}");
        }
    }
}
