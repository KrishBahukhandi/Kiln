//! The HTTP client, and the translation from transport failures to Kiln errors.

use std::time::Duration;

use kiln_core::error::{Error, ErrorKind, Result};

use crate::cache::MetadataCache;

/// Identifies Kiln to the servers it talks to, so operators can see who is
/// calling and Kiln can be blocked or rate-limited on its own merits.
fn user_agent() -> String {
    format!(
        "kiln/{} (+https://github.com/bahukhandi-labs/kiln)",
        kiln_core::KILN_VERSION
    )
}

/// How long to wait for a name, a connection, and response headers.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a metadata request may take in total.
///
/// Applies to release indexes and checksum files only. Downloads are pointedly
/// *not* capped this way: a slow connection should be slow, not fail.
const METADATA_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a download may go without receiving any data before Kiln gives up.
const STALLED_BODY_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest release index Kiln will read into memory.
const MAX_METADATA_BYTES: u64 = 32 * 1024 * 1024;

/// A blocking HTTP client.
pub struct Http {
    /// For release indexes and checksum files, which are small and bounded.
    agent: ureq::Agent,
    /// For artifacts. Deliberately has no total time limit.
    download_agent: ureq::Agent,
    /// Reports redirects instead of following them.
    redirect_agent: ureq::Agent,
    cache: Option<MetadataCache>,
    offline: bool,
}

impl Http {
    /// Build a client.
    ///
    /// In `offline` mode every request fails immediately with an explanation,
    /// rather than waiting for a connection that cannot succeed.
    pub fn new(offline: bool) -> Self {
        let base = || {
            ureq::Agent::config_builder()
                .user_agent(user_agent())
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_resolve(Some(CONNECT_TIMEOUT))
                .timeout_recv_response(Some(CONNECT_TIMEOUT))
                .http_status_as_error(false)
        };

        Http {
            agent: base()
                .timeout_global(Some(METADATA_TIMEOUT))
                .build()
                .new_agent(),
            // Downloads get their own timeout profile, deliberately not
            // `base()`.
            //
            // A metadata request should give up quickly. A download should not:
            // a 50 MB runtime on a hotel connection takes longer than any total
            // limit worth setting. More subtly, artifact URLs redirect — GitHub
            // sends release downloads to a CDN — and `timeout_recv_response` is
            // measured across the whole redirect chain rather than reset per
            // hop, so a header deadline sized for a small JSON fetch starts
            // failing real downloads intermittently.
            //
            // What is left still catches every failure worth catching: a host
            // that will not resolve, a connection that will not open, and a
            // stream that has stopped delivering bytes.
            download_agent: ureq::Agent::config_builder()
                .user_agent(user_agent())
                .timeout_resolve(Some(CONNECT_TIMEOUT))
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_recv_body(Some(STALLED_BODY_TIMEOUT))
                .http_status_as_error(false)
                .build()
                .new_agent(),
            redirect_agent: base()
                .max_redirects(0)
                .max_redirects_will_error(false)
                .timeout_global(Some(METADATA_TIMEOUT))
                .build()
                .new_agent(),
            cache: None,
            offline,
        }
    }

    /// Serve release indexes from `cache` when they are still fresh.
    #[must_use]
    pub fn with_cache(mut self, cache: MetadataCache) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Whether this client refuses to make requests.
    pub fn is_offline(&self) -> bool {
        self.offline
    }

    /// The agent used for artifact downloads.
    pub(crate) fn download_agent(&self) -> &ureq::Agent {
        &self.download_agent
    }

    /// Fail early if this client is not allowed to reach the network.
    pub(crate) fn check_online(&self, what: &str) -> Result<()> {
        if self.offline {
            return Err(
                Error::new(ErrorKind::Network, "Kiln needs network access to continue")
                    .because(format!(
                        "Kiln is running offline, and {what} is not available locally."
                    ))
                    .hint("run the same command without `--offline` once, to fetch it")
                    .command("kiln cache list"),
            );
        }
        Ok(())
    }

