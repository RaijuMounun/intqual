pub mod packet;
pub mod provider;
pub mod raw;

#[cfg(unix)]
pub mod unix;

pub use provider::IcmpProvider;
pub use packet::{IcmpEchoRequest, IcmpEchoReply};

#[cfg(unix)]
pub type DefaultIcmpProvider = unix::UnixDgramIcmp;

#[cfg(unix)]
pub type TracerouteIcmpProvider = raw::RawIcmpProvider;

#[cfg(target_os = "windows")]
pub type DefaultIcmpProvider = raw::RawIcmpProvider;

#[cfg(target_os = "windows")]
pub type TracerouteIcmpProvider = raw::RawIcmpProvider;
