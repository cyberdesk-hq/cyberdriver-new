// Generated source of truth for Flutter branding strings.
// Edit `branding/flutter/cyberdesk_branding.dart` and run
// `scripts/apply-branding.sh` to refresh `flutter/lib/cyberdesk_branding.dart`.
//
// All values mirror those in `branding/app_strings.json` so the Rust and
// Flutter sides stay aligned. Do not edit `flutter/lib/cyberdesk_branding.dart`
// directly.

class CyberdeskBranding {
  static const String appName = 'Cyberdriver';
  static const String appCompany = 'Cyberdesk, Inc';
  static const String supportUrl = 'https://cyberdesk.io/support';
  static const String websiteUrl = 'https://cyberdesk.io';
  static const String privacyPolicyUrl = 'https://www.cyberdesk.io/privacy';
  static const String agplSourceUrlClient =
      'https://github.com/cyberdesk-hq/cyberdriver-new';
  static const String agplSourceUrlServer =
      'https://github.com/cyberdesk-hq/cyberdriver-server';
  static const String urlScheme = 'cyberdriver';
  static const String prodRendezvousServer = 'hbbs.cyberdesk.io';
  static const String prodRelayServer = 'hbbr.cyberdesk.io';
  static const String prodApiServer = 'https://api.cyberdesk.io';
  static const String prodTunnelApiBase = 'wss://api.cyberdesk.io';
  static const String prodHbbsPubkey =
      'zhJ/30tgM6fCP+cJro8DjPN2WnswhMiowPkehilsMYc=';
  static const String devRendezvousServer = 'hbbs-dev.cyberdesk.io';
  static const String devRelayServer = 'hbbr-dev.cyberdesk.io';
  static const String devApiServer = 'https://cyberdesk-api-dev.fly.dev';
  static const String devTunnelApiBase = 'wss://cyberdesk-api-dev.fly.dev';
  static const String devHbbsPubkey =
      'EHHHwBfzjJasItIOwAJAI60Jj64uJu4rpI1cdE4ulhI=';
  static const String apiServerDefault = prodApiServer;
  static const String updateFeedUrl =
      'https://updates.cyberdesk.io/manifest.json';

  // Display strings shown in UI. Kept here so non-engineering
  // contributors can edit copy without touching Dart logic.
  static const String loginButtonLabel = 'Sign in for peer access';
  static const String peerAccessEmptyTitle =
      'Log in for peer access to other desktops in your organization';
  static const String peerAccessEmptySubtitle =
      'Dashboard streaming and remote control work with your API key. '
      'Desktop sign-in is only needed when you want this app to browse '
      'and connect to peers.';
  static const String aboutTagline =
      'The easiest way to automate computers with AI.';
  static const String enableServiceLabel = 'Enable Cyberdesk service';
  static const String disableServiceLabel = 'Disable Cyberdesk service';
  static const String streamingServiceStatusLabel =
      'Cyberdriver Streaming Service';
  static const String tunnelStatusLabel = 'Cyberdesk tunnel';
  static const String apiKeyFieldLabel = 'Cyberdesk API key';
  static const String apiKeyHelpText =
      'Required for dashboard streaming and remote control. Desktop sign-in is optional and only used for desktop-to-desktop peer access.';

  // Tunnel runtime states (M7 Settings UI).
  static const String tunnelConnected = 'Connected';
  static const String tunnelDisconnected = 'Disconnected';
  static const String tunnelDisabled = 'Disabled (no API key set)';
}
