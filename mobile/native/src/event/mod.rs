pub mod api;
mod event_hub;
pub mod subscriber;

use crate::api::WalletInfo;
use coordinator_commons::TradeParams;
use orderbook_commons::Prices;
use std::hash::Hash;
use trade::ContractSymbol;

use crate::event::event_hub::get;
use crate::event::subscriber::Subscriber;
use crate::trade::order::Order;
use crate::trade::position::Position;

pub fn subscribe(subscriber: impl Subscriber + 'static + Send + Sync + Clone) {
    get().subscribe(subscriber);
}

pub fn publish(event: &EventInternal) {
    get().publish(event);
}

#[derive(Clone, Debug)]
pub enum EventInternal {
    Init(String),
    Log(String),
    OrderUpdateNotification(Order),
    WalletInfoUpdateNotification(WalletInfo),
    OrderFilledWith(Box<TradeParams>),
    PositionUpdateNotification(Position),
    PositionCloseNotification(ContractSymbol),
    PriceUpdateNotification(Prices),
}

impl From<EventInternal> for EventType {
    fn from(value: EventInternal) -> Self {
        match value {
            EventInternal::Init(_) => EventType::Init,
            EventInternal::Log(_) => EventType::Log,
            EventInternal::OrderUpdateNotification(_) => EventType::OrderUpdateNotification,
            EventInternal::WalletInfoUpdateNotification(_) => {
                EventType::WalletInfoUpdateNotification
            }
            EventInternal::OrderFilledWith(_) => EventType::OrderFilledWith,
            EventInternal::PositionUpdateNotification(_) => EventType::PositionUpdateNotification,
            EventInternal::PositionCloseNotification(_) => EventType::PositionClosedNotification,
            EventInternal::PriceUpdateNotification(_) => EventType::PriceUpdateNotification,
        }
    }
}

#[derive(Copy, Clone, Eq, Hash, PartialEq)]
pub enum EventType {
    Init,
    Log,
    OrderUpdateNotification,
    WalletInfoUpdateNotification,
    OrderFilledWith,
    PositionUpdateNotification,
    PositionClosedNotification,
    PriceUpdateNotification,
}
