use crate::await_with_timeout::AwaitWithTimeout;
use crate::node::Node;
use anyhow::anyhow;
use anyhow::Result;
use bitcoin::secp256k1::PublicKey;
use dlc_manager::contract::contract_input::ContractInput;
use dlc_manager::subchannel::SubChannel;
use dlc_manager::subchannel::SubChannelState;
use dlc_manager::Oracle;
use dlc_manager::Storage;
use dlc_messages::Message;
use dlc_messages::SubChannelMessage;
use lightning::ln::channelmanager::ChannelDetails;

#[derive(Debug, Copy, Clone)]
pub struct Dlc {
    pub id: [u8; 32],
    pub offer_collateral: u64,
    pub accept_collateral: u64,
    pub accept_pk: PublicKey,
}

impl Node {
    pub async fn propose_dlc_channel(
        &self,
        channel_details: &ChannelDetails,
        contract_input: &ContractInput,
    ) -> Result<()> {
        let announcement = tokio::task::spawn_blocking({
            let oracle = self.oracle.clone();
            let event_id = contract_input.contract_infos[0].oracles.event_id.clone();
            move || {
                oracle
                    .get_announcement(&event_id)
                    .map_err(|e| anyhow!(e.to_string()))
            }
        })
        .await_with_timeout()
        .await
        .unwrap()??;

        let sub_channel_offer = self
            .sub_channel_manager
            .offer_sub_channel(
                &channel_details.channel_id,
                contract_input,
                &[vec![announcement]],
            )
            .map_err(|e| anyhow!("{e:#}"))?;

        self.dlc_message_handler.send_message(
            channel_details.counterparty.node_id,
            Message::SubChannel(SubChannelMessage::Offer(sub_channel_offer)),
        );

        Ok(())
    }

    pub fn accept_dlc_channel_offer(&self, channel_id: &[u8; 32]) -> Result<()> {
        let channel_id_hex = hex::encode(channel_id);

        tracing::info!(channel_id = %channel_id_hex, "Accepting DLC channel offer");

        let (node_id, accept_sub_channel) = self
            .sub_channel_manager
            .accept_sub_channel(channel_id)
            .map_err(|e| anyhow!(e.to_string()))?;

        self.dlc_message_handler.send_message(
            node_id,
            Message::SubChannel(SubChannelMessage::Accept(accept_sub_channel)),
        );

        Ok(())
    }

    pub fn propose_dlc_channel_collaborative_settlement(
        &self,
        channel_id: &[u8; 32],
        accept_settlement_amount: u64,
    ) -> Result<()> {
        let channel_id_hex = hex::encode(channel_id);

        tracing::info!(
            channel_id = %channel_id_hex,
            %accept_settlement_amount,
            "Settling DLC channel collaboratively"
        );

        let (sub_channel_close_offer, counterparty_pk) = self
            .sub_channel_manager
            .offer_subchannel_close(channel_id, accept_settlement_amount)
            .map_err(|e| anyhow!("{e}"))?;

        self.dlc_message_handler.send_message(
            counterparty_pk,
            Message::SubChannel(SubChannelMessage::CloseOffer(sub_channel_close_offer)),
        );

        Ok(())
    }

    pub fn accept_dlc_channel_collaborative_settlement(&self, channel_id: &[u8; 32]) -> Result<()> {
        let channel_id_hex = hex::encode(channel_id);

        tracing::info!(channel_id = %channel_id_hex, "Accepting DLC channel collaborative settlement");

        let (sub_channel_close_accept, counterparty_pk) = self
            .sub_channel_manager
            .accept_subchannel_close_offer(channel_id)
            .map_err(|e| anyhow!(e.to_string()))?;

        self.dlc_message_handler.send_message(
            counterparty_pk,
            Message::SubChannel(SubChannelMessage::CloseAccept(sub_channel_close_accept)),
        );

        Ok(())
    }

    pub fn get_dlc_channel_offer(&self, pubkey: &PublicKey) -> Result<Option<SubChannel>> {
        let dlc_channel = self
            .dlc_manager
            .get_store()
            .get_offered_sub_channels()
            .map_err(|e| anyhow!(e.to_string()))?
            .into_iter()
            .find(|dlc_channel| dlc_channel.counter_party == *pubkey);

        Ok(dlc_channel)
    }

    pub fn get_dlc_channel_signed(&self, pubkey: &PublicKey) -> Result<Option<SubChannel>> {
        let matcher = |dlc_channel: &&SubChannel| {
            dlc_channel.counter_party == *pubkey
                && matches!(&dlc_channel.state, SubChannelState::Signed(_))
        };
        let dlc_channel = self.get_dlc_channel(&matcher)?;
        Ok(dlc_channel)
    }

    pub fn get_dlc_channel_close_offer(&self, pubkey: &PublicKey) -> Result<Option<SubChannel>> {
        let matcher = |dlc_channel: &&SubChannel| {
            dlc_channel.counter_party == *pubkey
                && matches!(&dlc_channel.state, SubChannelState::CloseOffered(_))
        };
        let dlc_channel = self.get_dlc_channel(&matcher)?;

        Ok(dlc_channel)
    }

    pub fn list_dlc_channels(&self) -> Result<Vec<SubChannel>> {
        let dlc_channels = self
            .dlc_manager
            .get_store()
            .get_sub_channels()
            .map_err(|e| anyhow!(e.to_string()))?;

        Ok(dlc_channels)
    }

    fn get_dlc_channel(
        &self,
        matcher: impl FnMut(&&SubChannel) -> bool,
    ) -> Result<Option<SubChannel>> {
        let dlc_channels = self.list_dlc_channels()?;
        let dlc_channel = dlc_channels.iter().find(matcher);

        Ok(dlc_channel.cloned())
    }

    #[cfg(test)]
    pub fn process_incoming_messages(&self) -> Result<()> {
        let dlc_message_handler = &self.dlc_message_handler;
        let dlc_manager = &self.dlc_manager;
        let sub_channel_manager = &self.sub_channel_manager;
        let peer_manager = &self.peer_manager;
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

        // NOTE: According to the docs of `process_events` we shouldn't have to call this since
        // we use `lightning-net-tokio`. But we copied this from
        // `p2pderivatives/ldk-sample`
        if dlc_message_handler.has_pending_messages() {
            peer_manager.process_events();
        }

        Ok(())
    }
}

pub fn sub_channel_message_as_str(msg: &SubChannelMessage) -> &str {
    use SubChannelMessage::*;

    match msg {
        Offer(_) => "Offer",
        Accept(_) => "Accept",
        Confirm(_) => "Confirm",
        Finalize(_) => "Finalize",
        CloseOffer(_) => "CloseOffer",
        CloseAccept(_) => "CloseAccept",
        CloseConfirm(_) => "CloseConfirm",
        CloseFinalize(_) => "CloseFinalize",
        Reject(_) => "Reject",
    }
}
