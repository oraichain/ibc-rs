//! Protocol logic specific to ICS4 messages of type `MsgChannelOpenConfirm`.

use crate::core::ics02_client::error::ClientError;
use crate::prelude::*;
use ibc_proto::protobuf::Protobuf;

use crate::core::events::{IbcEvent, MessageEvent};
use crate::core::ics02_client::client_state::{ClientStateCommon, ClientStateValidation};
use crate::core::ics02_client::consensus_state::ConsensusState;
use crate::core::ics03_connection::connection::State as ConnectionState;
use crate::core::ics04_channel::channel::State;
use crate::core::ics04_channel::channel::{ChannelEnd, Counterparty, State as ChannelState};
use crate::core::ics04_channel::error::ChannelError;
use crate::core::ics04_channel::events::OpenConfirm;
use crate::core::ics04_channel::msgs::chan_open_confirm::MsgChannelOpenConfirm;
use crate::core::ics24_host::path::Path;
use crate::core::ics24_host::path::{ChannelEndPath, ClientConsensusStatePath};
use crate::core::router::Module;
use crate::core::{ContextError, ExecutionContext, ValidationContext};

pub(crate) fn chan_open_confirm_validate<ValCtx>(
    ctx_b: &ValCtx,
    module: &dyn Module,
    msg: MsgChannelOpenConfirm,
) -> Result<(), ContextError>
where
    ValCtx: ValidationContext,
{
    validate(ctx_b, &msg)?;

    module.on_chan_open_confirm_validate(&msg.port_id_on_b, &msg.chan_id_on_b)?;

    Ok(())
}

pub(crate) fn chan_open_confirm_execute<ExecCtx>(
    ctx_b: &mut ExecCtx,
    module: &mut dyn Module,
    msg: MsgChannelOpenConfirm,
) -> Result<(), ContextError>
where
    ExecCtx: ExecutionContext,
{
    let extras = module.on_chan_open_confirm_execute(&msg.port_id_on_b, &msg.chan_id_on_b)?;
    let chan_end_path_on_b = ChannelEndPath::new(&msg.port_id_on_b, &msg.chan_id_on_b);
    let chan_end_on_b = ctx_b.channel_end(&chan_end_path_on_b)?;

    // state changes
    {
        let chan_end_on_b = {
            let mut chan_end_on_b = chan_end_on_b.clone();
            chan_end_on_b.set_state(State::Open);

            chan_end_on_b
        };
        ctx_b.store_channel(&chan_end_path_on_b, chan_end_on_b)?;
    }

    // emit events and logs
    {
        ctx_b.log_message("success: channel open confirm".to_string());

        let conn_id_on_b = chan_end_on_b.connection_hops[0].clone();
        let port_id_on_a = chan_end_on_b.counterparty().port_id.clone();
        let chan_id_on_a = chan_end_on_b
            .counterparty()
            .channel_id
            .clone()
            .ok_or(ContextError::ChannelError(ChannelError::Other {
            description:
                "internal error: ChannelEnd doesn't have a counterparty channel id in OpenConfirm"
                    .to_string(),
        }))?;

        let core_event = IbcEvent::OpenConfirmChannel(OpenConfirm::new(
            msg.port_id_on_b.clone(),
            msg.chan_id_on_b.clone(),
            port_id_on_a,
            chan_id_on_a,
            conn_id_on_b,
        ));
        ctx_b.emit_ibc_event(IbcEvent::Message(MessageEvent::Channel));
        ctx_b.emit_ibc_event(core_event);

        for module_event in extras.events {
            ctx_b.emit_ibc_event(IbcEvent::Module(module_event));
        }

        for log_message in extras.log {
            ctx_b.log_message(log_message);
        }
    }

    Ok(())
}

