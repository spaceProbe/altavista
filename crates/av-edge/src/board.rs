//! The pure half of the board's link on the edge side (question 242 (b)): parsing and
//! validating `BoardBinding.port_devices` specs, with no I/O, no clock and no environment
//! reads. Its I/O half is the `av-edge-board` crate (`crates/av-edge-board`), the way the
//! edge plugin's pure half is [`crate::plugin`] and its binary lives elsewhere.
//!
//! # Device specs
//!
//! `BoardBinding.port_devices` maps a port name to a device spec string
//! (`proto/altavista/v1/system.proto`). Two spellings exist, parsed by
//! [`parse_port_device`] into a [`PortDevice`]:
//!
//! - **Serial**: `/dev/<name>@<baud>`, e.g. `/dev/ttyUSB0@115200`. The path is absolute and
//!   lies under `/dev/` (no `.`, `..` or empty components, no whitespace, control
//!   characters or `@`); the baud is a positive decimal integer that fits `u32`, digits
//!   only (no sign, no space, no exponent). **The line framing is fixed at 8N1** (eight
//!   data bits, no parity, one stop bit, no flow control): the spec has no syntax for
//!   anything else, and the cFS image's UART (`io_lockstep_app.c`) is opened 8N1 raw. A
//!   leading-zero baud (`0115200`) is refused rather than normalised, so one device has
//!   one spelling.
//! - **UDP**: `udp://<host>:<port>` where `<host>` is an IPv4 literal, a bracketed IPv6
//!   literal (`udp://[::1]:5000`) or an RFC 1123 hostname, and `<port>` is `1..=65535`
//!   (digits only). Nothing may follow the port (no path, query or fragment). The scheme
//!   is lower case.
//!
//! Every malformed spec is a [`PortDeviceSpecError`] variant naming what is wrong; there is
//! no default and no panic.
//!
//! # Canonical form
//!
//! [`PortDevice::canonical`] (also `Display`) is the one spelling of a device: serial
//! `<path>@<baud>` (decimal, no leading zeros), UDP `udp://<host>:<port>` with an IPv4
//! address as `Ipv4Addr` prints it, an IPv6 address compressed as `Ipv6Addr` prints it in
//! brackets, and a hostname in lower case. [`parse_port_device`] of a canonical string
//! returns the same device, and two specs that differ only in spelling (`udp://HOST:5000`
//! and `udp://host:5000`) have equal canonical forms. This is the form the Bind-time check
//! compares ([`BIND_PARAM_PORT_DEVICE`]).
//!
//! # One link per board instance
//!
//! A board instance has one physical link, over which every port is multiplexed by
//! lockstep-local (the manager's decision for the hardware-in-the-loop round).
//! [`BoardLink::from_binding`] therefore requires every entry of `port_devices` to name the
//! same device (after canonicalisation): a mixed map, such as the proto comment's `"tm"`
//! serial and `"tc"` UDP example, is [`BoardLinkError::MixedDevices`], naming the ports of
//! each device. [`BoardLink::from_binding_checked`] additionally checks the map against the
//! ports the system definition declares: ports missing from the map and ports the system
//! does not declare are each a typed refusal.
//!
//! # Hash
//!
//! [`BoardLink::config_hash`] is the SHA-256 (via `openssl`, as
//! [`crate::plugin::PluginConfig::config_hash`] does; never `sha2`) of the link's canonical
//! JSON: edge node id, canonical device, sorted port names. `power_control` is not part of
//! the link and is not hashed here; it has its own grammar, below.
//!
//! # Power control (question 242 (c))
//!
//! `BoardBinding.power_control` is the **edge node's** power control channel. It is run by the
//! board's edge service (`av-edge-board`), never by the kernel: the kernel asks the service,
//! at the address it already dials for the board, over `altavista.v1.BoardEdgeService`
//! (`proto/altavista/v1/board.proto`). [`parse_power_control`] parses the string into a
//! [`PowerControl`]:
//!
//! - **empty**: no channel ([`PowerControl::None`]).
//! - **`cmd:<absolute path>`**: run that executable on the edge node ([`PowerControl::Cmd`]).
//!   The path is absolute, normalised (no empty, `.` or `..` component, no trailing `/`),
//!   without whitespace or control characters and at most 4096 bytes: it names one
//!   executable and there is no shell and no argument syntax. The edge service runs it as
//!   `<path> power-cycle --edge-node-id <id> --instance <name> --fault-id <id> --tai-ns <n>`
//!   ([`power_cycle_argv`]); exit status 0 is success. The scheme is lower case.
//! - **`gpio://...`** (any string starting `gpio:`): **reserved**, not implemented this round.
//!   It is recognised so that it can be named in the refusal, [`PowerControlSpecError::Reserved`],
//!   and is never silently accepted or ignored.
//! - **anything else**: a typed [`PowerControlSpecError`].
//!
//! [`PowerControl::canonical`] is the one spelling (`""` or `cmd:<path>`); the kernel's binding
//! and the edge service compare canonical forms.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use crate::hash::hex_encode;
use crate::pb;

/// `LockstepBindRequest.parameters` key carrying the edge node the kernel believes it is
/// binding (the next task makes the kernel set it; the `av-edge-board` service checks it).
pub const BIND_PARAM_EDGE_NODE_ID: &str = "board.edge_node_id";
/// `LockstepBindRequest.parameters` key carrying the board link's device spec in its
/// canonical form ([`PortDevice::canonical`]).
pub const BIND_PARAM_PORT_DEVICE: &str = "board.port_device";

const SERIAL_PREFIX: &str = "/dev/";
const UDP_PREFIX: &str = "udp://";
const MAX_HOSTNAME_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;

/// One physical device a board's ports are carried over.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PortDevice {
    /// A serial line, 8N1 raw, no flow control (see the module doc).
    Serial { path: String, baud: u32 },
    /// A UDP peer. `host` is an IPv4 literal, an IPv6 literal without brackets, or a
    /// lower-case hostname.
    Udp { host: String, port: u16 },
}

