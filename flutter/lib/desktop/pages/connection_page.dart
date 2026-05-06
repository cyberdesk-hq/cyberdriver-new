// main window right pane

import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/cyberdesk_branding.dart';
import 'package:flutter_hbb/models/ab_model.dart';
import 'package:flutter_hbb/models/state_model.dart';
import 'package:get/get.dart';
import 'package:provider/provider.dart';
import 'package:url_launcher/url_launcher_string.dart';
import 'package:flutter_hbb/models/peer_model.dart';

import '../../common.dart';
import '../../common/widgets/peer_card.dart';
import '../../models/platform_model.dart';

class OnlineStatusWidget extends StatefulWidget {
  const OnlineStatusWidget({Key? key, this.onSvcStatusChanged})
      : super(key: key);

  final VoidCallback? onSvcStatusChanged;

  @override
  State<OnlineStatusWidget> createState() => _OnlineStatusWidgetState();
}

/// State for the connection page.
class _OnlineStatusWidgetState extends State<OnlineStatusWidget> {
  final _svcStopped = Get.find<RxBool>(tag: 'stop-service');
  final _svcIsUsingPublicServer = true.obs;
  Timer? _updateTimer;

  double get em => 14.0;
  double? get height => bind.isIncomingOnly() ? null : em * 3;

  void onUsePublicServerGuide() {
    const url = "https://rustdesk.com/pricing";
    canLaunchUrlString(url).then((can) {
      if (can) {
        launchUrlString(url);
      }
    });
  }

  @override
  void initState() {
    super.initState();
    _updateTimer = periodic_immediate(Duration(seconds: 1), () async {
      updateStatus();
    });
  }

  @override
  void dispose() {
    _updateTimer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final isIncomingOnly = bind.isIncomingOnly();
    startServiceWidget() => Offstage(
          offstage: !_svcStopped.value,
          child: InkWell(
                  onTap: () async {
                    await start_service(true);
                  },
                  child: Text(translate(CyberdeskBranding.enableServiceLabel),
                      style: TextStyle(
                          decoration: TextDecoration.underline, fontSize: em)))
              .marginOnly(left: em),
        );

    setupServerWidget() => Flexible(
          child: Offstage(
            offstage: !(!_svcStopped.value &&
                stateGlobal.svcStatus.value == SvcStatus.ready &&
                _svcIsUsingPublicServer.value),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.center,
              children: [
                Text(', ', style: TextStyle(fontSize: em)),
                Flexible(
                  child: InkWell(
                    onTap: onUsePublicServerGuide,
                    child: Row(
                      children: [
                        Flexible(
                          child: Text(
                            translate('setup_server_tip'),
                            style: TextStyle(
                                decoration: TextDecoration.underline,
                                fontSize: em),
                          ),
                        ),
                      ],
                    ),
                  ),
                )
              ],
            ),
          ),
        );

    basicWidget() => Row(
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            Container(
              height: 8,
              width: 8,
              decoration: BoxDecoration(
                borderRadius: BorderRadius.circular(4),
                color: _svcStopped.value ||
                        stateGlobal.svcStatus.value == SvcStatus.connecting
                    ? kColorWarn
                    : (stateGlobal.svcStatus.value == SvcStatus.ready
                        ? Color.fromARGB(255, 50, 190, 166)
                        : Color.fromARGB(255, 224, 79, 95)),
              ),
            ).marginSymmetric(horizontal: em),
            Container(
              width: isIncomingOnly ? 226 : null,
              child: _buildConnStatusMsg(),
            ),
            // stop
            if (!isIncomingOnly) startServiceWidget(),
            // ready && public
            // No need to show the guide if is custom client.
            if (!isIncomingOnly) setupServerWidget(),
          ],
        );

    return Container(
      height: height,
      child: Obx(() => isIncomingOnly
          ? Column(
              children: [
                basicWidget(),
                Align(
                        child: startServiceWidget(),
                        alignment: Alignment.centerLeft)
                    .marginOnly(top: 2.0, left: 22.0),
              ],
            )
          : basicWidget()),
    ).paddingOnly(right: isIncomingOnly ? 8 : 0);
  }

  _buildConnStatusMsg() {
    widget.onSvcStatusChanged?.call();
    return Text(
      _svcStopped.value
          ? translate("Service is not running")
          : stateGlobal.svcStatus.value == SvcStatus.connecting
              ? translate("connecting_status")
              : stateGlobal.svcStatus.value == SvcStatus.notReady
                  ? translate("not_ready_status")
                  : translate('Ready'),
      style: TextStyle(fontSize: em),
    );
  }

  updateStatus() async {
    final status =
        jsonDecode(await bind.mainGetConnectStatus()) as Map<String, dynamic>;
    final statusNum = status['status_num'] as int;
    if (statusNum == 0) {
      stateGlobal.svcStatus.value = SvcStatus.connecting;
    } else if (statusNum == -1) {
      stateGlobal.svcStatus.value = SvcStatus.notReady;
    } else if (statusNum == 1) {
      stateGlobal.svcStatus.value = SvcStatus.ready;
    } else {
      stateGlobal.svcStatus.value = SvcStatus.notReady;
    }
    _svcIsUsingPublicServer.value = await bind.mainIsUsingPublicServer();
    try {
      stateGlobal.videoConnCount.value = status['video_conn_count'] as int;
    } catch (_) {}
  }
}

