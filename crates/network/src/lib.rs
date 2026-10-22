//! Authenticated peer networking for watch-together sessions.

mod invitation;
mod embedded;
mod signaling;
mod upnp;
mod watch;

pub use invitation::{InvitationError, InvitationVerifier, JoinInvitation};
pub use embedded::EmbeddedSignalServer;
pub use upnp::RouterPortLease;
pub use signaling::{
    ClientFrame, ServerFrame, SignalClient, SignalError, SignalHub, router as signaling_router,
};
pub use watch::{GuestSession, HostSession, WatchError};
