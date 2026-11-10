//! CalDAV two-way sync (RFC 4791 / 6578 / 6764). (Being implemented.)

use super::{RemoteCalendar, SyncError, SyncReport};
use crate::config::CalendarSource;
use crate::store::Store;

pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// CalDAV server for mail providers known to offer it with the same
/// (app) password, by e-mail domain or IMAP host.
pub fn preset_url(_email_domain: &str, _imap_host: &str) -> Option<&'static str> {
    None
}

/// Finds the calendars behind `url` (a server address, a calendar-home URL
/// or a calendar URL), used to test a new source.
pub fn discover(_url: &str, _creds: &Credentials) -> Result<Vec<RemoteCalendar>, SyncError> {
    Err(SyncError::Other("CalDAV is not available yet".into()))
}

/// Uploads local changes of this source's calendars, then downloads
/// calendars and changed events.
pub fn sync(_store: &Store, _source: &CalendarSource, _creds: &Credentials) -> Result<SyncReport, SyncError> {
    Err(SyncError::Other("CalDAV sync is not available yet".into()))
}
