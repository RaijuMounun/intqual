#![allow(unused_imports)]
pub mod packet;
pub mod provider;

#[cfg(unix)]
pub mod unix;

#[cfg(unix)]
pub mod unix_raw;

#[cfg(target_os = "windows")]
pub mod windows;

pub use packet::{IcmpEchoReply, IcmpEchoRequest};
pub use provider::IcmpProvider;

#[cfg(unix)]
pub type DefaultIcmpProvider = unix::UnixDgramIcmp;

#[cfg(unix)]
pub type TracerouteIcmpProvider = unix_raw::RawIcmpProvider;

#[cfg(target_os = "windows")]
pub type DefaultIcmpProvider = windows::IcmpApiProvider;

#[cfg(target_os = "windows")]
pub type TracerouteIcmpProvider = windows::IcmpApiProvider;
