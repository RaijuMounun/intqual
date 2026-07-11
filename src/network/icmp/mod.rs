pub mod packet;
pub mod provider;

#[cfg(unix)]
pub mod unix;

#[cfg(unix)]
pub mod unix_raw;

#[cfg(target_os = "windows")]
pub mod windows;

pub use provider::IcmpProvider;
pub use packet::{IcmpEchoRequest, IcmpEchoReply};

#[cfg(unix)]
pub type DefaultIcmpProvider = unix::UnixDgramIcmp;

#[cfg(unix)]
pub type TracerouteIcmpProvider = unix_raw::RawIcmpProvider;

#[cfg(target_os = "windows")]
pub type DefaultIcmpProvider = windows::RawIcmpProvider;

#[cfg(target_os = "windows")]
pub type TracerouteIcmpProvider = windows::RawIcmpProvider;