impl PortDevice {
    /// The one spelling of this device (see the module doc, "Canonical form").
    pub fn canonical(&self) -> String {
        match self {
            PortDevice::Serial { path, baud } => format!("{path}@{baud}"),
            PortDevice::Udp { host, port } if host.contains(':') => format!("{UDP_PREFIX}[{host}]:{port}"),
            PortDevice::Udp { host, port } => format!("{UDP_PREFIX}{host}:{port}"),
        }
    }
}

impl fmt::Display for PortDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

impl FromStr for PortDevice {
    type Err = PortDeviceSpecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_port_device(s)
    }
}

/// Everything wrong a `port_devices` spec string can be. Each variant names the offending
/// part; `spec` is always the whole string as given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortDeviceSpecError {
    #[error("empty port device spec")]
    Empty,
    #[error("unknown scheme {scheme:?} in port device spec {spec:?}: only `udp://host:port` and `/dev/<name>@<baud>` are supported (the scheme is lower case)")]
    UnknownScheme { spec: String, scheme: String },
    #[error("serial port device spec {spec:?} has no `@<baud>` suffix")]
    MissingBaud { spec: String },
    #[error("serial port device spec {spec:?} has an empty baud after `@`")]
    EmptyBaud { spec: String },
    #[error("baud {baud:?} in port device spec {spec:?} is not a positive decimal integer (digits only, no sign, no leading zero, at most {max})", max = u32::MAX)]
    BadBaud { spec: String, baud: String },
    #[error("baud is zero in port device spec {spec:?}")]
    ZeroBaud { spec: String },
    #[error("serial path {path:?} is relative; an absolute `/dev/...` path is required")]
    RelativePath { path: String },
    #[error("serial path {path:?} is outside /dev/")]
    PathOutsideDev { path: String },
    #[error("serial path {path:?} has an empty, `.` or `..` component after /dev/")]
    PathNotNormalized { path: String },
    #[error("serial path {path:?} contains the character {ch:?}, which is not allowed in a device path")]
    InvalidPathChar { path: String, ch: char },
    #[error("udp port device spec {spec:?} has no host")]
    MissingHost { spec: String },
    #[error("bad host {host:?} in port device spec {spec:?}: {reason}")]
    BadHost { spec: String, host: String, reason: &'static str },
    #[error("udp port device spec {spec:?} has no `:<port>`")]
    MissingPort { spec: String },
    #[error("port {port:?} in port device spec {spec:?} is not a decimal integer in 1..=65535")]
    BadPort { spec: String, port: String },
    #[error("port is zero in port device spec {spec:?}")]
    ZeroPort { spec: String },
    #[error("trailing junk {junk:?} after the port in port device spec {spec:?}")]
    TrailingJunk { spec: String, junk: String },
}

/// Parse one `port_devices` value. See the module doc for the grammar; every failure is a
/// [`PortDeviceSpecError`].
pub fn parse_port_device(spec: &str) -> Result<PortDevice, PortDeviceSpecError> {
    if spec.is_empty() {
        return Err(PortDeviceSpecError::Empty);
    }
    if let Some(rest) = spec.strip_prefix(UDP_PREFIX) {
        return parse_udp(spec, rest);
    }
    if let Some(i) = spec.find("://") {
        let scheme = &spec[..i];
        if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return Err(PortDeviceSpecError::UnknownScheme { spec: spec.to_string(), scheme: scheme.to_string() });
        }
    }
    parse_serial(spec)
}

fn parse_serial(spec: &str) -> Result<PortDevice, PortDeviceSpecError> {
    let (path, baud_text) = spec.rsplit_once('@').ok_or_else(|| PortDeviceSpecError::MissingBaud { spec: spec.to_string() })?;
    if !path.starts_with('/') {
        return Err(PortDeviceSpecError::RelativePath { path: path.to_string() });
    }
    let Some(name) = path.strip_prefix(SERIAL_PREFIX) else {
        return Err(PortDeviceSpecError::PathOutsideDev { path: path.to_string() });
    };
    if let Some(ch) = path.chars().find(|c| c.is_control() || c.is_whitespace() || *c == '@') {
        return Err(PortDeviceSpecError::InvalidPathChar { path: path.to_string(), ch });
    }
    if name.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err(PortDeviceSpecError::PathNotNormalized { path: path.to_string() });
    }
    if baud_text.is_empty() {
        return Err(PortDeviceSpecError::EmptyBaud { spec: spec.to_string() });
    }
    let bad_baud = || PortDeviceSpecError::BadBaud { spec: spec.to_string(), baud: baud_text.to_string() };
    if !baud_text.bytes().all(|b| b.is_ascii_digit()) || (baud_text.len() > 1 && baud_text.starts_with('0')) {
        return Err(bad_baud());
    }
    let baud: u32 = baud_text.parse().map_err(|_| bad_baud())?;
    if baud == 0 {
        return Err(PortDeviceSpecError::ZeroBaud { spec: spec.to_string() });
    }
    Ok(PortDevice::Serial { path: path.to_string(), baud })
}

