use std::path::PathBuf;
use std::process::ExitCode;
use std::{io::Write, path::Path};

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "vigil")]
#[command(about = "Vigil, a Wayland-native desktop cockpit")]
struct Cli {
    #[arg(long, value_name = "PATH", default_value_os_t = default_config_path())]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    Timer {
        #[command(subcommand)]
        command: TimerCommand,
    },
    Activity {
        #[command(subcommand)]
        command: ActivityCommand,
    },
    #[command(name = "__test_control_server", hide = true)]
    TestControlServer {
        #[arg(long, default_value_t = 1)]
        requests: usize,
    },
    #[command(name = "__preview", hide = true)]
    Preview {
        #[arg(
            long,
            default_value = "idle",
            value_parser = ["idle", "escalation", "suggestion", "routine", "control-center", "reduced-motion"]
        )]
        scenario: String,
    },
}

#[derive(Debug, clap::Subcommand)]
enum TimerCommand {
    Start {
        duration: String,
        #[arg(long)]
        label: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Cancel {
        id: String,
    },
    List,
}

#[derive(Debug, clap::Subcommand)]
enum ActivityCommand {
    ShellRules,
    Start {
        #[arg(long)]
        id: String,
        #[arg(long)]
        label: String,
        #[arg(long)]
        cwd: PathBuf,
    },
    Finish {
        #[arg(long)]
        id: String,
        #[arg(long)]
        exit_code: i32,
    },
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vigil")
        .join("config.toml")
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();
    match run_cli(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli(cli: Cli) -> Result<()> {
    match cli.command {
        None => vigil::run(&cli.config),
        Some(Command::Timer { command }) => {
            let request = timer_request(command)?;
            let response = vigil::ControlClient::new()?.send(&request)?;
            handle_control_response(response)
        }
        Some(Command::Activity { command }) => handle_activity_command(command, &cli.config),
        Some(Command::TestControlServer { requests }) => vigil::run_test_control_server(requests),
        Some(Command::Preview { scenario }) => vigil::run_preview(&cli.config, &scenario),
    }
}

fn timer_request(command: TimerCommand) -> Result<vigil::ControlRequest> {
    Ok(match command {
        TimerCommand::Start { duration, label } => vigil::ControlRequest::TimerStart {
            label,
            duration_seconds: parse_duration_seconds(&duration)?,
        },
        TimerCommand::Pause { id } => vigil::ControlRequest::TimerPause { id },
        TimerCommand::Resume { id } => vigil::ControlRequest::TimerResume { id },
        TimerCommand::Cancel { id } => vigil::ControlRequest::TimerCancel { id },
        TimerCommand::List => vigil::ControlRequest::TimerList,
    })
}

fn activity_request(command: ActivityCommand, now_epoch: i64) -> Result<vigil::ControlRequest> {
    Ok(match command {
        ActivityCommand::ShellRules => bail!("shell-rules does not produce a control request"),
        ActivityCommand::Start { id, label, cwd } => vigil::ControlRequest::ActivityStart {
            id,
            label,
            cwd,
            started_at: now_epoch,
        },
        ActivityCommand::Finish { id, exit_code } => vigil::ControlRequest::ActivityFinish {
            id,
            exit_code,
            finished_at: now_epoch,
        },
    })
}

fn handle_activity_command(command: ActivityCommand, config_path: &Path) -> Result<()> {
    match command {
        ActivityCommand::ShellRules => print_activity_shell_rules(config_path),
        other => {
            let request = activity_request(other, current_epoch())?;
            let response = vigil::ControlClient::new()?.send(&request)?;
            handle_control_response(response)
        }
    }
}

fn print_activity_shell_rules(config_path: &Path) -> Result<()> {
    let config = vigil::AppConfig::load(config_path)?;
    let payload = encode_activity_shell_rules(&config.command_activity.allowlist)?;
    std::io::stdout().write_all(&payload)?;
    Ok(())
}

fn encode_activity_shell_rules(allowlist: &[vigil::CommandRule]) -> Result<Vec<u8>> {
    let mut payload = Vec::new();

    for rule in allowlist {
        validate_activity_shell_rule_field("label", &rule.label)?;
        for prefix in &rule.prefixes {
            validate_activity_shell_rule_field("prefix", prefix)?;
            payload.extend_from_slice(rule.label.as_bytes());
            payload.push(0);
            payload.extend_from_slice(prefix.as_bytes());
            payload.push(0);
        }
    }

    Ok(payload)
}

fn validate_activity_shell_rule_field(field: &str, value: &str) -> Result<()> {
    if value.contains('\0') {
        bail!("activity shell rule {field} must not contain NUL bytes");
    }

    Ok(())
}

fn handle_control_response(response: vigil::ControlResponse) -> Result<()> {
    match response {
        vigil::ControlResponse::Error { message } => Err(anyhow!(message)),
        other => {
            println!("{}", serde_json::to_string(&other)?);
            Ok(())
        }
    }
}

fn current_epoch() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time");
    i64::try_from(now.as_secs()).unwrap_or(i64::MAX)
}

fn parse_duration_seconds(text: &str) -> Result<u64> {
    if text.is_empty() {
        bail!("duration must not be empty");
    }

    let (number, multiplier) = match text.chars().last().expect("non-empty duration") {
        's' => (&text[..text.len() - 1], 1_u64),
        'm' => (&text[..text.len() - 1], 60_u64),
        'h' => (&text[..text.len() - 1], 60_u64 * 60),
        'd' => (&text[..text.len() - 1], 60_u64 * 60 * 24),
        _ => (text, 1_u64),
    };

    if number.is_empty() {
        bail!("duration is missing its numeric value");
    }

    let value = number
        .parse::<u64>()
        .map_err(|_| anyhow!("invalid duration: {text}"))?;
    if value == 0 {
        bail!("duration must be greater than zero");
    }

    value
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("duration is too large: {text}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ActivityCommand, activity_request, encode_activity_shell_rules};

    #[test]
    fn activity_start_request_uses_label_cwd_and_timestamp() {
        let request = activity_request(
            ActivityCommand::Start {
                id: "shell-1".to_string(),
                label: "Cargo test".to_string(),
                cwd: PathBuf::from("/tmp/project"),
            },
            1_800_000_000,
        )
        .unwrap();

        assert_eq!(
            request,
            vigil::ControlRequest::ActivityStart {
                id: "shell-1".to_string(),
                label: "Cargo test".to_string(),
                cwd: PathBuf::from("/tmp/project"),
                started_at: 1_800_000_000,
            }
        );
    }

    #[test]
    fn activity_finish_request_uses_exit_code_and_timestamp() {
        let request = activity_request(
            ActivityCommand::Finish {
                id: "shell-1".to_string(),
                exit_code: 23,
            },
            1_800_000_001,
        )
        .unwrap();

        assert_eq!(
            request,
            vigil::ControlRequest::ActivityFinish {
                id: "shell-1".to_string(),
                exit_code: 23,
                finished_at: 1_800_000_001,
            }
        );
    }

    #[test]
    fn activity_shell_rules_encode_non_default_configured_allowlist() {
        let config = vigil::AppConfig::from_toml(
            r#"
            [[command_activity.allowlist]]
            label = "Cargo nextest"
            prefixes = ["cargo nextest"]

            [[command_activity.allowlist]]
            label = "Pytest"
            prefixes = ["pytest"]
            "#,
        )
        .unwrap();

        let payload = encode_activity_shell_rules(&config.command_activity.allowlist).unwrap();

        assert_eq!(
            payload,
            b"Cargo nextest\0cargo nextest\0Pytest\0pytest\0".to_vec()
        );
    }
}
