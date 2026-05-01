// SPDX-License-Identifier: AGPL-3.0-only
//
// Cyberdriver 1.x-compatible CLI facade.
//
// This module intentionally runs before RustDesk's normal global/UI startup so
// read-only and lifecycle commands work on headless Linux machines where no
// X11/Wayland display is available.

use hbb_common::config::{self, LocalConfig};
use serde_json::json;
use std::path::PathBuf;

const NAME_ENV: &str = "CYBERDRIVER_MACHINE_NAME";
const NAME_MAX_LEN: usize = 128;

pub fn handle_early_args() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    apply_transient_name_from_args(&args);

    match parse_command(&args) {
        EarlyCommand::Continue => false,
        EarlyCommand::Handled(status) => {
            std::process::exit(status);
        }
        EarlyCommand::Join(join) => {
            run_join(join);
            true
        }
    }
}

pub fn machine_name_from_env() -> Option<String> {
    sanitize_machine_name(std::env::var(NAME_ENV).ok().as_deref())
}

pub fn sanitize_machine_name(raw: Option<&str>) -> Option<String> {
    let name = raw?.trim();
    if name.is_empty() {
        return None;
    }
    if !name.chars().all(|ch| matches!(ch as u32, 0x20..=0x7e)) {
        return None;
    }
    Some(name.chars().take(NAME_MAX_LEN).collect())
}

fn apply_transient_name_from_args(args: &[String]) {
    if let Some(raw_name) = option_value(args, "--name") {
        match sanitize_machine_name(Some(&raw_name)) {
            Some(name) => std::env::set_var(NAME_ENV, name),
            None => eprintln!(
                "warning: ignoring invalid --name; expected printable ASCII, max {NAME_MAX_LEN} chars"
            ),
        }
    }
}

enum EarlyCommand {
    Continue,
    Handled(i32),
    Join(JoinCommand),
}

struct JoinCommand {
    secret: String,
    api_base: Option<String>,
}

fn parse_command(args: &[String]) -> EarlyCommand {
    if args.is_empty() {
        return EarlyCommand::Continue;
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => {
            print_help();
            EarlyCommand::Handled(0)
        }
        "-v" | "--version" | "version" => {
            println!("cyberdriver {}", crate::VERSION);
            EarlyCommand::Handled(0)
        }
        "join" => parse_join(args),
        "status" | "health" => {
            print_status();
            EarlyCommand::Handled(0)
        }
        "config-print" => {
            print_config();
            EarlyCommand::Handled(0)
        }
        "reset-fingerprint" | "--reset-fingerprint" => {
            reset_fingerprint();
            EarlyCommand::Handled(0)
        }
        "stop" => {
            stop();
            EarlyCommand::Handled(0)
        }
        "logs" => {
            print_logs_hint();
            EarlyCommand::Handled(0)
        }
        _ => EarlyCommand::Continue,
    }
}

fn parse_join(args: &[String]) -> EarlyCommand {
    if has_flag(args, "-h") || has_flag(args, "--help") {
        print_join_help();
        return EarlyCommand::Handled(0);
    }

    let Some(secret) = option_value(args, "--secret") else {
        eprintln!("error: cyberdriver join requires --secret <ak_*>");
        eprintln!("try: cyberdriver join --help");
        return EarlyCommand::Handled(2);
    };

    let api_base = option_value(args, "--api-base")
        .or_else(|| option_value(args, "--host").map(|host| api_base_from_host(&host)));

    EarlyCommand::Join(JoinCommand { secret, api_base })
}

fn run_join(join: JoinCommand) {
    if let Err(message) = ensure_runtime_display_available() {
        eprintln!("{message}");
        std::process::exit(2);
    }

    std::env::set_var("CYBERDESK_AGENT_KEY", join.secret);
    if let Some(api_base) = join.api_base {
        std::env::set_var("CYBERDESK_API_BASE", api_base);
    }

    if !crate::common::global_init() {
        eprintln!("Cyberdriver global initialization failed.");
        std::process::exit(1);
    }
    crate::cyberdesk_branding::init();
    hbb_common::init_log(false, "cyberdriver-join");
    crate::start_server(true, false);
    crate::common::global_clean();
}

fn ensure_runtime_display_available() -> Result<(), &'static str> {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return Err(
                "No DISPLAY or WAYLAND_DISPLAY found. Start Xvfb, Xorg, or a supported Wayland session before running `cyberdriver join`.",
            );
        }
    }
    Ok(())
}

fn reset_fingerprint() {
    crate::cyberdesk_tunnel::reset_fingerprint();
    println!(
        "Cyberdriver fingerprint reset. A new fingerprint will be generated on next tunnel start."
    );
}

fn print_status() {
    let api_key = api_key();
    let fingerprint = crate::cyberdesk_tunnel::current_fingerprint();
    println!("Cyberdriver status");
    println!(
        "  tunnel: {}",
        if api_key.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  api key: {}",
        if api_key.is_some() {
            "configured"
        } else {
            "not configured"
        }
    );
    println!(
        "  fingerprint: {}",
        fingerprint.as_deref().unwrap_or("not generated")
    );
}

