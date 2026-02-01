// Async IPC client for GUI communication with the daemon.
// Unlike the synchronous CLI client, this uses tokio for non-blocking
// request/response and log subscription streaming.
