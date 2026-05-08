import 'package:flutter_hbb/cyberdesk_branding.dart';
import 'package:flutter_hbb/models/platform_model.dart';

const String kCyberdeskEnvironmentKey = 'cyberdesk_environment';
const String kCyberdeskProductionEnvironment = 'production';
const String kCyberdeskDevelopmentEnvironment = 'development';

Future<void> ensureDefaultCyberdeskEnvironment() async {
  if (bind.mainGetLocalOption(key: kCyberdeskEnvironmentKey).isEmpty) {
    await applyCyberdeskEnvironment(kCyberdeskProductionEnvironment);
  }
}

Future<void> applyCyberdeskEnvironment(String value) async {
  if (value == kCyberdeskDevelopmentEnvironment) {
    await bind.mainSetOption(
        key: 'custom-rendezvous-server',
        value: CyberdeskBranding.devRendezvousServer);
    await bind.mainSetOption(
        key: 'relay-server', value: CyberdeskBranding.devRelayServer);
    await bind.mainSetOption(
        key: 'api-server', value: CyberdeskBranding.devApiServer);
    await bind.mainSetOption(
        key: 'key', value: CyberdeskBranding.devHbbsPubkey);
    await bind.mainSetLocalOption(
        key: 'cyberdesk_api_base', value: CyberdeskBranding.devTunnelApiBase);
  } else if (value == kCyberdeskProductionEnvironment) {
    await bind.mainSetOption(
        key: 'custom-rendezvous-server',
        value: CyberdeskBranding.prodRendezvousServer);
    await bind.mainSetOption(
        key: 'relay-server', value: CyberdeskBranding.prodRelayServer);
    await bind.mainSetOption(
        key: 'api-server', value: CyberdeskBranding.prodApiServer);
    await bind.mainSetOption(
        key: 'key', value: CyberdeskBranding.prodHbbsPubkey);
    await bind.mainSetLocalOption(
        key: 'cyberdesk_api_base', value: CyberdeskBranding.prodTunnelApiBase);
  }
  await bind.mainSetLocalOption(key: kCyberdeskEnvironmentKey, value: value);
}
