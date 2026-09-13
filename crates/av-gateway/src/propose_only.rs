//! D4: `propose_command` must be structurally unable to reach any state but `PROPOSED` --
//! not "we only call Propose", structurally. [`ProposeOnlyAuthority`] is that structure: its
//! ONLY public method is [`ProposeOnlyAuthority::propose`]. Its one field, the generated
//! `CommandAuthorityServiceClient` (which has `check`/`authorize`/`dispatch`/`ack`/
//! `expire`/`fail` in addition to `propose`), is private to this module and this type never
//! exposes it -- no accessor, no `Deref`/`AsRef` impl, no `pub` field. Both [`crate::mcp`]
//! and [`crate::gateway`] hold, at most, an `Arc<ProposeOnlyAuthority>`; neither ever gets a
//! reference to the raw client, so a crafted call reaching for `authorize` (or any other
//! non-`propose` method) has no path to it at the Rust type level, let alone the wire --
//! see `crates/av-gateway/tests/propose_only.rs` for the several shapes this is proven
//! against (a `tools/call` naming `"authorize"`, a raw JSON-RPC `"method": "authorize"`, and
//! a `propose_command` call whose own proposal tries to smuggle a non-`PROPOSED` state).
//!
//! Question 53 stands: `av_command::state::propose` already refuses a non-empty
//! `envelope_id` in code (not only in policy) -- this module does not re-implement that
//! check; it dials the real `CommandAuthorityService.Propose` RPC, which calls `state::
//! propose` for real, so the refusal this module reports for an envelope is the SAME
//! refusal the real state machine produced, end to end through this tool.

use av_cdm::pb::{Command, CommandProposal, ProposeRequest};
use tonic::transport::Channel;
use tonic::Status;

use crate::counters::Counted;
use crate::pb::command_authority_service_client::CommandAuthorityServiceClient;

/// Every way [`ProposeOnlyAuthority::propose`] can refuse. Classified from the real
/// [`tonic::Status`] the `CommandAuthorityService.Propose` RPC returned -- see
/// [`classify_status`] for the exact, pinned substrings this reads (the underlying
/// `Display` text of `av_command::state::CommandError::{EnvelopeNotAllowed,AlreadyStarted}`,
/// a text this same repository controls, not an external dependency's undocumented prose --
/// `crates/av-gateway/tests/propose_only.rs`'s `classify_status_pins_the_real_server_error_
/// text_for_envelope_and_already_started` test fails loudly if either ever drifts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposeRefusal {
    /// Question 53: the proposal's own `Command.envelope_id` was non-empty.
    EnvelopeNotAllowed { detail: String },
    /// The proposal's `Command` already had a `state`/`transitions` populated -- `Propose`
    /// only ever starts a fresh command.
    AlreadyStarted { detail: String },
    /// Any other `INVALID_ARGUMENT` (a malformed request the server itself refused, e.g. an
    /// empty `command.id`/`entity_id`).
    InvalidArgument { detail: String },
    /// Question 209(a)/D3/D6: the real `Propose` RPC's own automatic check denied this
    /// proposal by policy -- classified from `PERMISSION_DENIED`
    /// (`av_command::service::ServiceError::PolicyDenied`'s own mapping), never from
    /// message-text sniffing the way the two `INVALID_ARGUMENT` variants above are (a
    /// `tonic::Code` is a stable, typed signal `av_command` itself commits to; matching on
    /// it is strictly more robust than matching this module's other two variants' own
    /// message substrings, which is why this one does not join them). `detail` is the typed
    /// error's own `Display` text verbatim -- it already names the decision id and the deny
    /// reasons (`ServiceError::PolicyDenied`'s own `Display` impl).
    PolicyDenied { detail: String },
    /// The RPC itself failed (connection refused, deadline, or any other non-`INVALID_
    /// ARGUMENT`/non-`PERMISSION_DENIED` status) rather than being refused by the state
    /// machine or by policy.
    Transport { detail: String },
}