fn parse_udp(spec: &str, rest: &str) -> Result<PortDevice, PortDeviceSpecError> {
    if rest.is_empty() {
        return Err(PortDeviceSpecError::MissingHost { spec: spec.to_string() });
    }
    if let Some(i) = rest.find(['/', '?', '#']) {
        if i == 0 {
            return Err(PortDeviceSpecError::MissingHost { spec: spec.to_string() });
        }
        return Err(PortDeviceSpecError::TrailingJunk { spec: spec.to_string(), junk: rest[i..].to_string() });
    }
    let bad_host = |host: &str, reason: &'static str| PortDeviceSpecError::BadHost { spec: spec.to_string(), host: host.to_string(), reason };

    let (host, port_text): (String, &str) = if let Some(bracketed) = rest.strip_prefix('[') {
        let close = bracketed.find(']').ok_or_else(|| bad_host(rest, "unterminated `[` in an IPv6 literal"))?;
        let inner = &bracketed[..close];
        let addr: Ipv6Addr = inner.parse().map_err(|_| bad_host(inner, "not a valid IPv6 address (zone ids are not supported)"))?;
        let after = &bracketed[close + 1..];
        match after.strip_prefix(':') {
            Some(p) => (addr.to_string(), p),
            None if after.is_empty() => return Err(PortDeviceSpecError::MissingPort { spec: spec.to_string() }),
            None => return Err(PortDeviceSpecError::TrailingJunk { spec: spec.to_string(), junk: after.to_string() }),
        }
    } else {
        let (h, p) = rest.rsplit_once(':').ok_or_else(|| PortDeviceSpecError::MissingPort { spec: spec.to_string() })?;
        if h.contains(':') {
            return Err(bad_host(h, "contains `:`; an IPv6 literal must be bracketed, as `udp://[::1]:5000`"));
        }
        (validate_host(h).map_err(|reason| bad_host(h, reason))?, p)
    };

    if port_text.is_empty() {
        return Err(PortDeviceSpecError::MissingPort { spec: spec.to_string() });
    }
    let bad_port = || PortDeviceSpecError::BadPort { spec: spec.to_string(), port: port_text.to_string() };
    if !port_text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad_port());
    }
    let port: u16 = port_text.parse().map_err(|_| bad_port())?;
    if port == 0 {
        return Err(PortDeviceSpecError::ZeroPort { spec: spec.to_string() });
    }
    Ok(PortDevice::Udp { host, port })
}

/// An IPv4 literal (canonicalised) or an RFC 1123 hostname (lower-cased). `Err` is the
/// reason, for [`PortDeviceSpecError::BadHost`].
fn validate_host(host: &str) -> Result<String, &'static str> {
    if host.is_empty() {
        return Err("empty host");
    }
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return Ok(v4.to_string());
    }
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err("looks like an IPv4 address but is not a valid one");
    }
    if host.len() > MAX_HOSTNAME_LEN {
        return Err("hostname longer than 253 characters");
    }
    for label in host.split('.') {
        if label.is_empty() {
            return Err("empty hostname label (a leading, trailing or doubled `.`)");
        }
        if label.len() > MAX_LABEL_LEN {
            return Err("hostname label longer than 63 characters");
        }
        if !label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("hostname labels may contain only ASCII letters, digits and `-`");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("a hostname label may not start or end with `-`");
        }
    }
    Ok(host.to_ascii_lowercase())
}

/// Everything wrong a whole [`pb::BoardBinding`] can be, above the single-spec level.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoardLinkError {
    #[error("BoardBinding.edge_node_id is empty")]
    EmptyEdgeNodeId,
    #[error("BoardBinding.edge_node_id {id:?} contains the character {ch:?} (whitespace and control characters are not allowed)")]
    InvalidEdgeNodeId { id: String, ch: char },
    #[error("BoardBinding.port_devices is empty: a board instance needs at least one port")]
    NoPorts,
    #[error("BoardBinding.port_devices has an empty port name")]
    EmptyPortName,
    #[error("port {port:?}: {source}")]
    PortDevice { port: String, source: PortDeviceSpecError },
    #[error("a board instance has one link, but its ports name different devices: {}", describe_groups(.groups))]
    MixedDevices { groups: Vec<(String, Vec<String>)> },
    #[error("BoardBinding.port_devices lacks the declared port(s) {ports:?}")]
    MissingPorts { ports: Vec<String> },
    #[error("BoardBinding.port_devices names port(s) {ports:?} that the system definition does not declare")]
    ExtraPorts { ports: Vec<String> },
}

fn describe_groups(groups: &[(String, Vec<String>)]) -> String {
    groups.iter().map(|(device, ports)| format!("{device} <- ports {ports:?}")).collect::<Vec<_>>().join("; ")
}

/// A board instance's one validated link: the edge node that hosts it, the single device
/// every port is multiplexed over, and the port names it carries (sorted; empty when built
/// by [`BoardLink::single`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardLink {
    edge_node_id: String,
    device: PortDevice,
    ports: Vec<String>,
}

#[derive(serde::Serialize)]
struct CanonicalLink<'a> {
    edge_node_id: &'a str,
    device: String,
    ports: &'a [String],
}

fn check_edge_node_id(id: &str) -> Result<(), BoardLinkError> {
    if id.is_empty() {
        return Err(BoardLinkError::EmptyEdgeNodeId);
    }
    if let Some(ch) = id.chars().find(|c| c.is_control() || c.is_whitespace()) {
        return Err(BoardLinkError::InvalidEdgeNodeId { id: id.to_string(), ch });
    }
    Ok(())
}

impl BoardLink {
    /// A link from an edge node id and one device, with no port list (what the
    /// `av-edge-board` command line names; the ports are the kernel's, per Bind).
    pub fn single(edge_node_id: &str, device: PortDevice) -> Result<Self, BoardLinkError> {
        check_edge_node_id(edge_node_id)?;
        Ok(Self { edge_node_id: edge_node_id.to_string(), device, ports: Vec::new() })
    }

    /// Validate a whole `BoardBinding` (see the module doc, "One link per board instance").
    pub fn from_binding(binding: &pb::BoardBinding) -> Result<Self, BoardLinkError> {
        check_edge_node_id(&binding.edge_node_id)?;
        if binding.port_devices.is_empty() {
            return Err(BoardLinkError::NoPorts);
        }
        let mut by_device: BTreeMap<String, (PortDevice, Vec<String>)> = BTreeMap::new();
        for (port, spec) in &binding.port_devices {
            if port.is_empty() {
                return Err(BoardLinkError::EmptyPortName);
            }
            let device = parse_port_device(spec).map_err(|source| BoardLinkError::PortDevice { port: port.clone(), source })?;
            by_device.entry(device.canonical()).or_insert_with(|| (device, Vec::new())).1.push(port.clone());
        }
        if by_device.len() > 1 {
            return Err(BoardLinkError::MixedDevices { groups: by_device.into_iter().map(|(canonical, (_, ports))| (canonical, ports)).collect() });
        }
        let (device, ports) = by_device.into_values().next().expect("port_devices is non-empty, so there is one group");
        Ok(Self { edge_node_id: binding.edge_node_id.clone(), device, ports })
    }

