//! Desktop notifications (best effort, never blocks the caller).

pub fn desktop(summary: String, body: String) {
    std::thread::spawn(move || {
        let result = notify_rust::Notification::new()
            .appname("RustMail")
            .summary(&summary)
            .body(&body)
            .icon("mail-unread")
            .timeout(notify_rust::Timeout::Milliseconds(6000))
            .show();
        if let Err(e) = result {
            log::debug!("desktop notification failed: {e}");
        }
    });
}