fn validate<Ctx>(ctx_b: &Ctx, msg: &MsgChannelOpenConfirm) -> Result<(), ContextError>
where
    Ctx: ValidationContext,
{
    ctx_b.validate_message_signer(&msg.signer)?;

    // Unwrap the old channel end and validate it against the message.
    let chan_end_path_on_b = ChannelEndPath::new(&msg.port_id_on_b, &msg.chan_id_on_b);
    let chan_end_on_b = ctx_b.channel_end(&chan_end_path_on_b)?;

    // Validate that the channel end is in a state where it can be confirmed.
    chan_end_on_b.verify_state_matches(&ChannelState::TryOpen)?;

    // An OPEN IBC connection running on the local (host) chain should exist.
    chan_end_on_b.verify_connection_hops_length()?;

    let conn_end_on_b = ctx_b.connection_end(&chan_end_on_b.connection_hops()[0])?;

    conn_end_on_b.verify_state_matches(&ConnectionState::Open)?;

    // Verify proofs
    {
        let client_id_on_b = conn_end_on_b.client_id();
        let client_state_of_a_on_b = ctx_b.client_state(client_id_on_b)?;

        {
            let status = client_state_of_a_on_b
                .status(ctx_b.get_client_validation_context(), client_id_on_b)?;
            if !status.is_active() {
                return Err(ClientError::ClientNotActive { status }.into());
            }
        }
        client_state_of_a_on_b.validate_proof_height(msg.proof_height_on_a)?;

        let client_cons_state_path_on_b =
            ClientConsensusStatePath::new(client_id_on_b, &msg.proof_height_on_a);
        let consensus_state_of_a_on_b = ctx_b.consensus_state(&client_cons_state_path_on_b)?;
        let prefix_on_a = conn_end_on_b.counterparty().prefix();
        let port_id_on_a = &chan_end_on_b.counterparty().port_id;
        let chan_id_on_a = chan_end_on_b
            .counterparty()
            .channel_id()
            .ok_or(ChannelError::MissingCounterparty)?;
        let conn_id_on_a = conn_end_on_b.counterparty().connection_id().ok_or(
            ChannelError::UndefinedConnectionCounterparty {
                connection_id: chan_end_on_b.connection_hops()[0].clone(),
            },
        )?;

        let expected_chan_end_on_a = ChannelEnd::new(
            ChannelState::Open,
            *chan_end_on_b.ordering(),
            Counterparty::new(msg.port_id_on_b.clone(), Some(msg.chan_id_on_b.clone())),
            vec![conn_id_on_a.clone()],
            chan_end_on_b.version.clone(),
        )?;
        let chan_end_path_on_a = ChannelEndPath::new(port_id_on_a, chan_id_on_a);

        // Verify the proof for the channel state against the expected channel end.
        // A counterparty channel id of None in not possible, and is checked in msg.
        client_state_of_a_on_b
            .verify_membership(
                prefix_on_a,
                &msg.proof_chan_end_on_a,
                consensus_state_of_a_on_b.root(),
                Path::ChannelEnd(chan_end_path_on_a),
                expected_chan_end_on_a.encode_vec(),
            )
            .map_err(ChannelError::VerifyChannelFailed)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::*;
    use test_log::test;

    use crate::core::ics03_connection::connection::ConnectionEnd;
    use crate::core::ics03_connection::connection::Counterparty as ConnectionCounterparty;
    use crate::core::ics03_connection::connection::State as ConnectionState;
    use crate::core::ics03_connection::msgs::test_util::get_dummy_raw_counterparty;
    use crate::core::ics03_connection::version::get_compatible_versions;
    use crate::core::ics04_channel::channel::{ChannelEnd, Counterparty, Order, State};
    use crate::core::ics04_channel::msgs::chan_open_confirm::test_util::get_dummy_raw_msg_chan_open_confirm;
    use crate::core::ics04_channel::msgs::chan_open_confirm::MsgChannelOpenConfirm;
    use crate::core::ics04_channel::Version;
    use crate::core::ics24_host::identifier::ChannelId;
    use crate::core::ics24_host::identifier::{ClientId, ConnectionId};
    use crate::core::router::ModuleId;
    use crate::core::router::Router;
    use crate::core::timestamp::ZERO_DURATION;
    use crate::Height;

    use crate::mock::client_state::client_type as mock_client_type;
    use crate::mock::context::MockContext;
    use crate::mock::router::MockRouter;
    use crate::{applications::transfer::MODULE_ID_STR, test_utils::DummyTransferModule};

    pub struct Fixture {
        pub context: MockContext,
        pub router: MockRouter,
        pub module_id: ModuleId,
        pub msg: MsgChannelOpenConfirm,
        pub client_id_on_b: ClientId,
        pub conn_id_on_b: ConnectionId,
        pub conn_end_on_b: ConnectionEnd,
        pub chan_end_on_b: ChannelEnd,
        pub proof_height: u64,
    }

    #[fixture]
    fn fixture() -> Fixture {
        let proof_height = 10;
        let context = MockContext::default();

        let module_id: ModuleId = ModuleId::new(MODULE_ID_STR.to_string());
        let mut router = MockRouter::default();
        router
            .add_route(module_id.clone(), DummyTransferModule::new())
            .unwrap();

        let client_id_on_b = ClientId::new(mock_client_type(), 45).unwrap();
        let conn_id_on_b = ConnectionId::new(2);
        let conn_end_on_b = ConnectionEnd::new(
            ConnectionState::Open,
            client_id_on_b.clone(),
            ConnectionCounterparty::try_from(get_dummy_raw_counterparty(Some(0))).unwrap(),
            get_compatible_versions(),
            ZERO_DURATION,
        )
        .unwrap();

        let msg =
            MsgChannelOpenConfirm::try_from(get_dummy_raw_msg_chan_open_confirm(proof_height))
                .unwrap();

        let chan_end_on_b = ChannelEnd::new(
            State::TryOpen,
            Order::Unordered,
            Counterparty::new(msg.port_id_on_b.clone(), Some(ChannelId::default())),
            vec![conn_id_on_b.clone()],
            Version::default(),
        )
        .unwrap();

        Fixture {
            context,
            router,
            module_id,
            msg,
            client_id_on_b,
            conn_id_on_b,
            conn_end_on_b,
            chan_end_on_b,
            proof_height,
        }
    }

    #[rstest]
    fn chan_open_confirm_fail_no_channel(fixture: Fixture) {
        let Fixture {
            context,
            msg,
            client_id_on_b,
            conn_id_on_b,
            conn_end_on_b,
            proof_height,
            ..
        } = fixture;
        let context = context
            .with_client(&client_id_on_b, Height::new(0, proof_height).unwrap())
            .with_connection(conn_id_on_b, conn_end_on_b);

        let res = validate(&context, &msg);

        assert!(
            res.is_err(),
            "Validation fails because no channel exists in the context"
        )
    }

    #[rstest]
    fn chan_open_confirm_fail_channel_wrong_state(fixture: Fixture) {
        let Fixture {
            context,
            msg,
            client_id_on_b,
            conn_id_on_b,
            conn_end_on_b,
            proof_height,
            ..
        } = fixture;

        let wrong_chan_end = ChannelEnd::new(
            State::Init,
            Order::Unordered,
            Counterparty::new(msg.port_id_on_b.clone(), Some(ChannelId::default())),
            vec![conn_id_on_b.clone()],
            Version::default(),
        )
        .unwrap();
        let context = context
            .with_client(&client_id_on_b, Height::new(0, proof_height).unwrap())
            .with_connection(conn_id_on_b, conn_end_on_b)
            .with_channel(
                msg.port_id_on_b.clone(),
                ChannelId::default(),
                wrong_chan_end,
            );

        let res = validate(&context, &msg);

        assert!(
            res.is_err(),
            "Validation fails because channel is in the wrong state"
        )
    }

    #[rstest]
    fn chan_open_confirm_validate_happy_path(fixture: Fixture) {
        let Fixture {
            context,
            msg,
            client_id_on_b,
            conn_id_on_b,
            conn_end_on_b,
            chan_end_on_b,
            proof_height,
            ..
        } = fixture;

        let context = context
            .with_client(&client_id_on_b, Height::new(0, proof_height).unwrap())
            .with_connection(conn_id_on_b, conn_end_on_b)
            .with_channel(
                msg.port_id_on_b.clone(),
                ChannelId::default(),
                chan_end_on_b,
            );

        let res = validate(&context, &msg);

        assert!(res.is_ok(), "Validation happy path")
    }

    #[rstest]
    fn chan_open_confirm_execute_happy_path(fixture: Fixture) {
        let Fixture {
            context,
            mut router,
            module_id,
            msg,
            client_id_on_b,
            conn_id_on_b,
            conn_end_on_b,
            chan_end_on_b,
            proof_height,
            ..
        } = fixture;

        let mut context = context
            .with_client(&client_id_on_b, Height::new(0, proof_height).unwrap())
            .with_connection(conn_id_on_b, conn_end_on_b)
            .with_channel(
                msg.port_id_on_b.clone(),
                ChannelId::default(),
                chan_end_on_b,
            );

        let module = router.get_route_mut(&module_id).unwrap();
        let res = chan_open_confirm_execute(&mut context, module, msg);

        assert!(res.is_ok(), "Execution happy path");

        assert_eq!(context.events.len(), 2);
        assert!(matches!(
            context.events[0],
            IbcEvent::Message(MessageEvent::Channel)
        ));
        assert!(matches!(context.events[1], IbcEvent::OpenConfirmChannel(_)));
    }
}
