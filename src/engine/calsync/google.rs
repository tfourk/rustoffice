//! Google Calendar API v3 two-way sync. (Being implemented.)

use super::{SyncError, SyncReport};
use crate::config::CalendarSource;
use crate::store::Store;

pub const API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Where to send requests (tests point `base` at a fake server) and the
/// OAuth access token to send them with.
pub struct GoogleApi {
    pub base: String,
    pub token: String,
}

/// Uploads local changes of this source's calendars, then downloads the
/// calendar list and changed events.
pub fn sync(_store: &Store, _source: &CalendarSource, _api: &GoogleApi) -> Result<SyncReport, SyncError> {
    Err(SyncError::Other("Google Calendar sync is not available yet".into()))
}
