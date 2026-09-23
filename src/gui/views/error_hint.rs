//! Turns raw SSH failure text into a short title and an actionable hint.

/// Human-facing summary of a tunnel failure.
#[derive(Debug, PartialEq, Eq)]
pub struct ErrorSummary {
    pub title: &'static str,
    pub hint: Option<&'static str>,
}

/// Classify a tunnel's `last_error`. Order matters: more specific patterns
/// (e.g. Kerberos-only auth) are checked before their general case.
pub fn summarize(error: &str) -> ErrorSummary {
    let e = error.to_ascii_lowercase();
    let (title, hint) = if e.contains("permission denied")
        && e.contains("gssapi")
        && !e.contains("publickey")
    {
        (
            "Authentication failed",
            Some(
                "The server accepts only Kerberos or a password, and Burrow can't type a \
                 password. Your Kerberos ticket may have expired; run kinit in Terminal.",
            ),
        )
    } else if e.contains("permission denied") {
        (
            "Authentication failed",
            Some("Check that your key is loaded (ssh-add -l) and accepted by the server."),
        )
    } else if e.contains("host key verification failed") {
        (
            "Host key not trusted",
            Some("Connect once with ssh in Terminal to review and accept the host key."),
        )
    } else if e.contains("already in use") {
        (
            "Local port in use",
            Some("Another program is using this port. Stop it or choose a different port."),
        )
    } else if e.contains("could not resolve hostname") || e.contains("name or service not known") {
        (
            "Host not found",
            Some("Check the host name and your DNS or VPN connection."),
        )
    } else if e.contains("timed out") || e.contains("not responding") {
        (
            "Host unreachable",
            Some("The server didn't answer. Check your network or VPN."),
        )
    } else if e.contains("connection refused") {
        (
            "Connection refused",
            Some("The server isn't accepting SSH on this port."),
        )
    } else if e.contains("connection closed") || e.contains("connection reset") {
        ("Connection closed by server", None)
    } else if e.contains("failed to spawn ssh") {
        (
            "SSH could not start",
            Some("Check the ssh binary path in the tunnel settings."),
        )
    } else {
        ("Connection failed", None)
    };
    ErrorSummary { title, hint }
}

/// The most telling line of a multi-line SSH error: the last one that isn't
/// generic noise, with the "exited with code N:" prefix stripped.
pub fn key_line(error: &str) -> &str {
    let error = error
        .split_once(": ")
        .filter(|(head, _)| head.starts_with("exited with code") || head == &"killed by signal")
        .map_or(error, |(_, rest)| rest);
    error
        .lines()
        .map(str::trim)
        .rfind(|l| {
            !l.is_empty()
                && !l.starts_with("Connection closed by UNKNOWN")
                && !l.starts_with("kex_exchange_identification")
                && !l.eq_ignore_ascii_case("Permission denied, please try again.")
        })
        .unwrap_or(error.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERBEROS: &str = "exited with code 255: Permission denied, please try again.\n\
        kostik@example.org: Permission denied (gssapi-keyex,gssapi-with-mic,password).\n\
        kex_exchange_identification: Connection closed by remote host\n\
        Connection closed by UNKNOWN port 65535";

    #[test]
    fn kerberos_only_auth_suggests_kinit() {
        let s = summarize(KERBEROS);
        assert_eq!(s.title, "Authentication failed");
        assert!(s.hint.unwrap().contains("kinit"));
    }

    #[test]
    fn publickey_auth_suggests_ssh_add() {
        let s = summarize("Permission denied (publickey,gssapi-with-mic).");
        assert!(s.hint.unwrap().contains("ssh-add"));
    }

    #[test]
    fn classifies_common_failures() {
        assert_eq!(
            summarize("port 3389 already in use").title,
            "Local port in use"
        );
        assert_eq!(
            summarize("ssh: Could not resolve hostname foo: nodename nor servname").title,
            "Host not found"
        );
        assert_eq!(
            summarize("Timeout, server x not responding.").title,
            "Host unreachable"
        );
        assert_eq!(summarize("something odd").title, "Connection failed");
    }

    #[test]
    fn key_line_skips_noise() {
        assert_eq!(
            key_line(KERBEROS),
            "kostik@example.org: Permission denied (gssapi-keyex,gssapi-with-mic,password)."
        );
    }

    #[test]
    fn key_line_strips_exit_prefix() {
        assert_eq!(
            key_line("exited with code 255: Connection refused"),
            "Connection refused"
        );
        assert_eq!(key_line("port 22 already in use"), "port 22 already in use");
    }
}
