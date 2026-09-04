use super::Cli;

impl Cli {
    fn handle_daemon_identity(&self, args: Vec<String>) -> Result<i32, String> {
        Self::assert_no_extra_args(&args)?;
        self.stdout_line(crate::supervisor::SERVICE_EXECUTABLE_IDENTITY);
        Ok(0)
    }

    fn handle_daemon_run(&self, args: Vec<String>) -> Result<i32, String> {
        let (args, json_flag) = Self::consume_flag(args, "--json");
        let (args, once) = Self::consume_flag(args, "--once");
        Self::assert_no_extra_args(&args)?;

        let summary = self.supervisor_service().run(once)?;
        if json_flag {
            self.print_json(&summary)?;
        }

        Ok(
            if once && summary.child_results.iter().any(|result| !result.success) {
                1
            } else {
                0
            },
        )
    }

    pub(super) fn dispatch_internal_command(
        &self,
        action: &str,
        rest: Vec<String>,
    ) -> Result<i32, String> {
        match action {
            "identity" => self.handle_daemon_identity(rest),
            "run" => self.handle_daemon_run(rest),
            "self-update" => self.run_self_update_worker(rest),
            _ => Err(format!("unknown internal command: {action}")),
        }
    }
}