    /// [`BoardLink::from_binding`], plus a completeness check against the ports the system
    /// definition declares: a declared port with no entry is
    /// [`BoardLinkError::MissingPorts`], an entry for an undeclared port is
    /// [`BoardLinkError::ExtraPorts`] (missing is reported first when both occur).
    pub fn from_binding_checked<I, S>(binding: &pb::BoardBinding, declared_ports: I) -> Result<Self, BoardLinkError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let link = Self::from_binding(binding)?;
        let declared_owned: Vec<String> = declared_ports.into_iter().map(|s| s.as_ref().to_string()).collect();
        let declared: BTreeSet<&str> = declared_owned.iter().map(String::as_str).collect();
        let present: BTreeSet<&str> = link.ports.iter().map(String::as_str).collect();
        let missing: Vec<String> = declared.difference(&present).map(|s| s.to_string()).collect();
        if !missing.is_empty() {
            return Err(BoardLinkError::MissingPorts { ports: missing });
        }
        let extra: Vec<String> = present.difference(&declared).map(|s| s.to_string()).collect();
        if !extra.is_empty() {
            return Err(BoardLinkError::ExtraPorts { ports: extra });
        }
        Ok(link)
    }

    pub fn edge_node_id(&self) -> &str {
        &self.edge_node_id
    }

    pub fn device(&self) -> &PortDevice {
        &self.device
    }

    /// The port names this link carries, sorted; empty for a [`BoardLink::single`] link.
    pub fn ports(&self) -> &[String] {
        &self.ports
    }

    /// `edge_node_id=<id>;device=<canonical device>;ports=<comma-joined sorted names>`.
    pub fn canonical(&self) -> String {
        format!("edge_node_id={};device={};ports={}", self.edge_node_id, self.device.canonical(), self.ports.join(","))
    }

    /// SHA-256 over the link's canonical JSON (edge node id, canonical device, sorted
    /// ports), via `openssl::sha::sha256`. Stable for equal links; any change to the node,
    /// the device (including its baud) or the port set changes it.
    pub fn config_hash(&self) -> [u8; 32] {
        let canonical = CanonicalLink { edge_node_id: &self.edge_node_id, device: self.device.canonical(), ports: &self.ports };
        let json = serde_json::to_vec(&canonical).expect("CanonicalLink serialises to JSON: plain strings only");
        openssl::sha::sha256(&json)
    }

    pub fn config_hash_hex(&self) -> String {
        hex_encode(&self.config_hash())
    }

    /// The two `LockstepBindRequest.parameters` entries a kernel binding this link sets and
    /// the `av-edge-board` service checks ([`BIND_PARAM_EDGE_NODE_ID`],
    /// [`BIND_PARAM_PORT_DEVICE`]).
    pub fn bind_parameters(&self) -> BTreeMap<String, String> {
        BTreeMap::from([(BIND_PARAM_EDGE_NODE_ID.to_string(), self.edge_node_id.clone()), (BIND_PARAM_PORT_DEVICE.to_string(), self.device.canonical())])
    }
}

// ---------------------------------------------------------------------------------------
// Power control (question 242 (c))
// ---------------------------------------------------------------------------------------

const CMD_PREFIX: &str = "cmd:";
const GPIO_PREFIX: &str = "gpio:";
/// Longest `cmd:` path accepted (POSIX `PATH_MAX`).
pub const MAX_POWER_CONTROL_PATH_LEN: usize = 4096;

/// The edge node's power control channel, parsed from `BoardBinding.power_control`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PowerControl {
    /// No channel: a power-cycle fault on the board is refused at load.
    None,
    /// Run this executable (absolute, normalised path) on the edge node.
    Cmd { path: String },
}

impl PowerControl {
    /// The one spelling of this channel: `""` for none, `cmd:<path>` otherwise.
    pub fn canonical(&self) -> String {
        match self {
            PowerControl::None => String::new(),
            PowerControl::Cmd { path } => format!("{CMD_PREFIX}{path}"),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, PowerControl::None)
    }
}

impl fmt::Display for PowerControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

impl FromStr for PowerControl {
    type Err = PowerControlSpecError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_power_control(s)
    }
}

/// Everything wrong a `power_control` string can be. `spec` is always the whole string as given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PowerControlSpecError {
    /// `gpio://...` is reserved for a later round: named, then refused.
    #[error("power control channel {spec:?} uses the reserved `gpio:` scheme, which is not implemented yet: only `cmd:<absolute path>` is supported")]
    Reserved { spec: String },
    #[error("unknown scheme {scheme:?} in power control channel {spec:?}: only `cmd:<absolute path>` is supported (`gpio://...` is reserved; the scheme is lower case)")]
    UnknownScheme { spec: String, scheme: String },
    #[error("power control channel {spec:?} is not `cmd:<absolute path>` (nor empty for none)")]
    NotAChannel { spec: String },
    #[error("`cmd:` power control channel has no path")]
    MissingPath,
    #[error("`cmd:` path {path:?} is relative; an absolute path is required")]
    RelativePath { path: String },
    #[error("`cmd:` path {path:?} has an empty, `.` or `..` component, or a trailing `/`")]
    PathNotNormalized { path: String },
    #[error("`cmd:` path {path:?} contains the character {ch:?}, which is not allowed (no whitespace or control characters: the path is one executable, there is no shell and no arguments)")]
    InvalidPathChar { path: String, ch: char },
    #[error("`cmd:` path is {len} bytes, longer than the {MAX_POWER_CONTROL_PATH_LEN} allowed")]
    PathTooLong { len: usize },
}

