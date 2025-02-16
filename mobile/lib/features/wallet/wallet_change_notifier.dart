import 'package:f_logs/f_logs.dart';
import 'dart:developer';
import 'package:flutter/material.dart' hide Flow;
import 'package:get_10101/bridge_generated/bridge_definitions.dart' as bridge;
import 'package:get_10101/common/application/event_service.dart';
import 'package:get_10101/common/domain/model.dart';
import 'package:get_10101/features/wallet/application/wallet_service.dart';
import 'package:get_10101/features/wallet/domain/wallet_balances.dart';
import 'domain/wallet_info.dart';

class WalletChangeNotifier extends ChangeNotifier implements Subscriber {
  final WalletService _service;
  WalletInfo walletInfo = WalletInfo(
    balances: WalletBalances(onChain: Amount(0), lightning: Amount(0)),
    history: List.empty(),
  );

  WalletChangeNotifier(this._service);

  void update(WalletInfo? walletInfo) {
    if (walletInfo == null) {
      // skip empty wallet info update.
      return;
    }
    this.walletInfo = walletInfo;

    FLog.trace(text: 'Successfully synced payment history');
    super.notifyListeners();
  }

  Future<void> refreshWalletInfo() async {
    await _service.refreshWalletInfo();
  }

  Amount total() => Amount(onChain().sats + lightning().sats);
  Amount onChain() => walletInfo.balances.onChain;
  Amount lightning() => walletInfo.balances.lightning;

  // TODO: This is not optimal, because we map the WalletInfo in the change notifier. We can do this, but it would be better to do this on the service level.
  @override
  void notify(bridge.Event event) {
    log("Receiving this in the order notifier: ${event.toString()}");

    if (event is bridge.Event_WalletInfoUpdateNotification) {
      update(WalletInfo.fromApi(event.field0));
    } else {
      log("Received unexpected event: ${event.toString()}");
    }
  }
}