fn print_config() {
    let value = json!({
        "api_key_configured": api_key().is_some(),
        "api_base": api_base(),
        "fingerprint": crate::cyberdesk_tunnel::current_fingerprint(),
        "machine_name": machine_name_from_env(),
        "config_path": crate::cyberdesk_tunnel::config_path().display().to_string(),
    });
    match serde_json::to_string_pretty(&value) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{{}}"),
    }
}

fn stop() {
    if stop_service_process() {
        println!("Cyberdriver service stop requested.");
    } else {
        println!(
            "Cyberdriver service was not stopped. It may not be installed or privileges may be insufficient."
        );
    }
}

#[cfg(windows)]
fn stop_service_process() -> bool {
    std::process::Command::new("sc")
        .args(["stop", &crate::get_app_name()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn stop_service_process() -> bool {
    std::process::Command::new("systemctl")
        .args(["stop", &crate::get_app_name().to_lowercase()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn stop_service_process() -> bool {
    std::process::Command::new("launchctl")
        .args(["remove", &format!("{}_server", crate::get_full_name())])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn stop_service_process() -> bool {
    false
}

fn print_logs_hint() {
    println!("Cyberdriver logs are written under the RustDesk/Cyberdriver config log directory.");
    println!(
        "Use the service manager or dashboard diagnostics for live tunnel logs in this build."
    );
}

fn print_help() {
    println!(
        r#"Cyberdriver

Usage:
  cyberdriver join --secret <ak_*> [--name <name>] [--api-base <ws-or-http-base>]
  cyberdriver status
  cyberdriver health
  cyberdriver config-print
  cyberdriver reset-fingerprint
  cyberdriver stop
  cyberdriver logs
  cyberdriver --version
  cyberdriver --help
"#
    );
}

fn print_join_help() {
    println!(
        r#"Cyberdriver join

Usage:
  cyberdriver join --secret <ak_*> [--name <name>] [--api-base <ws-or-http-base>]

Options:
  --secret <ak_*>          Cyberdesk API key.
  --name <name>            Optional machine name sent as X-CYBERDRIVER-NAME.
                           Printable ASCII only, max 128 chars, not persisted.
  --api-base <base>        Tunnel WebSocket base, e.g. ws://localhost:8080.
  --host <host>            Compatibility shorthand for wss://<host>.
  -h, --help               Show this help.
"#
    );
}

fn api_key() -> Option<String> {
    std::env::var("CYBERDESK_AGENT_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let value = LocalConfig::get_option("cyberdesk_api_key");
            if value.trim().is_empty() {
                None
            } else {
                Some(value)
            }
        })
}

fn api_base() -> String {
    std::env::var("CYBERDESK_API_BASE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            let value = LocalConfig::get_option("cyberdesk_api_base");
            if value.trim().is_empty() {
                default_api_base()
            } else {
                value
            }
        })
}

fn default_api_base() -> String {
    let api_server = crate::cyberdesk_branding::API_SERVER;
    if let Some(rest) = api_server.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = api_server.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        api_server.to_string()
    }
}

fn api_base_from_host(host: &str) -> String {
    if host.starts_with("ws://") || host.starts_with("wss://") {
        host.to_string()
    } else {
        format!("wss://{host}")
    }
}

fn option_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name && !pair[1].starts_with('-'))
        .map(|pair| pair[1].clone())
        .or_else(|| {
            let prefix = format!("{name}=");
            args.iter()
                .find_map(|arg| arg.strip_prefix(&prefix).map(ToOwned::to_owned))
        })
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

#[allow(dead_code)]
fn _config_path_for_docs() -> PathBuf {
    config::Config::path("cyberdesk_tunnel.toml")
}

#[cfg(test)]
mod tests {
    use super::{option_value, sanitize_machine_name, NAME_MAX_LEN};

    #[test]
    fn sanitize_machine_name_accepts_trimmed_printable_ascii() {
        assert_eq!(
            sanitize_machine_name(Some("  beacon-vm-12  ")),
            Some("beacon-vm-12".to_string())
        );
    }

    #[test]
    fn sanitize_machine_name_omits_empty_or_invalid_values() {
        assert_eq!(sanitize_machine_name(None), None);
        assert_eq!(sanitize_machine_name(Some("   ")), None);
        assert_eq!(sanitize_machine_name(Some("bad\nname")), None);
        assert_eq!(sanitize_machine_name(Some("münchen")), None);
    }

    #[test]
    fn sanitize_machine_name_truncates_to_legacy_limit() {
        let raw = "a".repeat(NAME_MAX_LEN + 1);
        let sanitized = sanitize_machine_name(Some(&raw));
        assert_eq!(sanitized.as_ref().map(|value| value.len()), Some(NAME_MAX_LEN));
    }

    #[test]
    fn option_value_does_not_treat_next_flag_as_value() {
        let args = vec![
            "join".to_string(),
            "--name".to_string(),
            "--secret".to_string(),
            "ak_test".to_string(),
        ];
        assert_eq!(option_value(&args, "--name"), None);
        assert_eq!(option_value(&args, "--secret"), Some("ak_test".to_string()));
    }
}
