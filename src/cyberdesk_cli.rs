// SPDX-License-Identifier: AGPL-3.0-only
//
// Cyberdriver 1.x-compatible CLI facade.
//
// This module intentionally runs before RustDesk's normal global/UI startup so
// read-only and lifecycle commands work on headless Linux machines where no
// X11/Wayland display is available.

use serde_json::json;
use std::io::Read as _;

const NAME_ENV: &str = "CYBERDRIVER_MACHINE_NAME";
const NAME_MAX_LEN: usize = 128;
const RUN_JOIN_COMMAND: &str = "__cyberdesk-run-join";
const MSI_CONFIGURE_COMMAND: &str = "__cyberdesk-msi-configure";

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
        EarlyCommand::MsiConfigure(configure) => {
            run_msi_configure(configure);
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
    let name = name
        .chars()
        .take(NAME_MAX_LEN)
        .collect::<String>()
        .trim_end()
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
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
    MsiConfigure(MsiConfigureCommand),
}

struct JoinCommand {
    secret: String,
    environment: Option<crate::cyberdesk_branding::CyberdeskEnvironment>,
    api_base: Option<String>,
    allow_insecure_api_base: bool,
}

struct MsiConfigureCommand {
    api_key: Option<String>,
    api_base: Option<String>,
    allow_insecure_api_base: bool,
    service_config_profile: bool,
    reset_fingerprint: bool,
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
        MSI_CONFIGURE_COMMAND => parse_msi_configure(args),
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

    let allow_insecure_api_base = has_flag(args, "--allow-insecure-api-base")
        || env_truthy("CYBERDESK_ALLOW_INSECURE_API_BASE");
    let environment = match parse_environment(args) {
        Ok(environment) => environment,
        Err(message) => {
            eprintln!("{message}");
            return EarlyCommand::Handled(2);
        }
    };
    let api_base = match option_value(args, "--api-base").or_else(|| option_value(args, "--host")) {
        Some(value) => match validate_api_base(&value, allow_insecure_api_base) {
            Ok(value) => Some(value),
            Err(message) => {
                eprintln!("{message}");
                return EarlyCommand::Handled(2);
            }
        },
        None => None,
    };

    EarlyCommand::Join(JoinCommand {
        secret,
        environment,
        api_base,
        allow_insecure_api_base,
    })
}

fn parse_msi_configure(args: &[String]) -> EarlyCommand {
    if has_flag(args, "--stdin") {
        return match read_msi_config_from_stdin() {
            Ok(configure) => EarlyCommand::MsiConfigure(configure),
            Err(message) => {
                eprintln!("{message}");
                EarlyCommand::Handled(2)
            }
        };
    }

    let api_key = option_value(args, "--api-key").filter(|value| !value.trim().is_empty());
    let allow_insecure_api_base = has_flag(args, "--allow-insecure-api-base")
        || env_truthy("CYBERDESK_ALLOW_INSECURE_API_BASE");
    let api_base = match option_value(args, "--api-base") {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => match validate_api_base(&value, allow_insecure_api_base) {
            Ok(value) => Some(value),
            Err(message) => {
                eprintln!("{message}");
                return EarlyCommand::Handled(2);
            }
        },
        None => None,
    };
    let reset_fingerprint = has_flag(args, "--reset-fingerprint");

    EarlyCommand::MsiConfigure(MsiConfigureCommand {
        api_key,
        api_base,
        allow_insecure_api_base,
        service_config_profile: has_flag(args, "--service-config-profile"),
        reset_fingerprint,
    })
}

fn read_msi_config_from_stdin() -> Result<MsiConfigureCommand, String> {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .map_err(|err| format!("failed to read MSI configuration from stdin: {err}"))?;

    let mut lines = raw.lines();
    let api_key = lines
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let api_base_raw = lines.next().map(str::trim).filter(|value| !value.is_empty());
    let allow_insecure_api_base = lines
        .next()
        .map(parse_truthy)
        .unwrap_or_else(|| env_truthy("CYBERDESK_ALLOW_INSECURE_API_BASE"));
    let api_base = match api_base_raw {
        Some(value) => Some(validate_api_base(value, allow_insecure_api_base)?),
        None => None,
    };

    Ok(MsiConfigureCommand {
        api_key,
        api_base,
        allow_insecure_api_base,
        service_config_profile: has_flag(
            &std::env::args().collect::<Vec<_>>(),
            "--service-config-profile",
        ),
        reset_fingerprint: false,
    })
}

