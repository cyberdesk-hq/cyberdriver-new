// SPDX-License-Identifier: AGPL-3.0-only
//
// Cyberdriver 1.x-compatible CLI facade.
//
// This module intentionally runs before RustDesk's normal global/UI startup so
// read-only and lifecycle commands work on headless Linux machines where no
// X11/Wayland display is available.

use serde_json::json;

const NAME_ENV: &str = "CYBERDRIVER_MACHINE_NAME";
const NAME_MAX_LEN: usize = 128;
const RUN_JOIN_COMMAND: &str = "__cyberdesk-run-join";

pub fn handle_early_args() -> bool {
    crate::cyberdesk_branding::init();
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
        EarlyCommand::RunJoin => {
            run_join_runtime();
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
    RunJoin,
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
        RUN_JOIN_COMMAND => EarlyCommand::RunJoin,
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

    let api_base = match option_value(args, "--api-base").or_else(|| option_value(args, "--host"))
    {
        Some(value) => match validate_api_base(&value) {
            Ok(value) => Some(value),
            Err(message) => {
                eprintln!("{message}");
                return EarlyCommand::Handled(2);
            }
        },
        None => None,
    };

    EarlyCommand::Join(JoinCommand { secret, api_base })
}

fn run_join(join: JoinCommand) {
    if let Err(message) = ensure_runtime_display_available() {
        eprintln!("{message}");
        std::process::exit(2);
    }

    if let Err(message) = crate::cyberdesk_tunnel::store_configured_api_key(join.secret) {
        eprintln!("error: {message}");
        std::process::exit(2);
    }
    if let Some(api_base) = join.api_base {
        crate::cyberdesk_tunnel::store_configured_api_base(api_base);
    }

    match spawn_join_runtime() {
        Ok(_) => {
            println!("Cyberdriver is starting in the background.");
            println!("Use `cyberdriver status` or the Cyberdesk dashboard to verify connection.");
        }
        Err(err) => {
            eprintln!("failed to start Cyberdriver runtime: {err}");
            std::process::exit(1);
        }
    }
}

fn spawn_join_runtime() -> std::io::Result<std::process::Child> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg(RUN_JOIN_COMMAND);
    if let Some(name) = machine_name_from_env() {
        command.env(NAME_ENV, name);
    } else {
        command.env_remove(NAME_ENV);
    }
    command.spawn()
}

fn run_join_runtime() {
    if let Err(message) = ensure_runtime_display_available() {
        eprintln!("{message}");
        std::process::exit(2);
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
    let api_key = crate::cyberdesk_tunnel::configured_api_key();
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
        "api_key_configured": crate::cyberdesk_tunnel::configured_api_key().is_some(),
        "api_base": crate::cyberdesk_tunnel::configured_api_base(),
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

Security:
  `--secret` is accepted for Cyberdriver 1.x compatibility. The bootstrap process stores it
  in local Cyberdriver config, starts the runtime without the secret in argv/env, then exits.
"#
    );
}

fn api_base_from_host(host: &str) -> String {
    format!("wss://{host}")
}

fn validate_api_base(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("error: --api-base must not be empty".to_string());
    }

    let lower_value = value.to_ascii_lowercase();
    if lower_value.starts_with("wss://") {
        return Ok(format!(
            "wss://{}",
            value["wss://".len()..].trim_end_matches('/')
        ));
    }
    if lower_value.starts_with("https://") {
        return Ok(format!(
            "wss://{}",
            value["https://".len()..].trim_end_matches('/')
        ));
    }

    if lower_value.starts_with("ws://") || lower_value.starts_with("http://") {
        if is_loopback_api_base(value) {
            if lower_value.starts_with("http://") {
                return Ok(format!(
                    "ws://{}",
                    value["http://".len()..].trim_end_matches('/')
                ));
            }
            return Ok(format!(
                "ws://{}",
                value["ws://".len()..].trim_end_matches('/')
            ));
        }
        return Err(
            "error: insecure --api-base is only allowed for localhost/loopback dev targets"
                .to_string(),
        );
    }

    if value.contains("://") {
        return Err("error: unsupported --api-base URL scheme".to_string());
    }

    Ok(api_base_from_host(value.trim_end_matches('/')))
}

fn is_loopback_api_base(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    matches!(
        url.host_str().map(|host| host.trim_matches(['[', ']'])),
        Some("localhost" | "127.0.0.1" | "::1")
    )
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

#[cfg(test)]
mod tests {
    use super::{option_value, sanitize_machine_name, validate_api_base, NAME_MAX_LEN};

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

    #[test]
    fn validate_api_base_rejects_insecure_non_loopback() {
        assert!(validate_api_base("").is_err());
        assert!(validate_api_base("   ").is_err());
        assert!(validate_api_base("ws://api.cyberdesk.io").is_err());
        assert!(validate_api_base("http://10.0.0.10:8080").is_err());
        assert!(validate_api_base("HTTP://10.0.0.10:8080").is_err());
        assert!(validate_api_base("http://evil.com#@localhost:8080").is_err());
        assert!(validate_api_base("ftp://api.cyberdesk.io").is_err());
    }

    #[test]
    fn validate_api_base_allows_secure_and_local_dev_targets() {
        assert_eq!(
            validate_api_base("api.cyberdesk.io"),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("https://api.cyberdesk.io"),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("HTTPS://api.cyberdesk.io"),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("ws://localhost:8080"),
            Ok("ws://localhost:8080".to_string())
        );
        assert_eq!(
            validate_api_base("WS://localhost:8080"),
            Ok("ws://localhost:8080".to_string())
        );
        assert_eq!(
            validate_api_base("http://127.0.0.1:8080"),
            Ok("ws://127.0.0.1:8080".to_string())
        );
        assert_eq!(
            validate_api_base("ws://[::1]:8080"),
            Ok("ws://[::1]:8080".to_string())
        );
        assert_eq!(
            validate_api_base("http://[::1]"),
            Ok("ws://[::1]".to_string())
        );
        assert_eq!(
            validate_api_base("api.cyberdesk.io/"),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("https://api.cyberdesk.io/"),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("ws://localhost:8080/"),
            Ok("ws://localhost:8080".to_string())
        );
    }
}
