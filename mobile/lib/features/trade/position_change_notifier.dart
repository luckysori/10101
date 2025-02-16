import 'dart:developer';
import 'package:flutter/material.dart';
import 'package:get_10101/common/application/event_service.dart';
import 'package:get_10101/common/domain/model.dart';
import 'package:get_10101/common/dummy_values.dart';
import 'package:get_10101/features/trade/application/order_service.dart';
import 'package:get_10101/features/trade/application/position_service.dart';
import 'package:get_10101/features/trade/domain/contract_symbol.dart';
import 'package:get_10101/bridge_generated/bridge_definitions.dart' as bridge;

import 'domain/position.dart';
import 'domain/price.dart';

class PositionChangeNotifier extends ChangeNotifier implements Subscriber {
  final PositionService _positionService;
  final OrderService _orderService;

  Map<ContractSymbol, Position> positions = {};

  Price? _price;

  Future<void> initialize() async {
    List<Position> positions = await _positionService.fetchPositions();
    for (Position position in positions) {
      this.positions[position.contractSymbol] = position;
    }
    _price = Price(bid: dummyBidPrice, ask: dummyAskPrice);

    notifyListeners();
  }

  PositionChangeNotifier(this._positionService, this._orderService);

  @override
  void notify(bridge.Event event) {
    log("Receiving this in the position notifier: ${event.toString()}");

    if (event is bridge.Event_PositionUpdateNotification) {
      Position position = Position.fromApi(event.field0);

      if (_price != null) {
        position.unrealizedPnl = Amount(_positionService.calculatePnl(position, _price!));
      } else {
        position.unrealizedPnl = null;
      }
      positions[position.contractSymbol] = position;
    } else if (event is bridge.Event_PositionClosedNotification) {
      ContractSymbol contractSymbol = ContractSymbol.fromApi(event.field0.contractSymbol);
      positions.remove(contractSymbol);
    } else if (event is bridge.Event_PriceUpdateNotification) {
      _price = Price.fromApi(event.field0);
      for (ContractSymbol symbol in positions.keys) {
        if (_price != null) {
          if (positions[symbol] != null) {
            positions[symbol]!.unrealizedPnl =
                Amount(_positionService.calculatePnl(positions[symbol]!, _price!));
          }
        }
      }
    } else {
      log("Received unexpected event: ${event.toString()}");
    }

    notifyListeners();
  }

  Future<void> closePosition(ContractSymbol contractSymbol) async {
    if (positions[contractSymbol] == null) {
      throw Exception("No position for contract symbol $contractSymbol");
    }

    Position position = positions[contractSymbol]!;
    await _orderService.submitMarketOrder(position.leverage, position.quantity,
        position.contractSymbol, position.direction.opposite());
  }
}
