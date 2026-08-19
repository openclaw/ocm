use super::{Cli, render};

impl Cli {
    pub(super) fn handle_service_install(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "service install")?;
        let Some(name) = args.first() else {
            return Err("service install requires <env>".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.with_progress(
            format!("Enabling {name} in the OCM background service"),
            || self.service_service().install(name),
        )?;
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::service::service_installed(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_service_status(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "service status")?;
        let (args, all_flag) = Self::consume_flag(args, "--all");

        if all_flag || args.is_empty() {
            Self::assert_no_extra_args(&args)?;
            let services = self.service_service().list()?;
            if json_flag {
                self.print_json(&services)?;
                return Ok(0);
            }

            self.stdout_lines(render::service::service_overview(&services, profile));
            return Ok(0);
        }
        let Some(name) = args.first() else {
            unreachable!("handled by the empty-args branch")
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.service_service().status(name)?;
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::service::service_status(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_service_start(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "service start")?;
        let Some(name) = args.first() else {
            return Err("service start requires <env>".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.service_service().start_action(name)?;
        let code = if summary.gateway_ready == Some(false) {
            1
        } else {
            0
        };
        if json_flag {
            self.print_json(&summary)?;
            return Ok(code);
        }

        self.stdout_lines(render::service::service_action(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(code)
    }

    pub(super) fn handle_service_stop(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) = self.consume_human_output_flags(args, "service stop")?;
        let Some(name) = args.first() else {
            return Err("service stop requires <env>".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.service_service().stop(name)?;
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::service::service_action(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_service_restart(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "service restart")?;
        let (args, force) = Self::consume_flag(args, "--force");
        let Some(name) = args.first() else {
            return Err("service restart requires <env>".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self
            .service_service()
            .restart_action_with_options(name, crate::service::ServiceRestartOptions { force })?;
        let code = if summary.gateway_ready == Some(false) {
            1
        } else {
            0
        };
        if json_flag {
            self.print_json(&summary)?;
            return Ok(code);
        }

        self.stdout_lines(render::service::service_action(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(code)
    }

    pub(super) fn handle_service_uninstall(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "service uninstall")?;
        let Some(name) = args.first() else {
            return Err("service uninstall requires <env>".to_string());
        };
        Self::assert_no_extra_args(&args[1..])?;

        let summary = self.service_service().uninstall(name)?;
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::service::service_action(
            &summary,
            profile,
            &self.command_example(),
        ));
        Ok(0)
    }

    pub(super) fn handle_service_refresh_daemon(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag, profile) =
            self.consume_human_output_flags(args, "service refresh-daemon")?;
        let (args, acknowledge_gateway_restarts) =
            Self::consume_flag(args, "--acknowledge-gateway-restarts");
        Self::assert_no_extra_args(&args)?;

        let summary = self
            .supervisor_service()
            .refresh_daemon_explicit(acknowledge_gateway_restarts)?;
        if json_flag {
            self.print_json(&summary)?;
            return Ok(0);
        }

        self.stdout_lines(render::service::service_daemon_refreshed(&summary, profile));
        Ok(0)
    }

    pub(super) fn dispatch_service_command(
        &self,
        action: &str,
        rest: Vec<String>,
    ) -> Result<i32, String> {
        match action {
            "install" => self.handle_service_install(rest),
            "status" => self.handle_service_status(rest),
            "start" => self.handle_service_start(rest),
            "stop" => self.handle_service_stop(rest),
            "restart" => self.handle_service_restart(rest),
            "refresh-daemon" => self.handle_service_refresh_daemon(rest),
            "uninstall" => self.handle_service_uninstall(rest),
            _ => Err(format!("unknown service command: {action}")),
        }
    }
}
