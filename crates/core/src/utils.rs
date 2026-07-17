use std::time::{SystemTime, UNIX_EPOCH};
use crate::models::ProbeError;

pub fn current_timestamp() -> Result<u64, ProbeError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| ProbeError::TimeSyncError)
}