    /// Fetch a text document, such as a release index or a checksum file.
    ///
    /// `what` names the thing being fetched, in the user's terms, and appears in
    /// any error: "the Node.js release index", not the URL.
    pub fn get_text(&self, url: &str, what: &str) -> Result<String> {
        if let Some(cache) = &self.cache
            && let Some(cached) = cache.get(url)
        {
            return Ok(cached);
        }

        self.check_online(what)?;

        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| transport_error(e, url, what))?;

        check_status(response.status().as_u16(), url, what)?;

        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_METADATA_BYTES)
            .read_to_string()
            .map_err(|e| transport_error(e, url, what))?;

        if let Some(cache) = &self.cache {
            // A cache write failing is not a reason to fail the command.
            let _ = cache.put(url, &body);
        }
        Ok(body)
    }

    /// Ask where a URL redirects to, without following it.
    ///
    /// Used to discover the newest release tag of a project that publishes one:
    /// `releases/latest` redirects to `releases/tag/<tag>`, which is a stable,
    /// unauthenticated, un-rate-limited way to learn the tag.
    pub fn redirect_target(&self, url: &str, what: &str) -> Result<String> {
        self.check_online(what)?;

        let response = self
            .redirect_agent
            .get(url)
            .call()
            .map_err(|e| transport_error(e, url, what))?;

        let status = response.status().as_u16();
        if !(300..400).contains(&status) {
            return Err(Error::new(
                ErrorKind::Network,
                format!("Could not determine the latest release of {what}"),
            )
            .because(format!("{url} answered {status} instead of redirecting"))
            .hint("the upstream layout may have changed; please report this"));
        }

        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Network,
                    format!("Could not determine the latest release of {what}"),
                )
                .because(format!("{url} redirected without saying where to"))
            })
    }
}

/// Map an HTTP status onto Kiln's error vocabulary.
pub(crate) fn check_status(status: u16, url: &str, what: &str) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }

    let error = match status {
        404 | 410 => Error::not_found(format!("{what} was not found"))
            .because(format!("{url} answered {status}"))
            .hint("the version may have been withdrawn, or never published for this platform"),
        401 | 403 => Error::new(ErrorKind::Network, format!("Access to {what} was refused"))
            .because(format!("{url} answered {status}"))
            .hint("a proxy or a rate limit may be in the way; try again shortly"),
        429 => Error::new(ErrorKind::Network, format!("{what} is rate-limiting Kiln"))
            .because(format!("{url} answered 429"))
            .hint("wait a few minutes and run the command again"),
        500..=599 => Error::new(ErrorKind::Network, format!("{what} is unavailable"))
            .because(format!("{url} answered {status}"))
            .hint("this is an upstream problem; try again shortly"),
        _ => Error::new(ErrorKind::Network, format!("Could not fetch {what}"))
            .because(format!("{url} answered {status}")),
    };
    Err(error)
}

