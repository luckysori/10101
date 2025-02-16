use crate::api;
use crate::db::models::Order;
use crate::db::models::OrderState;
use crate::db::models::Position;
use crate::trade;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin;
use diesel::connection::SimpleConnection;
use diesel::r2d2;
use diesel::r2d2::ConnectionManager;
use diesel::r2d2::Pool;
use diesel::r2d2::PooledConnection;
use diesel::SqliteConnection;
use diesel_migrations::embed_migrations;
use diesel_migrations::EmbeddedMigrations;
use diesel_migrations::MigrationHarness;
use state::Storage;
use std::sync::Arc;
use time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

mod custom_types;
pub mod models;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!();
/// Sets the number of max connections to the DB.
///
/// This number was arbitrarily chosen and can be adapted if needed.
const MAX_DB_POOL_SIZE: u32 = 16;

static DB: Storage<Arc<Pool<ConnectionManager<SqliteConnection>>>> = Storage::new();

#[derive(Debug)]
pub struct ConnectionOptions {
    pub enable_wal: bool,
    pub enable_foreign_keys: bool,
    pub busy_timeout: Option<Duration>,
}

impl r2d2::CustomizeConnection<SqliteConnection, r2d2::Error> for ConnectionOptions {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), r2d2::Error> {
        (|| {
            if self.enable_wal {
                conn.batch_execute("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
            }
            if self.enable_foreign_keys {
                conn.batch_execute("PRAGMA foreign_keys = ON;")?;
            }
            if let Some(d) = self.busy_timeout {
                conn.batch_execute(&format!(
                    "PRAGMA busy_timeout = {};",
                    d.whole_milliseconds()
                ))?;
            }
            Ok(())
        })()
        .map_err(diesel::r2d2::Error::QueryError)
    }
}

pub fn init_db(db_dir: &str, network: bitcoin::Network) -> Result<()> {
    let database_url = format!("sqlite://{db_dir}/trades-{network}.sqlite");
    let manager = ConnectionManager::<SqliteConnection>::new(database_url);
    let pool = r2d2::Pool::builder()
        .max_size(MAX_DB_POOL_SIZE)
        .connection_customizer(Box::new(ConnectionOptions {
            enable_wal: true,
            enable_foreign_keys: true,
            busy_timeout: Some(Duration::seconds(30)),
        }))
        .build(manager)?;

    let mut connection = pool.get()?;

    connection
        .run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow!("could not run db migration: {e:#}"))?;
    tracing::debug!("Database migration run - db initialized");

    DB.set(Arc::new(pool));

    Ok(())
}

pub fn connection() -> Result<PooledConnection<ConnectionManager<SqliteConnection>>> {
    let pool = DB.try_get().context("DB uninitialised").cloned()?;

    pool.get()
        .map_err(|e| anyhow!("cannot acquire database connection: {e:#}"))
}

pub fn update_last_login() -> Result<api::LastLogin> {
    let mut db = connection()?;
    let now = OffsetDateTime::now_utc();
    let last_login = models::LastLogin::update_last_login(now, &mut db)?.into();
    Ok(last_login)
}

pub fn insert_order(order: trade::order::Order) -> Result<trade::order::Order> {
    let mut db = connection()?;
    let order = Order::insert(order.into(), &mut db)?;

    Ok(order.try_into()?)
}

pub fn update_order_state(order_id: Uuid, order_state: trade::order::OrderState) -> Result<()> {
    let mut db = connection()?;
    Order::update_state(order_id.to_string(), order_state.into(), &mut db)
        .context("Failed to update order state")?;

    Ok(())
}

pub fn get_order(order_id: Uuid) -> Result<trade::order::Order> {
    let mut db = connection()?;
    let order = Order::get(order_id.to_string(), &mut db)?;

    Ok(order.try_into()?)
}

pub fn get_orders_for_ui() -> Result<Vec<trade::order::Order>> {
    let mut db = connection()?;
    let orders = Order::get_without_rejected_and_initial(&mut db)?;

    // TODO: Can probably be optimized with combinator
    let mut mapped = vec![];
    for order in orders {
        mapped.push(order.try_into()?)
    }

    Ok(mapped)
}

pub fn get_filled_orders() -> Result<Vec<trade::order::Order>> {
    let mut db = connection()?;

    let orders = Order::get_by_state(OrderState::Filled, &mut db)?;
    let orders = orders
        .into_iter()
        .map(|order| {
            order
                .try_into()
                .context("Failed to convert to trade::order::Order")
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(orders)
}

/// Returns an order of there is currently an order that is being filled
pub fn maybe_get_order_in_filling() -> Result<Option<trade::order::Order>> {
    let mut db = connection()?;
    let orders = Order::get_by_state(OrderState::Filling, &mut db)?;

    if orders.is_empty() {
        return Ok(None);
    }

    if orders.len() > 1 {
        bail!("More than one order is being filled at the same time, this should not happen.")
    }

    let first = orders
        .get(0)
        .expect("at this point we know there is exactly one order");

    Ok(Some(first.clone().try_into()?))
}

pub fn delete_order(order_id: Uuid) -> Result<()> {
    let mut db = connection()?;
    Order::delete(order_id.to_string(), &mut db)?;

    Ok(())
}

pub fn insert_position(position: trade::position::Position) -> Result<trade::position::Position> {
    let mut db = connection()?;
    let position = Position::insert(position.into(), &mut db)?;

    Ok(position.into())
}

pub fn get_positions() -> Result<Vec<trade::position::Position>> {
    let mut db = connection()?;
    let positions = Position::get_all(&mut db)?;
    let positions = positions
        .into_iter()
        .map(|position| position.into())
        .collect();

    Ok(positions)
}

pub fn delete_positions() -> Result<()> {
    let mut db = connection()?;
    Position::delete_all(&mut db)?;

    Ok(())
}

pub fn update_position_state(
    contract_symbol: ::trade::ContractSymbol,
    position_state: trade::position::PositionState,
) -> Result<()> {
    let mut db = connection()?;
    Position::update_state(contract_symbol.into(), position_state.into(), &mut db)
        .context("Failed to update position state")?;

    Ok(())
}
