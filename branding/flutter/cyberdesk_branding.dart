// Generated source of truth for Flutter branding strings.
// Edit `branding/flutter/cyberdesk_branding.dart` and run
// `scripts/apply-branding.sh` to refresh `flutter/lib/cyberdesk_branding.dart`.
//
// All values mirror those in `branding/app_strings.json` so the Rust and
// Flutter sides stay aligned. Do not edit `flutter/lib/cyberdesk_branding.dart`
// directly.

class CyberdeskBranding {
  static const String appName = 'Cyberdriver';
  static const String appCompany = 'Cyberdesk Inc.';
  static const String supportUrl = 'https://cyberdesk.io/support';
  static const String agplSourceUrlClient =
      'https://github.com/cyberdesk-hq/cyberdriver-new';
  static const String agplSourceUrlServer =
      'https://github.com/cyberdesk-hq/cyberdriver-server';
  static const String urlScheme = 'cyberdesk';
  static const String apiServerDefault = 'https://api.cyberdesk.io';
  static const String updateFeedUrl = 'https://updates.cyberdesk.io/manifest.json';

  // Display strings shown in UI. Kept here so non-engineering
  // contributors can edit copy without touching Dart logic.
  static const String loginButtonLabel = 'Sign in with Cyberdesk';
  static const String enableServiceLabel = 'Enable Cyberdesk service';
  static const String disableServiceLabel = 'Disable Cyberdesk service';
  static const String tunnelStatusLabel = 'Cyberdesk tunnel';
  static const String apiKeyFieldLabel = 'Cyberdesk API key';
  static const String apiKeyHelpText =
      'Paste an org API key (ak_…) to allow Cyberdesk to control this machine.';

  // Tunnel runtime states (M7 Settings UI).
  static const String tunnelConnected = 'Connected';
  static const String tunnelDisconnected = 'Disconnected';
  static const String tunnelDisabled = 'Disabled (no API key set)';
}