/// Parse a `BoardBinding.power_control` string. See the module doc, "Power control".
pub fn parse_power_control(spec: &str) -> Result<PowerControl, PowerControlSpecError> {
    if spec.is_empty() {
        return Ok(PowerControl::None);
    }
    if let Some(path) = spec.strip_prefix(CMD_PREFIX) {
        return parse_cmd_path(path);
    }
    if spec.starts_with(GPIO_PREFIX) {
        return Err(PowerControlSpecError::Reserved { spec: spec.to_string() });
    }
    if let Some(i) = spec.find(':') {
        let scheme = &spec[..i];
        if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return Err(PowerControlSpecError::UnknownScheme { spec: spec.to_string(), scheme: scheme.to_string() });
        }
    }
    Err(PowerControlSpecError::NotAChannel { spec: spec.to_string() })
}

fn parse_cmd_path(path: &str) -> Result<PowerControl, PowerControlSpecError> {
    if path.is_empty() {
        return Err(PowerControlSpecError::MissingPath);
    }
    if path.len() > MAX_POWER_CONTROL_PATH_LEN {
        return Err(PowerControlSpecError::PathTooLong { len: path.len() });
    }
    if let Some(ch) = path.chars().find(|c| c.is_control() || c.is_whitespace()) {
        return Err(PowerControlSpecError::InvalidPathChar { path: path.to_string(), ch });
    }
    let Some(rest) = path.strip_prefix('/') else {
        return Err(PowerControlSpecError::RelativePath { path: path.to_string() });
    };
    if rest.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err(PowerControlSpecError::PathNotNormalized { path: path.to_string() });
    }
    Ok(PowerControl::Cmd { path: path.to_string() })
}

