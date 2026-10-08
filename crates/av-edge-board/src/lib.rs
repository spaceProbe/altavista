//! The board's edge service (question 242 (b)): `altavista.v1.LockstepService` for the
//! kernel, lockstep-local v1 to the flight software over the board's link.
//!
//! `av-lockstep-shim` fronts a flight-software process over a Unix socket; this crate fronts
//! the board's hardware link (a serial line, or a UDP endpoint; `BoardBinding.port_devices`)
//! with the same `PeerLink`, the same framing, and the same `ShimService`. The additions are
//! the two transports ([`serial`], [`udp`]), the [`link`] that picks one, and the Bind-time
//! board check ([`service`]), and the board I/O log ([`iolog`], [`timed`]; the log itself is
//! `av_edge::board_log`). The pure half (spec parsing, typed errors, the link's config
//! hash) is `av_edge::board`. The binary is `av-edge-board`; the crate README gives the
//! command line.
//!
//! Everything here is proven against stand-ins only (a pseudo-terminal, loopback UDP, fake
//! guests); no board has been involved.
#![cfg(unix)]

pub mod iolog;
pub mod link;
pub mod serial;
pub mod service;
pub mod timed;
pub mod udp;