/// Connection page for connecting to a remote peer.
class ConnectionPage extends StatefulWidget {
  const ConnectionPage({Key? key}) : super(key: key);

  @override
  State<ConnectionPage> createState() => _ConnectionPageState();
}

/// State for the connection page.
class _ConnectionPageState extends State<ConnectionPage> {
  @override
  Widget build(BuildContext context) {
    final isOutgoingOnly = bind.isOutgoingOnly();
    return Column(
      children: [
        Expanded(
          child: _CyberdeskOrgDesktopGrid().paddingOnly(left: 18, right: 18),
        ),
        if (!isOutgoingOnly) const Divider(height: 1),
        if (!isOutgoingOnly) OnlineStatusWidget()
      ],
    );
  }
}

class _CyberdeskOrgDesktopGrid extends StatefulWidget {
  const _CyberdeskOrgDesktopGrid();

  @override
  State<_CyberdeskOrgDesktopGrid> createState() =>
      _CyberdeskOrgDesktopGridState();
}

class _CyberdeskOrgDesktopGridState extends State<_CyberdeskOrgDesktopGrid> {
  Timer? _onlineTimer;
  Timer? _metadataTimer;
  final _searchController = TextEditingController();
  String _searchText = '';
  bool _pullingPeers = false;
  bool _refreshing = false;

  @override
  void initState() {
    super.initState();
    _searchController.addListener(() {
      if (mounted) {
        setState(() {
          _searchText = _searchController.text.trim().toLowerCase();
        });
      }
    });
    _refreshPeers();
    _onlineTimer = Timer.periodic(const Duration(seconds: 10), (_) {
      _queryOnlineStates();
    });
    _metadataTimer = Timer.periodic(const Duration(seconds: 30), (_) {
      _refreshPeers(showSpinner: false);
    });
  }

  @override
  void dispose() {
    _onlineTimer?.cancel();
    _metadataTimer?.cancel();
    _searchController.dispose();
    super.dispose();
  }

  Future<void> _refreshPeers({bool showSpinner = true}) async {
    if (_pullingPeers) return;
    _pullingPeers = true;
    if (showSpinner && mounted) {
      setState(() {
        _refreshing = true;
      });
    }
    try {
      await gFFI.abModel
          .pullAb(force: ForcePullAb.listAndCurrent, quiet: !showSpinner);
      if (mounted) {
        _queryOnlineStates();
      }
    } finally {
      _pullingPeers = false;
      if (mounted) {
        setState(() {
          _refreshing = false;
        });
      }
    }
  }

  void _queryOnlineStates() {
    final ids = _orgDesktops(gFFI.abModel.peersModel.peers)
        .map((peer) => peer.id)
        .where((id) => id.isNotEmpty)
        .toList(growable: false);
    if (ids.isNotEmpty) {
      bind.queryOnlines(ids: ids);
    }
  }

  List<Peer> _orgDesktops(List<Peer> peers) {
    final selfId = gFFI.serverModel.serverId.text.replaceAll(' ', '');
    return peers.where((peer) => peer.id != selfId).toList();
  }

  String _displayName(Peer peer) {
    final displayName = peer.cyberdeskDisplayName;
    return displayName.isEmpty ? 'Unnamed desktop' : displayName;
  }

  bool _matchesSearch(Peer peer) {
    if (_searchText.isEmpty) {
      return true;
    }
    final machineName = peer.cyberdeskMachineName.toLowerCase();
    final machineId = peer.cyberdeskMachineId.toLowerCase();
    return machineName.contains(_searchText) || machineId.contains(_searchText);
  }

