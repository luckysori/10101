use crate::await_with_timeout::AwaitWithTimeout;
use crate::node::Node;
use crate::tests::init_tracing;
use bitcoin::Amount;

#[tokio::test]
#[ignore]
async fn single_hop_payment() {
    init_tracing();

    // Arrange

    let payer = Node::start_test_app("payer")
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();
    let payee = Node::start_test_app("payee")
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    payer
        .connect(payee.info)
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    payer
        .fund(Amount::from_btc(0.1).unwrap())
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    payer
        .open_channel(&payee, 30_000, 0)
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    let payer_balance_before = payer.get_ldk_balance();
    let payee_balance_before = payee.get_ldk_balance();

    // No mining step needed because the channels are _implicitly_
    // configured to support 0-conf

    // Act

    let invoice_amount = 3_000;
    let invoice = payee.create_invoice(invoice_amount).unwrap();

    payer.send_payment(&invoice).unwrap();

    payee
        .wait_for_payment_claimed(invoice.payment_hash())
        .await_with_timeout()
        .await
        .unwrap()
        .unwrap();

    // Assert

    // Sync LN wallet after payment is claimed to update the balances
    payer.sync().unwrap();
    payee.sync().unwrap();

    let payer_balance_after = payer.get_ldk_balance();
    let payee_balance_after = payee.get_ldk_balance();

    assert_eq!(
        payer_balance_before.available - payer_balance_after.available,
        invoice_amount
    );

    assert_eq!(
        payee_balance_after.available - payee_balance_before.available,
        invoice_amount
    );
}