/// Map a transport failure onto Kiln's error vocabulary.
///
/// The point is to distinguish "your machine is not on the internet" from "the
/// server said no", because the two need completely different responses from the
/// person reading the message.
pub(crate) fn transport_error(error: ureq::Error, url: &str, what: &str) -> Error {
    use ureq::Error as U;

    match error {
        U::HostNotFound => Error::new(ErrorKind::Network, format!("Could not fetch {what}"))
            .because(format!("The host in {url} could not be resolved."))
            .hint("check your network connection or DNS settings")
            .hint("if the artifact is already installed, Kiln does not need the network"),
        U::ConnectionFailed | U::Io(_) => {
            Error::new(ErrorKind::Network, format!("Could not fetch {what}"))
                .because(format!("The connection to {url} failed."))
                .hint("check your network connection, VPN, or proxy settings")
        }
        U::Timeout(_) | U::BodyStalled => {
            Error::new(ErrorKind::Network, format!("Timed out fetching {what}"))
                .because(format!("{url} did not respond in time."))
                .hint("try again, or check whether a proxy is interfering")
        }
        U::TooManyRedirects | U::RedirectFailed => {
            Error::new(ErrorKind::Network, format!("Could not fetch {what}"))
                .because(format!("{url} redirected too many times."))
        }
        U::Tls(_) | U::Rustls(_) | U::Pem(_) => Error::new(
            ErrorKind::Network,
            format!("Could not fetch {what} securely"),
        )
        .because(format!(
            "The TLS connection to {url} could not be established."
        ))
        .hint("a corporate proxy that intercepts TLS is the usual cause"),
        U::StatusCode(status) => match check_status(status, url, what) {
            Err(error) => error,
            // `check_status` only returns Ok for 2xx, which is not an error.
            Ok(()) => Error::internal("Unexpected success status treated as an error"),
        },
        other => Error::new(ErrorKind::Network, format!("Could not fetch {what}"))
            .because(format!("{url}: {other}"))
            .with_source(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_identifies_kiln_and_its_version() {
        let agent = user_agent();
        assert!(agent.starts_with("kiln/"));
        assert!(agent.contains(kiln_core::KILN_VERSION));
    }

    #[test]
    fn offline_clients_refuse_before_connecting() {
        let http = Http::new(true);
        assert!(http.is_offline());

        let error = http
            .get_text(
                "https://nodejs.org/dist/index.json",
                "the Node.js release index",
            )
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Network);
        assert!(error.reason().unwrap().contains("offline"));
        // The sentence has to read correctly whatever `what` is.
        assert!(
            error
                .reason()
                .unwrap()
                .contains("the Node.js release index is not available locally"),
            "{}",
            error.reason().unwrap()
        );
    }

    #[test]
    fn success_statuses_are_not_errors() {
        for status in [200, 201, 204, 299] {
            assert!(check_status(status, "https://example.test/x", "a thing").is_ok());
        }
    }

    #[test]
    fn missing_artifacts_are_not_found_rather_than_network_failures() {
        // The distinction matters: one means "ask for a different version", the
        // other means "check your wifi".
        for status in [404, 410] {
            let error =
                check_status(status, "https://example.test/x", "Node.js 99.0.0").unwrap_err();
            assert_eq!(error.kind(), ErrorKind::NotFound);
            assert!(error.summary().contains("Node.js 99.0.0"));
        }
    }

    #[test]
    fn rate_limits_and_outages_are_distinguishable() {
        let limited = check_status(429, "https://example.test/x", "the index").unwrap_err();
        assert!(limited.summary().contains("rate-limiting"));

        let down = check_status(503, "https://example.test/x", "the index").unwrap_err();
        assert!(down.summary().contains("unavailable"));
        assert!(down.hints().iter().any(|h| h.text().contains("upstream")));

        for status in [429, 503, 403] {
            assert_eq!(
                check_status(status, "https://example.test/x", "the index")
                    .unwrap_err()
                    .kind(),
                ErrorKind::Network
            );
        }
    }

    #[test]
    fn dns_failure_suggests_checking_the_connection() {
        let error = transport_error(
            ureq::Error::HostNotFound,
            "https://nodejs.test/dist/index.json",
            "the Node.js release index",
        );
        assert_eq!(error.kind(), ErrorKind::Network);
        assert!(error.reason().unwrap().contains("resolved"));
        assert!(
            error
                .hints()
                .iter()
                .any(|h| h.text().contains("already installed")),
            "should mention that cached artifacts need no network"
        );
    }

    #[test]
    fn tls_failure_names_the_usual_cause() {
        let error = transport_error(
            ureq::Error::Tls("handshake"),
            "https://nodejs.org/",
            "the Node.js release index",
        );
        assert!(error.hints().iter().any(|h| h.text().contains("proxy")));
    }
}