impl Counted for ProposeRefusal {
    fn code(&self) -> &'static str {
        match self {
            ProposeRefusal::EnvelopeNotAllowed { .. } => "propose_envelope_not_allowed",
            ProposeRefusal::AlreadyStarted { .. } => "propose_already_started",
            ProposeRefusal::InvalidArgument { .. } => "propose_invalid_argument",
            ProposeRefusal::PolicyDenied { .. } => "propose_policy_denied",
            ProposeRefusal::Transport { .. } => "propose_transport_error",
        }
    }
}

impl std::fmt::Display for ProposeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposeRefusal::EnvelopeNotAllowed { detail }
            | ProposeRefusal::AlreadyStarted { detail }
            | ProposeRefusal::InvalidArgument { detail }
            | ProposeRefusal::PolicyDenied { detail }
            | ProposeRefusal::Transport { detail } => write!(f, "{detail}"),
        }
    }
}

/// Classifies a real `Propose` RPC's returned [`Status`] into a [`ProposeRefusal`]. Pinned
/// against `av_command::state::CommandError`'s own `Display` text (see this module's own
/// doc and the "pins the real server error text" test) -- both `INVALID_ARGUMENT` refusals
/// map to that one code (`crates/av-command/src/service.rs`'s own `to_status` doc: "same
/// reasoning"), so the message text is the only way to tell them apart. Question 209(a)/D6:
/// a policy denial is its own, distinct `tonic::Code` (`PERMISSION_DENIED`) -- classified on
/// the code alone, never on message text, since the code itself is already the stable,
/// typed signal.
fn classify_status(status: &Status) -> ProposeRefusal {
    let detail = status.message().to_string();
    if status.code() == tonic::Code::InvalidArgument {
        if detail.contains("propose refuses a non-empty envelope_id") {
            return ProposeRefusal::EnvelopeNotAllowed { detail };
        }
        if detail.contains("propose called on a command already at") {
            return ProposeRefusal::AlreadyStarted { detail };
        }
        return ProposeRefusal::InvalidArgument { detail };
    }
    if status.code() == tonic::Code::PermissionDenied {
        return ProposeRefusal::PolicyDenied { detail };
    }
    ProposeRefusal::Transport { detail: format!("{}: {}", status.code(), detail) }
}

/// The one, structurally propose-only seam into the command authority (D4). See the module
/// doc: `client` is the only field, private, and never exposed.
pub struct ProposeOnlyAuthority {
    client: CommandAuthorityServiceClient<Channel>,
}

impl ProposeOnlyAuthority {
    /// Dials `endpoint` (e.g. `"http://127.0.0.1:50123"`) over the same plaintext-on-
    /// loopback tonic stack every other service in this workspace uses (`tls`/`channel`
    /// only, never `tls*` -- ADR-004).
    pub async fn connect(endpoint: String) -> Result<Self, ProposeRefusal> {
        let channel = tonic::transport::Endpoint::from_shared(endpoint)
            .map_err(|e| ProposeRefusal::Transport { detail: e.to_string() })?
            .connect()
            .await
            .map_err(|e| ProposeRefusal::Transport { detail: e.to_string() })?;
        Ok(Self::from_channel(channel))
    }

    /// Builds directly from an already-connected [`Channel`] -- this crate's own test
    /// suite's way of dialing a real, in-process `CommandAuthorityServiceImpl` over a real
    /// loopback socket, mirroring `crates/av-command/tests/grpc_service.rs`'s own pattern.
    pub fn from_channel(channel: Channel) -> Self {
        Self { client: CommandAuthorityServiceClient::new(channel) }
    }

    /// The ONLY public method on this type (D4). Dials the real `CommandAuthorityService.
    /// Propose` RPC -- so a non-empty `envelope_id` or an already-started `Command` is
    /// refused by the SAME `av_command::state::propose` the real service enforces, not a
    /// second, local copy of that check.
    pub async fn propose(&self, proposal: CommandProposal, principal: String) -> Result<Command, ProposeRefusal> {
        let mut client = self.client.clone();
        let response = client
            .propose(ProposeRequest { proposal: Some(proposal), principal })
            .await
            .map_err(|status| classify_status(&status))?;
        response
            .into_inner()
            .command
            .ok_or_else(|| ProposeRefusal::Transport { detail: "Propose RPC succeeded but returned no command".to_string() })
    }
}
