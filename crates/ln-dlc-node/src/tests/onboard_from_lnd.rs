use crate::await_with_timeout::AwaitWithTimeout;
use crate::node::Node;
use crate::tests::init_tracing;
use crate::tests::lnd::LndNode;
use crate::tests::log_channel_id;
use bitcoin::Amount;
use std::time::Duration;

#[tokio::test]
#[ignore]
async fn onboard_from_lnd() {
    init_tracing();

    let coordinator = Node::start_test_coordinator("coordinator")
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();
    let payee = Node::start_test_app("payee")
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();
    let payer = LndNode::new();

    payee
        .connect(coordinator.info)
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    // Fund the on-chain wallets of the nodes who will open a channel
    coordinator
        .fund(Amount::from_sat(100_000))
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();
    payer
        .fund(Amount::from_sat(100_000))
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    payer
        .open_channel(&coordinator, Amount::from_sat(50_000))
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    log_channel_id(&coordinator, 0, "lnd-coordinator");

    coordinator.sync().unwrap();
    payee.sync().unwrap();

    let invoice_amount = 1000;

    let fake_scid = coordinator.create_intercept_scid(payee.info.pubkey);
    let invoice = payee
        .create_interceptable_invoice(
            Some(invoice_amount),
            fake_scid,
            coordinator.info.pubkey,
            0,
            "".to_string(),
        )
        .unwrap();

    // lnd sends the payment
    payer
        .send_payment(invoice)
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    // For the payment to be claimed before the wallets sync
    tokio::time::sleep(Duration::from_secs(3))
        .await_with_timeout()
        .await
        .unwrap();

    coordinator.sync().unwrap();
    payee.sync().unwrap();

    // Assert

    let payee_balance = payee.get_ldk_balance();
    assert_eq!(invoice_amount, payee_balance.available);
}
