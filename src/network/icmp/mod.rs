pub mod packet;
pub mod provider;

#[cfg(unix)]
pub mod unix;

#[cfg(target_os = "windows")]
pub mod windows;

pub use provider::IcmpProvider;
pub use packet::{IcmpEchoRequest, IcmpEchoReply};

#[cfg(unix)]
pub type DefaultIcmpProvider = unix::UnixDgramIcmp;

#[cfg(unix)]
pub type TracerouteIcmpProvider = unix::UnixRawIcmp;

#[cfg(target_os = "windows")]
pub type DefaultIcmpProvider = windows::WindowsRawIcmp;

#[cfg(target_os = "windows")]
pub type TracerouteIcmpProvider = windows::WindowsRawIcmp;