fn run_join(join: JoinCommand) {
    let _ = join.allow_insecure_api_base;
    if let Err(message) = ensure_runtime_display_available() {
        eprintln!("{message}");
        std::process::exit(2);
    }

    if let Err(message) = crate::cyberdesk_tunnel::store_configured_api_key(join.secret) {
        eprintln!("error: {message}");
        std::process::exit(2);
    }
    if let Some(environment) = join.environment {
        crate::cyberdesk_branding::apply_environment(environment);
    }
    if let Some(api_base) = join.api_base {
        if let Some(api_server) = api_server_from_tunnel_base(&api_base) {
            hbb_common::config::Config::set_option("api-server".to_string(), api_server);
            hbb_common::config::LocalConfig::set_option(
                crate::cyberdesk_branding::ENVIRONMENT_OPTION.to_string(),
                "custom".to_string(),
            );
        }
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

fn run_msi_configure(configure: MsiConfigureCommand) {
    let _ = configure.allow_insecure_api_base;
    if configure.service_config_profile {
        use_windows_service_config_profile();
    }
    if let Some(api_key) = configure.api_key {
        if !api_key.trim().is_empty() {
            if let Err(message) = crate::cyberdesk_tunnel::store_configured_api_key(api_key) {
                eprintln!("error: {message}");
                std::process::exit(2);
            }
        }
    }
    if let Some(api_base) = configure.api_base {
        crate::cyberdesk_tunnel::store_configured_api_base(api_base);
    }
    if configure.reset_fingerprint {
        if let Err(err) = crate::cyberdesk_tunnel::reset_fingerprint() {
            eprintln!("failed to reset Cyberdriver fingerprint: {err}");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
fn use_windows_service_config_profile() {
    let profile = r"C:\Windows\ServiceProfiles\LocalService";
    std::env::set_var("USERPROFILE", profile);
    std::env::set_var("APPDATA", format!(r"{profile}\AppData\Roaming"));
    std::env::set_var("LOCALAPPDATA", format!(r"{profile}\AppData\Local"));
    let _ = std::fs::create_dir_all(format!(r"{profile}\AppData\Roaming"));
    let _ = std::fs::create_dir_all(format!(r"{profile}\AppData\Local"));
}

#[cfg(not(windows))]
fn use_windows_service_config_profile() {}

fn spawn_join_runtime() -> std::io::Result<std::process::Child> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg(RUN_JOIN_COMMAND);
    if let Some(name) = machine_name_from_env() {
        command.env(NAME_ENV, name);
    } else {
        command.env_remove(NAME_ENV);
    }
    command.env_remove("CYBERDESK_AGENT_KEY");
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
    match crate::cyberdesk_tunnel::reset_fingerprint() {
        Ok(_) => println!(
            "Cyberdriver fingerprint reset. A new fingerprint will be generated on next tunnel start."
        ),
        Err(err) => {
            eprintln!("failed to reset Cyberdriver fingerprint: {err}");
            std::process::exit(1);
        }
    }
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
        "cyberdesk_environment": hbb_common::config::LocalConfig::get_option(
            crate::cyberdesk_branding::ENVIRONMENT_OPTION
        ),
        "desktop_api_server": hbb_common::config::Config::get_option("api-server"),
        "rendezvous_server": hbb_common::config::Config::get_option("custom-rendezvous-server"),
        "relay_server": hbb_common::config::Config::get_option("relay-server"),
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
  cyberdriver join --secret <ak_*> [--name <name>] [--api-base <ws-or-http-base>] [--allow-insecure-api-base]
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
  cyberdriver join --secret <ak_*> [--name <name>] [--env prod|dev] [--api-base <ws-or-http-base>] [--allow-insecure-api-base]

Options:
  --secret <ak_*>          Cyberdesk API key.
  --name <name>            Optional machine name sent as X-CYBERDRIVER-NAME.
                           Printable ASCII only, max 128 chars, not persisted.
  --env <prod|dev>         Apply Cyberdesk environment preset for API, hbbs, hbbr, and key.
  --dev                    Shorthand for --env dev.
  --api-base <base>        Tunnel WebSocket base, e.g. ws://localhost:8080.
                           Also updates the desktop API server for GUI login.
  --host <host>            Compatibility shorthand for wss://<host>.
  --allow-insecure-api-base
                           Allow ws:// or http:// for non-loopback dev targets
                           such as Tailscale. Never use for production.
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

fn validate_api_base(raw: &str, allow_insecure_api_base: bool) -> Result<String, String> {
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
        if is_loopback_api_base(value) || allow_insecure_api_base {
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
            "error: insecure --api-base is only allowed for localhost/loopback dev targets; use --allow-insecure-api-base or CYBERDESK_ALLOW_INSECURE_API_BASE=1 for explicit local dev testing"
                .to_string(),
        );
    }

    if value.contains("://") {
        return Err("error: unsupported --api-base URL scheme".to_string());
    }

    Ok(api_base_from_host(value.trim_end_matches('/')))
}

fn parse_environment(
    args: &[String],
) -> Result<Option<crate::cyberdesk_branding::CyberdeskEnvironment>, String> {
    let from_dev_flag = has_flag(args, "--dev")
        .then_some(crate::cyberdesk_branding::CyberdeskEnvironment::Development);
    let from_env = match option_value(args, "--env") {
        Some(value) => match crate::cyberdesk_branding::CyberdeskEnvironment::parse(&value) {
            Some(environment) => Some(environment),
            None => return Err("error: --env must be one of: prod, dev".to_string()),
        },
        None => None,
    };
    match (from_dev_flag, from_env) {
        (Some(dev), Some(env)) if dev != env => {
            Err("error: --dev conflicts with --env prod".to_string())
        }
        (Some(dev), _) => Ok(Some(dev)),
        (None, environment) => Ok(environment),
    }
}

fn api_server_from_tunnel_base(api_base: &str) -> Option<String> {
    api_base
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| api_base.strip_prefix("ws://").map(|rest| format!("http://{rest}")))
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
        .find(|pair| pair[0] == name && !is_known_option(&pair[1]))
        .map(|pair| pair[1].clone())
        .or_else(|| {
            let prefix = format!("{name}=");
            args.iter()
                .find_map(|arg| arg.strip_prefix(&prefix).map(ToOwned::to_owned))
        })
}

fn is_known_option(value: &str) -> bool {
    matches!(
        value,
        "--secret"
            | "--name"
            | "--env"
            | "--dev"
            | "--api-base"
            | "--host"
            | "--api-key"
            | "--stdin"
            | "--reset-fingerprint"
            | "--allow-insecure-api-base"
            | "--service-config-profile"
            | "-h"
            | "--help"
    )
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|value| parse_truthy(&value))
        .unwrap_or(false)
}

fn parse_truthy(value: &str) -> bool {
    matches!(
        value.trim(),
        "1" | "Y" | "y" | "true" | "TRUE" | "True" | "yes" | "YES" | "Yes"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        api_server_from_tunnel_base, option_value, parse_environment, sanitize_machine_name,
        validate_api_base, NAME_MAX_LEN,
    };
    use crate::cyberdesk_branding::CyberdeskEnvironment;

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
    fn sanitize_machine_name_trims_after_truncation() {
        let raw = format!("{} b", "a".repeat(NAME_MAX_LEN - 1));
        let sanitized = sanitize_machine_name(Some(&raw));
        assert_eq!(sanitized, Some("a".repeat(NAME_MAX_LEN - 1)));
        assert!(!sanitized.unwrap_or_default().ends_with(' '));
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
    fn option_value_does_not_treat_msi_flags_as_values() {
        let args = vec![
            "__cyberdesk-msi-configure".to_string(),
            "--api-base".to_string(),
            "--api-key".to_string(),
            "ak_test".to_string(),
            "--reset-fingerprint".to_string(),
        ];
        assert_eq!(option_value(&args, "--api-base"), None);
        assert_eq!(option_value(&args, "--api-key"), Some("ak_test".to_string()));
    }

    #[test]
    fn parse_environment_accepts_dev_and_prod_aliases() {
        assert_eq!(
            parse_environment(&["join".to_string(), "--env".to_string(), "dev".to_string()]),
            Ok(Some(CyberdeskEnvironment::Development))
        );
        assert_eq!(
            parse_environment(&[
                "join".to_string(),
                "--env=production".to_string(),
            ]),
            Ok(Some(CyberdeskEnvironment::Production))
        );
        assert_eq!(
            parse_environment(&["join".to_string(), "--dev".to_string()]),
            Ok(Some(CyberdeskEnvironment::Development))
        );
    }

    #[test]
    fn parse_environment_rejects_unknown_or_conflicting_values() {
        assert!(parse_environment(&[
            "join".to_string(),
            "--env".to_string(),
            "staging".to_string(),
        ])
        .is_err());
        assert!(parse_environment(&[
            "join".to_string(),
            "--dev".to_string(),
            "--env".to_string(),
            "prod".to_string(),
        ])
        .is_err());
    }

    #[test]
    fn api_server_from_tunnel_base_maps_websocket_scheme() {
        assert_eq!(
            api_server_from_tunnel_base("wss://cyberdesk-api-dev.fly.dev"),
            Some("https://cyberdesk-api-dev.fly.dev".to_string())
        );
        assert_eq!(
            api_server_from_tunnel_base("ws://localhost:8080"),
            Some("http://localhost:8080".to_string())
        );
    }

    #[test]
    fn validate_api_base_omits_empty_msi_stdin_values() {
        assert!(validate_api_base("", false).is_err());
        assert!(validate_api_base("   ", false).is_err());
    }

    #[test]
    fn option_value_accepts_single_dash_values() {
        let args = vec![
            "join".to_string(),
            "--secret".to_string(),
            "-ak_test".to_string(),
        ];
        assert_eq!(option_value(&args, "--secret"), Some("-ak_test".to_string()));
    }

    #[test]
    fn validate_api_base_rejects_insecure_non_loopback() {
        assert!(validate_api_base("", false).is_err());
        assert!(validate_api_base("   ", false).is_err());
        assert!(validate_api_base("ws://api.cyberdesk.io", false).is_err());
        assert!(validate_api_base("http://10.0.0.10:8080", false).is_err());
        assert!(validate_api_base("HTTP://10.0.0.10:8080", false).is_err());
        assert!(validate_api_base("http://evil.com#@localhost:8080", false).is_err());
        assert!(validate_api_base("ftp://api.cyberdesk.io", false).is_err());
    }

    #[test]
    fn validate_api_base_allows_secure_and_local_dev_targets() {
        assert_eq!(
            validate_api_base("api.cyberdesk.io", false),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("https://api.cyberdesk.io", false),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("HTTPS://api.cyberdesk.io", false),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("ws://localhost:8080", false),
            Ok("ws://localhost:8080".to_string())
        );
        assert_eq!(
            validate_api_base("WS://localhost:8080", false),
            Ok("ws://localhost:8080".to_string())
        );
        assert_eq!(
            validate_api_base("http://127.0.0.1:8080", false),
            Ok("ws://127.0.0.1:8080".to_string())
        );
        assert_eq!(
            validate_api_base("ws://[::1]:8080", false),
            Ok("ws://[::1]:8080".to_string())
        );
        assert_eq!(
            validate_api_base("http://[::1]", false),
            Ok("ws://[::1]".to_string())
        );
        assert_eq!(
            validate_api_base("api.cyberdesk.io/", false),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("https://api.cyberdesk.io/", false),
            Ok("wss://api.cyberdesk.io".to_string())
        );
        assert_eq!(
            validate_api_base("ws://localhost:8080/", false),
            Ok("ws://localhost:8080".to_string())
        );
    }

    #[test]
    fn validate_api_base_allows_explicit_insecure_dev_targets() {
        assert_eq!(
            validate_api_base("ws://100.66.79.97:8080", true),
            Ok("ws://100.66.79.97:8080".to_string())
        );
        assert_eq!(
            validate_api_base("http://10.0.0.10:8080", true),
            Ok("ws://10.0.0.10:8080".to_string())
        );
    }
}
