use anyhow::Result;
use bitcoin::secp256k1::PublicKey;
use bitcoin::Network;
use futures::TryStreamExt;
use orderbook_commons::NewOrder;
use orderbook_commons::OrderResponse;
use orderbook_commons::OrderType;
use reqwest::Url;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use trade::Direction;
use uuid::Uuid;

mod bitmex_client;
mod orderbook_client;

pub async fn run(orderbook_url: Url, maker_id: PublicKey, network: Network) -> Result<()> {
    let network = match network {
        Network::Bitcoin => bitmex_stream::Network::Mainnet,
        _ => bitmex_stream::Network::Testnet,
    };
    let mut price_stream = bitmex_client::bitmex(network).await;

    let mut last_bid = None;
    let mut last_ask = None;

    while let Some(quote) = price_stream.try_next().await? {
        tracing::debug!("Received new quote {quote:?}");

        last_bid = update_order(
            orderbook_url.clone(),
            quote.ask(),
            Direction::Long,
            maker_id,
            last_bid,
            dec!(1000),
        )
        .await;
        last_ask = update_order(
            orderbook_url.clone(),
            quote.bid(),
            Direction::Short,
            maker_id,
            last_ask,
            dec!(1000),
        )
        .await;
    }

    Ok(())
}

async fn update_order(
    orderbook_url: Url,
    price: Decimal,
    direction: Direction,
    maker_id: PublicKey,
    last_order: Option<OrderResponse>,
    quantity: Decimal,
) -> Option<OrderResponse> {
    if let Some(last_order) = last_order {
        let order_id = last_order.id;
        if let Err(err) = orderbook_client::delete_order(orderbook_url.clone(), order_id).await {
            tracing::error!("Failed deleting old order `{order_id}` because of {err:#}");
        }
    };

    match orderbook_client::post_new_order(
        orderbook_url,
        NewOrder {
            id: Uuid::new_v4(),
            price,
            quantity,
            trader_id: maker_id,
            direction,
            order_type: OrderType::Limit,
        },
    )
    .await
    {
        Ok(order) => Some(order),
        Err(err) => {
            tracing::error!("Failed posting new order {err:#}");
            None
        }
    }
}
