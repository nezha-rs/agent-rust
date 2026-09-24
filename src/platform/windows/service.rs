#[cfg(windows)]
pub use windows::maybe_run;

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::config::AgentConfig;
    use anyhow::bail;
    use clap::Parser;
    use std::{
        ffi::OsString,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };
    use tokio::sync::watch;
    use windows_service::{
        define_windows_service,
        service::{
            ServiceAccess, ServiceAction as FailureAction, ServiceActionType, ServiceControl,
            ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceFailureActions,
            ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    pub fn maybe_run(args: &Args) -> Result<bool> {
        let executable = std::env::current_exe().context("locate executable")?;
        let config = absolute_config_path(args, &executable)?;
        let name = service_name(&executable, &config)?;
        match windows_service::service_dispatcher::start(&name, ffi_service_main) {
            Ok(()) => Ok(true),
            Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(1063) => {
                Ok(false)
            }
            Err(error) => Err(error).context("connect to Windows Service Control Manager"),
        }
    }

    pub fn control(args: &Args, action: ServiceAction, user: bool) -> Result<()> {
        if user {
            bail!("--user is available only for Linux systemd services");
        }
        let executable = std::env::current_exe().context("locate executable")?;
        let config = absolute_config_path(args, &executable)?;
        let name = service_name(&executable, &config)?;
        let manager_access = if matches!(action, ServiceAction::Install) {
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE
        } else {
            ServiceManagerAccess::CONNECT
        };
        let manager = ServiceManager::local_computer(None::<&str>, manager_access)?;

        if matches!(action, ServiceAction::Install) {
            if !config.is_file() {
                bail!("configuration file does not exist: {}", config.display());
            }
            let check_args = Args {
                config: Some(config.clone()),
                server: None,
                client_secret: None,
                uuid: None,
                command: None,
            };
            AgentConfig::load(&check_args).context("validate service configuration")?;
            let info = ServiceInfo {
                name: OsString::from(&name),
                display_name: executable
                    .file_name()
                    .context("executable has no file name")?
                    .to_os_string(),
                service_type: ServiceType::OWN_PROCESS,
                start_type: ServiceStartType::AutoStart,
                error_control: ServiceErrorControl::Normal,
                executable_path: executable,
                launch_arguments: vec![OsString::from("-c"), config.into_os_string()],
                dependencies: vec![],
                account_name: None,
                account_password: None,
            };
            let service = manager
                .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::DELETE)?;
            let setup = (|| -> Result<()> {
                service.set_description("Nezha Monitoring Agent")?;
                service.update_failure_actions(ServiceFailureActions {
                    reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(10)),
                    reboot_msg: None,
                    command: None,
                    actions: Some(vec![FailureAction {
                        action_type: ServiceActionType::Restart,
                        delay: Duration::from_secs(1),
                    }]),
                })?;
                Ok(())
            })();
            if let Err(error) = setup {
                let _ = service.delete();
                return Err(error).context("configure Windows service");
            }
            println!("installed {name}");
            return Ok(());
        }

        let access = match action {
            ServiceAction::Uninstall => {
                ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS | ServiceAccess::STOP
            }
            ServiceAction::Start => ServiceAccess::START,
            ServiceAction::Stop => ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
            ServiceAction::Restart => {
                ServiceAccess::STOP | ServiceAccess::START | ServiceAccess::QUERY_STATUS
            }
            ServiceAction::Install => unreachable!(),
        };
        let service = manager.open_service(&name, access)?;
        match action {
            ServiceAction::Uninstall => {
                if service.query_status()?.current_state != ServiceState::Stopped {
                    stop_and_wait(&service)?;
                }
                service.delete()?;
                println!("uninstalled {name}");
            }
            ServiceAction::Start => service.start(&[] as &[&str])?,
            ServiceAction::Stop => stop_and_wait(&service)?,
            ServiceAction::Restart => {
                stop_and_wait(&service)?;
                service.start(&[] as &[&str])?;
            }
            ServiceAction::Install => unreachable!(),
        }
        Ok(())
    }

    fn stop_and_wait(service: &windows_service::service::Service) -> Result<()> {
        if service.query_status()?.current_state == ServiceState::Stopped {
            return Ok(());
        }
        service.stop()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if service.query_status()?.current_state == ServiceState::Stopped {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        bail!("Windows service did not stop within 20 seconds")
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(arguments: Vec<OsString>) {
        if let Err(error) = run_service(arguments) {
            eprintln!("Windows service failed: {error:#}");
        }
    }

    fn run_service(arguments: Vec<OsString>) -> Result<()> {
        let name = arguments
            .first()
            .context("SCM did not provide a service name")?
            .to_string_lossy()
            .into_owned();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let status_slot = Arc::new(Mutex::new(
            None::<service_control_handler::ServiceStatusHandle>,
        ));
        let callback_status = Arc::clone(&status_slot);
        let handler = move |event| match event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(handle) = *callback_status.lock().unwrap() {
                    let _ = set_status(
                        &handle,
                        ServiceState::StopPending,
                        ServiceControlAccept::empty(),
                        0,
                    );
                }
                let _ = shutdown_tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let status_handle = service_control_handler::register(&name, handler)?;
        *status_slot.lock().unwrap() = Some(status_handle);
        set_status(
            &status_handle,
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            0,
        )?;
        let result = (|| -> Result<()> {
            let args = Args::parse();
            AgentConfig::load(&args).context("load service configuration")?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            set_status(
                &status_handle,
                ServiceState::Running,
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
                0,
            )?;
            runtime.block_on(crate::run_agent(args, shutdown_rx))
        })();
        let exit_code = if result.is_ok() { 0 } else { 1 };
        set_status(
            &status_handle,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            exit_code,
        )?;
        result
    }

    fn set_status(
        handle: &service_control_handler::ServiceStatusHandle,
        state: ServiceState,
        accepts: ServiceControlAccept,
        exit_code: u32,
    ) -> Result<()> {
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepts,
            exit_code: if exit_code == 0 {
                ServiceExitCode::NO_ERROR
            } else {
                ServiceExitCode::ServiceSpecific(exit_code)
            },
            checkpoint: 0,
            wait_hint: if matches!(
                state,
                ServiceState::StartPending | ServiceState::StopPending
            ) {
                Duration::from_secs(20)
            } else {
                Duration::ZERO
            },
            process_id: None,
        })?;
        Ok(())
    }
}
