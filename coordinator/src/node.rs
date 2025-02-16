use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use bitcoin::secp256k1::PublicKey;
use coordinator_commons::TradeParams;
use dlc_manager::contract::contract_input::ContractInput;
use dlc_manager::contract::contract_input::ContractInputInfo;
use dlc_manager::contract::contract_input::OracleInput;
use dlc_manager::contract::numerical_descriptor::NumericalDescriptor;
use dlc_manager::contract::ContractDescriptor;
use dlc_manager::payout_curve::PayoutFunction;
use dlc_manager::payout_curve::PayoutFunctionPiece;
use dlc_manager::payout_curve::PayoutPoint;
use dlc_manager::payout_curve::PolynomialPayoutCurvePiece;
use dlc_manager::payout_curve::RoundingInterval;
use dlc_manager::payout_curve::RoundingIntervals;
use dlc_manager::ChannelId;
use dlc_messages::message_handler::MessageHandler as DlcMessageHandler;
use dlc_messages::Message;
use lightning::ln::channelmanager::ChannelDetails;
use ln_dlc_node::node::sub_channel_message_as_str;
use ln_dlc_node::node::DlcManager;
use ln_dlc_node::node::SubChannelManager;
use ln_dlc_node::PeerManager;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use trade::cfd;
use trade::cfd::calculate_long_liquidation_price;
use trade::cfd::calculate_margin;
use trade::cfd::calculate_short_liquidation_price;
use trade::cfd::BTCUSD_MAX_PRICE;
use trade::ContractSymbol;
use trade::Direction;

/// The leverage used by the coordinator for all trades.
const COORDINATOR_LEVERAGE: f64 = 1.0;

pub struct Node {
    pub inner: Arc<ln_dlc_node::node::Node>,
    pub positions: Mutex<HashMap<String, Position>>,
}

pub struct Position {
    pub contract_symbol: ContractSymbol,
    pub leverage: f64,
    pub quantity: f64,
    pub direction: Direction,
    pub trader: PublicKey,
    pub average_entry_price: f64,
}

impl Node {
    pub async fn trade(&self, trade_params: &TradeParams) -> Result<()> {
        match self.decide_trade_action(trade_params)? {
            TradeAction::Open => self.open_position(trade_params).await?,
            TradeAction::Close(channel_id) => self.close_position(trade_params, channel_id).await?,
        };

        Ok(())
    }

    async fn open_position(&self, trade_params: &TradeParams) -> Result<()> {
        tracing::info!("Opening position");

        // todo: Revisit position model and store to database.
        let position = Position {
            contract_symbol: trade_params.contract_symbol,
            leverage: trade_params.leverage,
            quantity: trade_params.quantity,
            direction: trade_params.direction,
            trader: trade_params.pubkey,
            average_entry_price: trade_params
                .average_execution_price()
                .to_f64()
                .expect("to fit into f64"),
        };
        self.positions
            .lock()
            .expect("to get lock on positions")
            .insert(trade_params.pubkey.to_string(), position);

        let margin_trader = margin_trader(trade_params);
        let margin_coordinator = margin_coordinator(trade_params);

        let leverage_long = leverage_long(trade_params);
        let leverage_short = leverage_short(trade_params);

        let total_collateral = margin_coordinator + margin_trader;

        let contract_descriptor = build_contract_descriptor(
            total_collateral,
            trade_params.average_execution_price(),
            leverage_long,
            leverage_short,
        )
        .context("Could not build contract descriptor")?;

        let contract_symbol = trade_params.contract_symbol.label();
        let maturity_time = trade_params.filled_with.expiry_timestamp;
        let maturity_time = maturity_time.unix_timestamp();

        // The contract input to be used for setting up the trade between the trader and the
        // coordinator
        let event_id = format!("{contract_symbol}{maturity_time}");
        tracing::debug!(event_id, "Proposing dlc channel");
        let contract_input = ContractInput {
            offer_collateral: margin_coordinator,
            accept_collateral: margin_trader,
            fee_rate: 2,
            contract_infos: vec![ContractInputInfo {
                contract_descriptor,
                oracles: OracleInput {
                    public_keys: vec![self.inner.oracle_pk()],
                    event_id,
                    threshold: 1,
                },
            }],
        };

        let channel_details = self.get_counterparty_channel(trade_params.pubkey)?;
        self.inner
            .propose_dlc_channel(&channel_details, &contract_input)
            .await
            .context("Could not propose dlc channel")?;
        Ok(())
    }