/// The argument vector (after the program) the edge service passes a `cmd:` channel:
/// `power-cycle --edge-node-id <id> --instance <name> --fault-id <id> --tai-ns <n>`. Pure, so
/// the contract is tested without running anything.
pub fn power_cycle_argv(edge_node_id: &str, instance: &str, fault_id: &str, tai_ns: i64) -> Vec<String> {
    ["power-cycle", "--edge-node-id", edge_node_id, "--instance", instance, "--fault-id", fault_id, "--tai-ns", &tai_ns.to_string()].iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial(path: &str, baud: u32) -> PortDevice {
        PortDevice::Serial { path: path.to_string(), baud }
    }

    fn udp(host: &str, port: u16) -> PortDevice {
        PortDevice::Udp { host: host.to_string(), port }
    }

    fn binding(node: &str, ports: &[(&str, &str)]) -> pb::BoardBinding {
        pb::BoardBinding { edge_node_id: node.to_string(), port_devices: ports.iter().map(|(p, d)| (p.to_string(), d.to_string())).collect(), power_control: String::new() }
    }

    #[test]
    fn valid_serial_and_udp_specs_parse() {
        assert_eq!(parse_port_device("/dev/ttyUSB0@115200"), Ok(serial("/dev/ttyUSB0", 115200)));
        assert_eq!(parse_port_device("/dev/cu.usbserial-1410@9600"), Ok(serial("/dev/cu.usbserial-1410", 9600)));
        assert_eq!(parse_port_device("/dev/pts/3@115200"), Ok(serial("/dev/pts/3", 115200)));
        assert_eq!(parse_port_device("udp://10.0.0.7:5000"), Ok(udp("10.0.0.7", 5000)));
        assert_eq!(parse_port_device("udp://[::1]:65535"), Ok(udp("::1", 65535)));
        assert_eq!(parse_port_device("udp://[2001:DB8:0:0::1]:1"), Ok(udp("2001:db8::1", 1)));
        assert_eq!(parse_port_device("udp://Board-1.Lab.example:5000"), Ok(udp("board-1.lab.example", 5000)));
        assert_eq!("udp://localhost:7".parse::<PortDevice>(), Ok(udp("localhost", 7)));
    }

    #[test]
    fn every_malformed_spec_is_a_typed_error() {
        use PortDeviceSpecError as E;
        let spec = |s: &str| s.to_string();
        let cases: Vec<(&str, PortDeviceSpecError)> = vec![
            ("", E::Empty),
            ("tcp://10.0.0.7:5000", E::UnknownScheme { spec: spec("tcp://10.0.0.7:5000"), scheme: spec("tcp") }),
            ("UDP://10.0.0.7:5000", E::UnknownScheme { spec: spec("UDP://10.0.0.7:5000"), scheme: spec("UDP") }),
            ("file:///dev/ttyUSB0@9600", E::UnknownScheme { spec: spec("file:///dev/ttyUSB0@9600"), scheme: spec("file") }),
            ("/dev/ttyUSB0", E::MissingBaud { spec: spec("/dev/ttyUSB0") }),
            ("/dev/ttyUSB0@", E::EmptyBaud { spec: spec("/dev/ttyUSB0@") }),
            ("/dev/ttyUSB0@fast", E::BadBaud { spec: spec("/dev/ttyUSB0@fast"), baud: spec("fast") }),
            ("/dev/ttyUSB0@-9600", E::BadBaud { spec: spec("/dev/ttyUSB0@-9600"), baud: spec("-9600") }),
            ("/dev/ttyUSB0@+9600", E::BadBaud { spec: spec("/dev/ttyUSB0@+9600"), baud: spec("+9600") }),
            ("/dev/ttyUSB0@9600 ", E::BadBaud { spec: spec("/dev/ttyUSB0@9600 "), baud: spec("9600 ") }),
            ("/dev/ttyUSB0@115200bps", E::BadBaud { spec: spec("/dev/ttyUSB0@115200bps"), baud: spec("115200bps") }),
            ("/dev/ttyUSB0@0115200", E::BadBaud { spec: spec("/dev/ttyUSB0@0115200"), baud: spec("0115200") }),
            ("/dev/ttyUSB0@4294967296", E::BadBaud { spec: spec("/dev/ttyUSB0@4294967296"), baud: spec("4294967296") }),
            ("/dev/ttyUSB0@0", E::ZeroBaud { spec: spec("/dev/ttyUSB0@0") }),
            ("ttyUSB0@115200", E::RelativePath { path: spec("ttyUSB0") }),
            ("dev/ttyUSB0@115200", E::RelativePath { path: spec("dev/ttyUSB0") }),
            ("./dev/ttyUSB0@115200", E::RelativePath { path: spec("./dev/ttyUSB0") }),
            ("@115200", E::RelativePath { path: spec("") }),
            ("/tmp/tty@115200", E::PathOutsideDev { path: spec("/tmp/tty") }),
            ("/devices/tty@115200", E::PathOutsideDev { path: spec("/devices/tty") }),
            ("/dev@115200", E::PathOutsideDev { path: spec("/dev") }),
            ("/dev/@115200", E::PathNotNormalized { path: spec("/dev/") }),
            ("/dev/../etc/passwd@115200", E::PathNotNormalized { path: spec("/dev/../etc/passwd") }),
            ("/dev/./tty@115200", E::PathNotNormalized { path: spec("/dev/./tty") }),
            ("/dev//tty@115200", E::PathNotNormalized { path: spec("/dev//tty") }),
            ("/dev/tty/@115200", E::PathNotNormalized { path: spec("/dev/tty/") }),
            ("/dev/tty USB@115200", E::InvalidPathChar { path: spec("/dev/tty USB"), ch: ' ' }),
            ("/dev/tty@1@115200", E::InvalidPathChar { path: spec("/dev/tty@1"), ch: '@' }),
            ("/dev/tty\n@115200", E::InvalidPathChar { path: spec("/dev/tty\n"), ch: '\n' }),
            ("udp://", E::MissingHost { spec: spec("udp://") }),
            ("udp:///x", E::MissingHost { spec: spec("udp:///x") }),
            ("udp://:5000", E::BadHost { spec: spec("udp://:5000"), host: spec(""), reason: "empty host" }),
            ("udp://10.0.0.7", E::MissingPort { spec: spec("udp://10.0.0.7") }),
            ("udp://10.0.0.7:", E::MissingPort { spec: spec("udp://10.0.0.7:") }),
            ("udp://[::1]", E::MissingPort { spec: spec("udp://[::1]") }),
            ("udp://[::1]:", E::MissingPort { spec: spec("udp://[::1]:") }),
            ("udp://10.0.0.7:0", E::ZeroPort { spec: spec("udp://10.0.0.7:0") }),
            ("udp://10.0.0.7:65536", E::BadPort { spec: spec("udp://10.0.0.7:65536"), port: spec("65536") }),
            ("udp://10.0.0.7:+80", E::BadPort { spec: spec("udp://10.0.0.7:+80"), port: spec("+80") }),
            ("udp://10.0.0.7:http", E::BadPort { spec: spec("udp://10.0.0.7:http"), port: spec("http") }),
            ("udp://10.0.0.7:5000 ", E::BadPort { spec: spec("udp://10.0.0.7:5000 "), port: spec("5000 ") }),
            ("udp://10.0.0.7:5000/x", E::TrailingJunk { spec: spec("udp://10.0.0.7:5000/x"), junk: spec("/x") }),
            ("udp://10.0.0.7:5000?x=1", E::TrailingJunk { spec: spec("udp://10.0.0.7:5000?x=1"), junk: spec("?x=1") }),
            ("udp://[::1]:5000/x", E::TrailingJunk { spec: spec("udp://[::1]:5000/x"), junk: spec("/x") }),
            ("udp://[::1]x:5000", E::TrailingJunk { spec: spec("udp://[::1]x:5000"), junk: spec("x:5000") }),
            ("udp://256.0.0.1:5000", E::BadHost { spec: spec("udp://256.0.0.1:5000"), host: spec("256.0.0.1"), reason: "looks like an IPv4 address but is not a valid one" }),
            ("udp://10.0.0:5000", E::BadHost { spec: spec("udp://10.0.0:5000"), host: spec("10.0.0"), reason: "looks like an IPv4 address but is not a valid one" }),
            ("udp://::1:5000", E::BadHost { spec: spec("udp://::1:5000"), host: spec("::1"), reason: "contains `:`; an IPv6 literal must be bracketed, as `udp://[::1]:5000`" }),
            ("udp://[::1:5000", E::BadHost { spec: spec("udp://[::1:5000"), host: spec("[::1:5000"), reason: "unterminated `[` in an IPv6 literal" }),
            ("udp://[nonsense]:5000", E::BadHost { spec: spec("udp://[nonsense]:5000"), host: spec("nonsense"), reason: "not a valid IPv6 address (zone ids are not supported)" }),
            ("udp://[fe80::1%en0]:5000", E::BadHost { spec: spec("udp://[fe80::1%en0]:5000"), host: spec("fe80::1%en0"), reason: "not a valid IPv6 address (zone ids are not supported)" }),
            ("udp://user@host:5000", E::BadHost { spec: spec("udp://user@host:5000"), host: spec("user@host"), reason: "hostname labels may contain only ASCII letters, digits and `-`" }),
            ("udp://-bad.example:5000", E::BadHost { spec: spec("udp://-bad.example:5000"), host: spec("-bad.example"), reason: "a hostname label may not start or end with `-`" }),
            ("udp://bad..example:5000", E::BadHost { spec: spec("udp://bad..example:5000"), host: spec("bad..example"), reason: "empty hostname label (a leading, trailing or doubled `.`)" }),
            ("udp://trailing.:5000", E::BadHost { spec: spec("udp://trailing.:5000"), host: spec("trailing."), reason: "empty hostname label (a leading, trailing or doubled `.`)" }),
            ("udp://under_score:5000", E::BadHost { spec: spec("udp://under_score:5000"), host: spec("under_score"), reason: "hostname labels may contain only ASCII letters, digits and `-`" }),
        ];
        for (input, want) in cases {
            assert_eq!(parse_port_device(input), Err(want), "spec {input:?}");
        }
        let long_label = format!("udp://{}:5000", "a".repeat(64));
        assert!(matches!(parse_port_device(&long_label), Err(E::BadHost { reason: "hostname label longer than 63 characters", .. })));
        let long_name = format!("udp://{}:5000", vec!["a".repeat(60); 5].join("."));
        assert!(matches!(parse_port_device(&long_name), Err(E::BadHost { reason: "hostname longer than 253 characters", .. })));
    }

    #[test]
    fn canonical_form_round_trips_and_unifies_spellings() {
        for spec in ["/dev/ttyUSB0@115200", "/dev/pts/9@1", "udp://10.0.0.7:5000", "udp://[::1]:5000", "udp://[2001:db8::1]:65535", "udp://board.lab:5000"] {
            let dev = parse_port_device(spec).unwrap();
            assert_eq!(dev.canonical(), spec, "canonical of {spec}");
            assert_eq!(parse_port_device(&dev.canonical()).unwrap(), dev);
            assert_eq!(dev.to_string(), spec);
        }
        assert_eq!(parse_port_device("udp://BOARD.lab:5000").unwrap().canonical(), "udp://board.lab:5000");
        assert_eq!(parse_port_device("udp://[2001:0DB8:0000:0000:0000:0000:0000:0001]:80").unwrap().canonical(), "udp://[2001:db8::1]:80");
    }

    #[test]
    fn a_binding_with_one_device_is_one_link() {
        let b = binding("edge-7", &[("tc", "/dev/ttyUSB0@115200"), ("tm", "/dev/ttyUSB0@115200")]);
        let link = BoardLink::from_binding(&b).unwrap();
        assert_eq!(link.edge_node_id(), "edge-7");
        assert_eq!(link.device(), &serial("/dev/ttyUSB0", 115200));
        assert_eq!(link.ports(), ["tc".to_string(), "tm".to_string()]);
        assert_eq!(link.canonical(), "edge_node_id=edge-7;device=/dev/ttyUSB0@115200;ports=tc,tm");
        let params = link.bind_parameters();
        assert_eq!(params[BIND_PARAM_EDGE_NODE_ID], "edge-7");
        assert_eq!(params[BIND_PARAM_PORT_DEVICE], "/dev/ttyUSB0@115200");
        // Different spellings of one UDP device are one link.
        let b = binding("n", &[("a", "udp://HOST:5000"), ("b", "udp://host:5000")]);
        assert_eq!(BoardLink::from_binding(&b).unwrap().device(), &udp("host", 5000));
    }

    #[test]
    fn a_binding_is_refused_in_each_typed_way() {
        use BoardLinkError as E;
        assert_eq!(BoardLink::from_binding(&binding("", &[("a", "/dev/x@1")])), Err(E::EmptyEdgeNodeId));
        assert_eq!(BoardLink::from_binding(&binding("a b", &[("a", "/dev/x@1")])), Err(E::InvalidEdgeNodeId { id: "a b".into(), ch: ' ' }));
        assert_eq!(BoardLink::from_binding(&binding("n", &[])), Err(E::NoPorts));
        assert_eq!(BoardLink::from_binding(&binding("n", &[("", "/dev/x@1")])), Err(E::EmptyPortName));
        assert_eq!(
            BoardLink::from_binding(&binding("n", &[("tm", "/dev/x@1"), ("tc", "udp://10.0.0.7:0")])),
            Err(E::PortDevice { port: "tc".into(), source: PortDeviceSpecError::ZeroPort { spec: "udp://10.0.0.7:0".into() } })
        );
        // The proto comment's own example is a mixed map: refused, ports named.
        let mixed = BoardLink::from_binding(&binding("n", &[("tm", "/dev/ttyUSB0@115200"), ("tc", "udp://10.0.0.7:5000"), ("aux", "udp://10.0.0.7:5000")])).unwrap_err();
        assert_eq!(
            mixed,
            E::MixedDevices { groups: vec![("/dev/ttyUSB0@115200".into(), vec!["tm".into()]), ("udp://10.0.0.7:5000".into(), vec!["aux".into(), "tc".into()])] }
        );
        let text = mixed.to_string();
        assert!(text.contains("tm") && text.contains("tc") && text.contains("aux") && text.contains("ttyUSB0"), "{text}");
        // Same path, different baud, is a different device.
        assert!(matches!(BoardLink::from_binding(&binding("n", &[("a", "/dev/x@9600"), ("b", "/dev/x@115200")])), Err(E::MixedDevices { .. })));
    }

    #[test]
    fn completeness_against_the_declared_ports_is_checked() {
        use BoardLinkError as E;
        let b = binding("n", &[("a", "/dev/x@1"), ("b", "/dev/x@1")]);
        assert!(BoardLink::from_binding_checked(&b, ["b", "a"]).is_ok());
        assert_eq!(BoardLink::from_binding_checked(&b, ["a", "b", "c", "d"]), Err(E::MissingPorts { ports: vec!["c".into(), "d".into()] }));
        assert_eq!(BoardLink::from_binding_checked(&b, ["a"]), Err(E::ExtraPorts { ports: vec!["b".into()] }));
        // A structural refusal still comes first.
        assert_eq!(BoardLink::from_binding_checked(&binding("", &[("a", "/dev/x@1")]), ["a"]), Err(E::EmptyEdgeNodeId));
    }

    #[test]
    fn the_hash_is_stable_and_sensitive() {
        let base = BoardLink::from_binding(&binding("edge-7", &[("tc", "/dev/ttyUSB0@115200"), ("tm", "/dev/ttyUSB0@115200")])).unwrap();
        let again = BoardLink::from_binding(&binding("edge-7", &[("tm", "/dev/ttyUSB0@115200"), ("tc", "/dev/ttyUSB0@115200")])).unwrap();
        assert_eq!(base.config_hash(), again.config_hash());
        assert_eq!(base.config_hash_hex().len(), 64);
        let variants = [
            binding("edge-8", &[("tc", "/dev/ttyUSB0@115200"), ("tm", "/dev/ttyUSB0@115200")]),
            binding("edge-7", &[("tc", "/dev/ttyUSB0@57600"), ("tm", "/dev/ttyUSB0@57600")]),
            binding("edge-7", &[("tc", "/dev/ttyUSB1@115200"), ("tm", "/dev/ttyUSB1@115200")]),
            binding("edge-7", &[("tc", "/dev/ttyUSB0@115200")]),
            binding("edge-7", &[("tc", "udp://10.0.0.7:5000"), ("tm", "udp://10.0.0.7:5000")]),
        ];
        let mut seen = BTreeSet::from([base.config_hash()]);
        for v in variants {
            assert!(seen.insert(BoardLink::from_binding(&v).unwrap().config_hash()), "a changed field must change the hash: {v:?}");
        }
        // Pinned, so the definition cannot drift unnoticed: SHA-256 of
        // {"edge_node_id":"e","device":"/dev/ttyS1@115200","ports":["p"]}.
        let pinned = BoardLink::from_binding(&binding("e", &[("p", "/dev/ttyS1@115200")])).unwrap();
        assert_eq!(pinned.config_hash(), openssl::sha::sha256(br#"{"edge_node_id":"e","device":"/dev/ttyS1@115200","ports":["p"]}"#));
        // power_control is not part of the link.
        let mut with_power = binding("e", &[("p", "/dev/ttyS1@115200")]);
        with_power.power_control = "gpio:3".into();
        assert_eq!(BoardLink::from_binding(&with_power).unwrap().config_hash(), pinned.config_hash());
    }

    #[test]
    fn power_control_variants_parse() {
        assert_eq!(parse_power_control(""), Ok(PowerControl::None));
        assert!(PowerControl::None.is_none());
        let cmd = parse_power_control("cmd:/opt/board/power-cycle.sh").unwrap();
        assert_eq!(cmd, PowerControl::Cmd { path: "/opt/board/power-cycle.sh".into() });
        assert!(!cmd.is_none());
        assert_eq!(cmd.canonical(), "cmd:/opt/board/power-cycle.sh");
        assert_eq!(cmd.to_string(), "cmd:/opt/board/power-cycle.sh");
        assert_eq!("cmd:/a".parse::<PowerControl>(), Ok(PowerControl::Cmd { path: "/a".into() }));
        assert_eq!(PowerControl::None.canonical(), "");
        // Canonical strings parse back to themselves.
        for spec in ["", "cmd:/x", "cmd:/usr/local/bin/ps-4", "cmd:/tmp/a.b/c-d_e"] {
            assert_eq!(parse_power_control(spec).unwrap().canonical(), spec);
        }
    }

    #[test]
    fn every_malformed_power_control_is_a_typed_error() {
        use PowerControlSpecError as E;
        let spec = |s: &str| s.to_string();
        let cases: Vec<(&str, PowerControlSpecError)> = vec![
            ("gpio://17", E::Reserved { spec: spec("gpio://17") }),
            ("gpio:3", E::Reserved { spec: spec("gpio:3") }),
            ("gpio:", E::Reserved { spec: spec("gpio:") }),
            ("cmd:", E::MissingPath),
            ("cmd:relative/path", E::RelativePath { path: spec("relative/path") }),
            ("cmd:./x", E::RelativePath { path: spec("./x") }),
            ("cmd:/", E::PathNotNormalized { path: spec("/") }),
            ("cmd:/a/", E::PathNotNormalized { path: spec("/a/") }),
            ("cmd://a", E::PathNotNormalized { path: spec("//a") }),
            ("cmd:/a//b", E::PathNotNormalized { path: spec("/a//b") }),
            ("cmd:/a/./b", E::PathNotNormalized { path: spec("/a/./b") }),
            ("cmd:/a/../b", E::PathNotNormalized { path: spec("/a/../b") }),
            ("cmd:/a b", E::InvalidPathChar { path: spec("/a b"), ch: ' ' }),
            ("cmd:/a\tb", E::InvalidPathChar { path: spec("/a\tb"), ch: '\t' }),
            ("cmd:/a\nb", E::InvalidPathChar { path: spec("/a\nb"), ch: '\n' }),
            ("cmd:/a\0b", E::InvalidPathChar { path: spec("/a\0b"), ch: '\0' }),
            ("CMD:/a", E::UnknownScheme { spec: spec("CMD:/a"), scheme: spec("CMD") }),
            ("GPIO://1", E::UnknownScheme { spec: spec("GPIO://1"), scheme: spec("GPIO") }),
            ("http://host/x", E::UnknownScheme { spec: spec("http://host/x"), scheme: spec("http") }),
            ("ssh:host", E::UnknownScheme { spec: spec("ssh:host"), scheme: spec("ssh") }),
            ("/opt/board/power", E::NotAChannel { spec: spec("/opt/board/power") }),
            ("power", E::NotAChannel { spec: spec("power") }),
            (" cmd:/a", E::NotAChannel { spec: spec(" cmd:/a") }),
            (":x", E::NotAChannel { spec: spec(":x") }),
        ];
        for (input, want) in cases {
            assert_eq!(parse_power_control(input), Err(want), "spec {input:?}");
        }
        let long = format!("cmd:/{}", "a".repeat(MAX_POWER_CONTROL_PATH_LEN));
        assert!(matches!(parse_power_control(&long), Err(E::PathTooLong { len }) if len == MAX_POWER_CONTROL_PATH_LEN + 1));
        let ok = format!("cmd:/{}", "a".repeat(MAX_POWER_CONTROL_PATH_LEN - 1));
        assert!(parse_power_control(&ok).is_ok());
        // Every error names what it is about.
        assert!(E::Reserved { spec: "gpio://17".into() }.to_string().contains("reserved"));
    }

    #[test]
    fn the_power_cycle_argv_is_the_documented_one() {
        assert_eq!(
            power_cycle_argv("zcu104-a", "controller", "pc1", 1_767_225_638_000_000_000),
            ["power-cycle", "--edge-node-id", "zcu104-a", "--instance", "controller", "--fault-id", "pc1", "--tai-ns", "1767225638000000000"]
        );
        assert_eq!(power_cycle_argv("e", "i", "f", -5).last().map(String::as_str), Some("-5"));
    }
}
