use super::{Cli, render};

impl Cli {
    fn is_help_flag(value: &str) -> bool {
        matches!(value, "--help" | "-h")
    }

    fn is_help_token(value: &str) -> bool {
        value == "help" || Self::is_help_flag(value)
    }

    fn print_help_text(&self, text: String) -> Result<i32, String> {
        self.stdout_text(&text)?;
        Ok(0)
    }

    fn render_help_topic(&self, path: &[&str]) -> Result<String, String> {
        let cmd = self.command_example();
        match path {
            [] => Ok(render::help::root_help(&cmd)),
            ["help"] | ["--help"] | ["-h"] => Ok(render::help::root_help(&cmd)),
            ["setup"] => Ok(render::help::setup_help(&cmd)),
            ["dev"] => Ok(render::help::dev_help(&cmd)),
            ["start"] => Ok(render::help::start_help(&cmd)),
            ["upgrade"] => Ok(render::help::upgrade_help(&cmd)),
            ["upgrade", "rollback"] => Ok(render::help::upgrade_rollback_help(&cmd)),
            ["doctor"] => Ok(render::help::doctor_help(&cmd)),
            ["logs"] => Ok(render::help::logs_help(&cmd)),
            ["init"] => Ok(render::help::init_help(&cmd)),
            ["self"] => Ok(render::help::self_help(&cmd)),
            ["env"] => Ok(render::help::env_help(&cmd)),
            ["migrate"] => Ok(render::help::migrate_help(&cmd)),
            ["adopt"] => Ok(render::help::adopt_help(&cmd)),
            ["release"] => Ok(render::help::release_help(&cmd)),
            ["adopt", action] => render::help::adopt_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown adopt command: {action}")),
            ["self", action] => render::help::self_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown self command: {action}")),
            ["env", "snapshot"] => Ok(render::help::env_snapshot_help(&cmd)),
            ["env", "artifact"] | ["env", "artifact", "export"] => {
                Ok(render::help::env_artifact_help(&cmd))
            }
            ["env", action] => render::help::env_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown env command: {action}")),
            ["release", action] => render::help::release_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown release command: {action}")),
            ["env", "snapshot", action] => render::help::env_snapshot_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown env snapshot command: {action}")),
            ["launcher"] => Ok(render::help::launcher_help(&cmd)),
            ["launcher", action] => render::help::launcher_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown launcher command: {action}")),
            ["runtime"] => Ok(render::help::runtime_help(&cmd)),
            ["runtime", action] => render::help::runtime_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown runtime command: {action}")),
            ["doctor", action] => render::help::doctor_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown doctor command: {action}")),
            ["service"] => Ok(render::help::service_help(&cmd)),
            ["service", action] => render::help::service_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown service command: {action}")),
            ["dev", action] => render::help::dev_command_help(&cmd, action)
                .ok_or_else(|| format!("unknown dev command: {action}")),
            [group, ..]
                if matches!(
                    *group,
                    "setup"
                        | "dev"
                        | "start"
                        | "upgrade"
                        | "doctor"
                        | "logs"
                        | "self"
                        | "env"
                        | "migrate"
                        | "adopt"
                        | "release"
                        | "launcher"
                        | "runtime"
                        | "service"
                        | "init"
                ) =>
            {
                Err(format!("unknown help topic: {}", path.join(" ")))
            }
            [group, ..] => Err(format!("unknown command group: {group}")),
        }
    }

    pub(super) fn help_result_for_invocation(
        &self,
        args: &[String],
    ) -> Option<Result<i32, String>> {
        let topic = match args {
            [] => Some(Vec::<&str>::new()),
            [flag] if Self::is_help_flag(flag) => Some(Vec::<&str>::new()),
            [help, rest @ ..] if help == "help" => {
                Some(rest.iter().map(String::as_str).collect::<Vec<_>>())
            }
            [group]
                if matches!(
                    group.as_str(),
                    "self"
                        | "env"
                        | "adopt"
                        | "release"
                        | "launcher"
                        | "runtime"
                        | "service"
                        | "dev"
                        | "logs"
                ) =>
            {
                Some(vec![group.as_str()])
            }
            [group] if group == "doctor" => Some(vec!["doctor"]),
            [group, flag] if group == "migrate" && Self::is_help_flag(flag) => {
                Some(vec!["migrate"])
            }
            [group, flag] if group == "setup" && Self::is_help_flag(flag) => Some(vec!["setup"]),
            [group, flag] if group == "start" && Self::is_help_flag(flag) => Some(vec!["start"]),
            [group, flag] if group == "upgrade" && Self::is_help_flag(flag) => {
                Some(vec!["upgrade"])
            }
            [group, next, rest @ ..] if group == "upgrade" && Self::is_help_token(next) => {
                let mut topic = vec!["upgrade"];
                topic.extend(rest.iter().map(String::as_str));
                Some(topic)
            }
            [group, next] if group == "init" && Self::is_help_token(next) => Some(vec!["init"]),
            [group, next, rest @ ..]
                if matches!(
                    group.as_str(),
                    "self"
                        | "env"
                        | "dev"
                        | "logs"
                        | "migrate"
                        | "adopt"
                        | "release"
                        | "launcher"
                        | "runtime"
                        | "service"
                ) && Self::is_help_token(next) =>
            {
                let mut topic = vec![group.as_str()];
                topic.extend(rest.iter().map(String::as_str));
                Some(topic)
            }
            [group, next, rest @ ..] if group == "doctor" && Self::is_help_token(next) => {
                let mut topic = vec!["doctor"];
                topic.extend(rest.iter().map(String::as_str));
                Some(topic)
            }
            [group, action, flag]
                if matches!(
                    group.as_str(),
                    "self"
                        | "env"
                        | "dev"
                        | "migrate"
                        | "adopt"
                        | "release"
                        | "launcher"
                        | "runtime"
                        | "service"
                        | "logs"
                ) && Self::is_help_flag(flag) =>
            {
                Some(vec![group.as_str(), action.as_str()])
            }
            [group, action, flag] if group == "doctor" && Self::is_help_flag(flag) => {
                Some(vec!["doctor", action.as_str()])
            }
            [group, action, flag]
                if group == "upgrade" && action == "rollback" && Self::is_help_flag(flag) =>
            {
                Some(vec!["upgrade", "rollback"])
            }
            [group, subcommand] if group == "env" && subcommand == "snapshot" => {
                Some(vec!["env", "snapshot"])
            }
            [group, subcommand] if group == "env" && subcommand == "artifact" => {
                Some(vec!["env", "artifact"])
            }
            [group, subcommand, action, flag]
                if group == "env" && subcommand == "artifact" && Self::is_help_flag(flag) =>
            {
                Some(vec!["env", "artifact", action.as_str()])
            }
            [group, subcommand, next, rest @ ..]
                if group == "env" && subcommand == "snapshot" && Self::is_help_token(next) =>
            {
                let mut topic = vec!["env", "snapshot"];
                topic.extend(rest.iter().map(String::as_str));
                Some(topic)
            }
            [group, subcommand, action, flag]
                if group == "env" && subcommand == "snapshot" && Self::is_help_flag(flag) =>
            {
                Some(vec!["env", "snapshot", action.as_str()])
            }
            _ => None,
        }?;

        Some(
            self.render_help_topic(&topic)
                .and_then(|text| self.print_help_text(text)),
        )
    }

    pub(super) fn dispatch_help_command(&self, args: Vec<String>) -> Result<i32, String> {
        let topic = args.iter().map(String::as_str).collect::<Vec<_>>();
        self.render_help_topic(&topic)
            .and_then(|text| self.print_help_text(text))
    }
}