    async fn close_position(
        &self,
        trade_params: &TradeParams,
        channel_id: ChannelId,
    ) -> Result<()> {
        let trader_pk = trade_params.pubkey;

        tracing::info!(
            order_id = %trade_params.filled_with.order_id,
            %trader_pk,
            "Closing position"
        );

        let leverage_long = leverage_long(trade_params);
        let leverage_short = leverage_short(trade_params);

        let closing_price = trade_params.average_execution_price();

        let opening_price = match self
            .positions
            .lock()
            .expect("to get lock on positions")
            .remove(&trade_params.pubkey.to_string())
        {
            Some(position) => Decimal::try_from(position.average_entry_price)?,
            None => {
                tracing::warn!("Did not find position in memory, thus we do not have the opening price to calculate a correct accept settlement amount. Using the closing price.");
                // FIXME: This is wrong as we cannot use the closing price to calculated the
                // `accept_settlement_amount`. We must save the initial price when creating the
                // position and use it here again for closing.
                closing_price
            }
        };

        let accept_settlement_amount = calculate_accept_settlement_amount(
            opening_price,
            closing_price,
            trade_params.quantity,
            leverage_long,
            leverage_short,
            trade_params.direction,
        )?;

        tracing::debug!(
            "Settling position of {accept_settlement_amount} with {}",
            trade_params.pubkey
        );

        self.inner
            .propose_dlc_channel_collaborative_settlement(&channel_id, accept_settlement_amount)?;

        Ok(())
    }

    /// Decides what trade action should be performed according to the
    /// coordinator's current trading status with the trader.
    ///
    /// We look for a pre-existing position with the trader and
    /// instruct accordingly:
    ///
    /// 1. If a position of equal quantity and opposite direction is
    /// found, we direct the caller to close the position.
    ///
    /// 2. If no position is found, we direct the caller to open a
    /// position.
    ///
    /// 3. If a position of differing quantity is found, we direct the
    /// caller to extend or reduce the position. _This is currently
    /// not supported_.
    fn decide_trade_action(&self, trade_params: &TradeParams) -> Result<TradeAction> {
        let action = match self.inner.get_dlc_channel_signed(&trade_params.pubkey)? {
            Some(subchannel) => {
                // FIXME: Should query the database for more
                // information

                // TODO: Detect if the position should be
                // extended/reduced. Return corresponding error as
                // this is currently not supported.

                TradeAction::Close(subchannel.channel_id)
            }
            None => TradeAction::Open,
        };

        Ok(action)
    }

    fn get_counterparty_channel(&self, trader_pubkey: PublicKey) -> Result<ChannelDetails> {
        let channel_details = self.inner.list_usable_channels();
        let channel_details = channel_details
            .into_iter()
            .find(|c| c.counterparty.node_id == trader_pubkey)
            .context("Channel details not found")
            .map_err(|e| anyhow!("{e:#}"))?;
        Ok(channel_details)
    }
}

enum TradeAction {
    Open,
    Close(ChannelId),
    // Extend,
    // Reduce,
}

/// Calculates the accept settlement amount based on the pnl.
fn calculate_accept_settlement_amount(
    opening_price: Decimal,
    closing_price: Decimal,
    quantity: f64,
    long_leverage: f64,
    short_leverage: f64,
    direction: Direction,
) -> Result<u64> {
    let pnl = cfd::calculate_pnl(
        opening_price,
        closing_price,
        quantity,
        long_leverage,
        short_leverage,
        direction,
    )?;

    let leverage = match direction {
        Direction::Long => long_leverage,
        Direction::Short => short_leverage,
    };

    let margin_trader = calculate_margin(opening_price, quantity, leverage);

    let accept_settlement_amount = Decimal::from(margin_trader) + Decimal::from(pnl);
    // the amount can only be positive, adding a safeguard here with the max comparison to
    // ensure the i64 fits into u64
    let accept_settlement_amount = accept_settlement_amount
        .max(Decimal::ZERO)
        .to_u64()
        .expect("to fit into u64");
    Ok(accept_settlement_amount)
}

fn margin_trader(trade_params: &TradeParams) -> u64 {
    calculate_margin(
        trade_params.average_execution_price(),
        trade_params.quantity,
        trade_params.leverage,
    )
}