  @override
  Widget build(BuildContext context) {
    return ChangeNotifierProvider<Peers>.value(
      value: gFFI.abModel.peersModel,
      child: Consumer<Peers>(
        builder: (context, model, _) {
          final allDesktops = _orgDesktops(model.peers);
          final desktops =
              allDesktops.where((peer) => _matchesSearch(peer)).toList();
          return Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Padding(
                padding: const EdgeInsets.only(top: 22, bottom: 14),
                child: Row(
                  children: [
                    Expanded(
                      child: Text(
                        'Desktops',
                        style: Theme.of(context).textTheme.titleLarge,
                      ),
                    ),
                    SizedBox(
                      width: 280,
                      child: TextField(
                        controller: _searchController,
                        decoration: InputDecoration(
                          isDense: true,
                          prefixIcon: const Icon(Icons.search),
                          suffixIcon: _searchText.isEmpty
                              ? null
                              : IconButton(
                                  tooltip: translate('Clear'),
                                  icon: const Icon(Icons.close),
                                  onPressed: _searchController.clear,
                                ),
                          hintText: 'Search by machine name or ID',
                        ),
                      ),
                    ),
                    const SizedBox(width: 8),
                    IconButton(
                      tooltip: translate('Refresh'),
                      onPressed: _refreshing ? null : _refreshPeers,
                      icon: _refreshing
                          ? const SizedBox(
                              width: 18,
                              height: 18,
                              child: CircularProgressIndicator(strokeWidth: 2),
                            )
                          : const Icon(Icons.refresh),
                    ),
                  ],
                ),
              ),
              Expanded(
                child: desktops.isEmpty
                    ? Center(
                        child: Text(
                          _refreshing
                              ? translate('Loading...')
                              : allDesktops.isNotEmpty
                                  ? 'No desktops match your search.'
                                  : 'No desktops in this organization yet.',
                          textAlign: TextAlign.center,
                        ),
                      )
                    : GridView.builder(
                        gridDelegate:
                            const SliverGridDelegateWithMaxCrossAxisExtent(
                          maxCrossAxisExtent: 260,
                          mainAxisExtent: 150,
                          mainAxisSpacing: 14,
                          crossAxisSpacing: 14,
                        ),
                        itemCount: desktops.length,
                        itemBuilder: (context, index) {
                          return _CyberdeskDesktopCard(
                            peer: desktops[index],
                            displayName: _displayName(desktops[index]),
                          );
                        },
                      ),
              ),
            ],
          );
        },
      ),
    );
  }
}

class _CyberdeskDesktopCard extends StatefulWidget {
  const _CyberdeskDesktopCard({
    required this.peer,
    required this.displayName,
  });

  final Peer peer;
  final String displayName;

  @override
  State<_CyberdeskDesktopCard> createState() => _CyberdeskDesktopCardState();
}

class _CyberdeskDesktopCardState extends State<_CyberdeskDesktopCard> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final borderColor =
        _hovered ? Theme.of(context).colorScheme.primary : Colors.transparent;
    return MouseRegion(
      onEnter: (_) => setState(() {
        _hovered = true;
      }),
      onExit: (_) => setState(() {
        _hovered = false;
      }),
      child: GestureDetector(
        onTap: () =>
            connect(context, widget.peer.id, tabLabel: widget.displayName),
        child: Tooltip(
          message: widget.displayName,
          waitDuration: const Duration(seconds: 1),
          child: Container(
            decoration: BoxDecoration(
              border: Border.all(color: borderColor, width: 2),
              borderRadius: BorderRadius.circular(16),
              color: Theme.of(context).colorScheme.background,
            ),
            clipBehavior: Clip.antiAlias,
            child: Column(
              children: [
                Expanded(
                  child: Container(
                    width: double.infinity,
                    color: str2color(
                        '${widget.peer.id}${widget.peer.platform}', 0x7f),
                    child: Center(
                      child: getPlatformImage(widget.peer.platform, size: 54),
                    ),
                  ),
                ),
                Container(
                  height: 46,
                  padding: const EdgeInsets.symmetric(horizontal: 12),
                  child: Row(
                    children: [
                      getOnline(8, widget.peer.online),
                      const SizedBox(width: 10),
                      Expanded(
                        child: Text(
                          widget.displayName,
                          overflow: TextOverflow.ellipsis,
                          style: Theme.of(context).textTheme.titleSmall,
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