fn margin_coordinator(trade_params: &TradeParams) -> u64 {
    calculate_margin(
        trade_params.average_execution_price(),
        trade_params.quantity,
        COORDINATOR_LEVERAGE,
    )
}

fn leverage_long(trade_params: &TradeParams) -> f64 {
    match trade_params.direction {
        Direction::Long => trade_params.leverage,
        Direction::Short => COORDINATOR_LEVERAGE,
    }
}

fn leverage_short(trade_params: &TradeParams) -> f64 {
    match trade_params.direction {
        Direction::Long => COORDINATOR_LEVERAGE,
        Direction::Short => trade_params.leverage,
    }
}

fn get_rounding_intervals() -> RoundingIntervals {
    RoundingIntervals {
        intervals: vec![RoundingInterval {
            begin_interval: 0,
            rounding_mod: 500,
        }],
    }
}

/// Builds the contract descriptor from the point of view of the trader.
fn build_contract_descriptor(
    total_collateral: u64,
    initial_price: Decimal,
    leverage_long: f64,
    leverage_short: f64,
) -> Result<ContractDescriptor> {
    Ok(ContractDescriptor::Numerical(NumericalDescriptor {
        payout_function: build_payout_function(
            total_collateral,
            initial_price,
            leverage_long,
            leverage_short,
        )?,
        rounding_intervals: get_rounding_intervals(),
        difference_params: None,
        oracle_numeric_infos: dlc_trie::OracleNumericInfo {
            base: 2,
            nb_digits: vec![20],
        },
    }))
}

/// Builds a [`PayoutFunction`].
///
/// TODO: We are currently building a linear payout function for
/// simplicity. This is *wrong*. We should build an inverse payout
/// function like we used to do in ItchySats.
fn build_payout_function(
    total_collateral: u64,
    initial_price: Decimal,
    leverage_long: f64,
    leverage_short: f64,
) -> Result<PayoutFunction> {
    let leverage_short = Decimal::try_from(leverage_short)?;
    let liquidation_price_short = calculate_short_liquidation_price(leverage_short, initial_price);

    let leverage_long = Decimal::try_from(leverage_long)?;
    let liquidation_price_long = calculate_long_liquidation_price(leverage_long, initial_price);

    let lower_limit = liquidation_price_long
        .floor()
        .to_u64()
        .expect("Failed to fit floored liquidation price to u64");
    let upper_limit = liquidation_price_short
        .floor()
        .to_u64()
        .expect("Failed to fit floored liquidation price to u64");

    let lower_range = PolynomialPayoutCurvePiece::new(vec![
        PayoutPoint {
            event_outcome: 0,
            outcome_payout: 0,
            extra_precision: 0,
        },
        PayoutPoint {
            event_outcome: lower_limit,
            outcome_payout: 0,
            extra_precision: 0,
        },
    ])
    .map_err(|e| anyhow!("{e:#}"))?;

    let middle_range = PolynomialPayoutCurvePiece::new(vec![
        PayoutPoint {
            event_outcome: lower_limit,
            outcome_payout: 0,
            extra_precision: 0,
        },
        PayoutPoint {
            event_outcome: upper_limit,
            outcome_payout: total_collateral,
            extra_precision: 0,
        },
    ])
    .map_err(|e| anyhow!("{e:#}"))?;

    let mut pieces = vec![
        PayoutFunctionPiece::PolynomialPayoutCurvePiece(lower_range),
        PayoutFunctionPiece::PolynomialPayoutCurvePiece(middle_range),
    ];

    // When the upper limit is greater than or equal to the
    // `BTCUSD_MAX_PRICE`, we don't have to add another curve piece.
    if upper_limit < BTCUSD_MAX_PRICE {
        let upper_range = PolynomialPayoutCurvePiece::new(vec![
            PayoutPoint {
                event_outcome: upper_limit,
                outcome_payout: total_collateral,
                extra_precision: 0,
            },
            PayoutPoint {
                event_outcome: BTCUSD_MAX_PRICE,
                outcome_payout: total_collateral,
                extra_precision: 0,
            },
        ])
        .map_err(|e| anyhow!("{e:#}"))?;

        pieces.push(PayoutFunctionPiece::PolynomialPayoutCurvePiece(upper_range));
    }

    PayoutFunction::new(pieces).map_err(|e| anyhow!("{e:#}"))
}

pub fn process_incoming_messages_internal(
    dlc_message_handler: &DlcMessageHandler,
    dlc_manager: &DlcManager,
    sub_channel_manager: &SubChannelManager,
    peer_manager: &PeerManager,
) -> Result<()> {
    let messages = dlc_message_handler.get_and_clear_received_messages();

    for (node_id, msg) in messages {
        match msg {
            Message::OnChain(_) | Message::Channel(_) => {
                tracing::debug!(from = %node_id, "Processing DLC-manager message");
                let resp = dlc_manager
                    .on_dlc_message(&msg, node_id)
                    .map_err(|e| anyhow!(e.to_string()))?;

                if let Some(msg) = resp {
                    tracing::debug!(to = %node_id, "Sending DLC-manager message");
                    dlc_message_handler.send_message(node_id, msg);
                }
            }
            Message::SubChannel(msg) => {
                tracing::debug!(
                    from = %node_id,
                    msg = %sub_channel_message_as_str(&msg),
                    "Processing DLC channel message"
                );
                let resp = sub_channel_manager
                    .on_sub_channel_message(&msg, &node_id)
                    .map_err(|e| anyhow!(e.to_string()))?;

                if let Some(msg) = resp {
                    tracing::debug!(
                        to = %node_id,
                        msg = %sub_channel_message_as_str(&msg),
                        "Sending DLC channel message"
                    );
                    dlc_message_handler.send_message(node_id, Message::SubChannel(msg));
                }
            }
        }
    }

    // NOTE: According to the docs of `process_events` we shouldn't have to call this since we
    // use `lightning-net-tokio`. But we copied this from `p2pderivatives/ldk-sample`
    if dlc_message_handler.has_pending_messages() {
        peer_manager.process_events();
    }

    Ok(())
}

#[cfg(test)]
pub mod tests {
    use crate::node::calculate_accept_settlement_amount;
    use rust_decimal::Decimal;
    use trade::cfd::calculate_margin;
    use trade::Direction;

    // some basic sanity tests, that in case the position goes the right or wrong way the settlement
    // amount is moving correspondingly up or down.

    #[test]
    fn given_a_long_position_and_a_larger_closing_price() {
        let opening_price = Decimal::from(22000);
        let closing_price = Decimal::from(23000);
        let quantity: f64 = 1.0;
        let accept_settlement_amount = calculate_accept_settlement_amount(
            opening_price,
            closing_price,
            quantity,
            1.0,
            1.0,
            Direction::Long,
        )
        .unwrap();

        let margin_trader = calculate_margin(opening_price, quantity, 1.0);
        assert!(accept_settlement_amount > margin_trader);
    }

    #[test]
    fn given_a_short_position_and_a_larger_closing_price() {
        let opening_price = Decimal::from(22000);
        let closing_price = Decimal::from(23000);
        let quantity: f64 = 1.0;
        let accept_settlement_amount = calculate_accept_settlement_amount(
            opening_price,
            closing_price,
            quantity,
            1.0,
            1.0,
            Direction::Short,
        )
        .unwrap();

        let margin_trader = calculate_margin(opening_price, quantity, 1.0);
        assert!(accept_settlement_amount < margin_trader);
    }

    #[test]
    fn given_a_long_position_and_a_smaller_closing_price() {
        let opening_price = Decimal::from(23000);
        let closing_price = Decimal::from(22000);
        let quantity: f64 = 1.0;
        let accept_settlement_amount = calculate_accept_settlement_amount(
            opening_price,
            closing_price,
            quantity,
            1.0,
            1.0,
            Direction::Long,
        )
        .unwrap();

        let margin_trader = calculate_margin(opening_price, quantity, 1.0);
        assert!(accept_settlement_amount < margin_trader);
    }

    #[test]
    fn given_a_short_position_and_a_smaller_closing_price() {
        let opening_price = Decimal::from(23000);
        let closing_price = Decimal::from(22000);
        let quantity: f64 = 1.0;
        let accept_settlement_amount = calculate_accept_settlement_amount(
            opening_price,
            closing_price,
            quantity,
            1.0,
            1.0,
            Direction::Short,
        )
        .unwrap();

        let margin_trader = calculate_margin(opening_price, quantity, 1.0);
        assert!(accept_settlement_amount > margin_trader);
    }
}
