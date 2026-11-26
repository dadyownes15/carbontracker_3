//! A failed request to a service, in plain words.

/// Why a request to `service` failed: it timed out, could not connect,
/// answered with an error status, or sent what could not be read.
pub(crate) fn failure(service: &str, error: &reqwest::Error) -> String {
    if error.is_timeout() {
        format!("{service} did not answer in time")
    } else if error.is_connect() {
        format!("cannot connect to {service}: no network, or it is down")
    } else if let Some(status) = error.status() {
        format!("{service} answered {status}")
    } else if error.is_decode() || error.is_body() {
        format!("{service} sent an answer that could not be read")
    } else {
        format!("{service}: {error}")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn client() -> reqwest::Client {
        reqwest::Client::builder().timeout(Duration::from_millis(200)).build().unwrap()
    }

    /// AC/PROV-53
    #[tokio::test]
    async fn each_failure_is_said_in_plain_words() {
        // Nothing listens on the discard port.
        let refused = client().get("http://127.0.0.1:9/").send().await.unwrap_err();
        assert_eq!(failure("Energinet", &refused), "cannot connect to Energinet: no network, or it is down");

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
            .mount(&server)
            .await;
        let slow = client().get(server.uri()).send().await.unwrap_err();
        assert_eq!(failure("Energinet", &slow), "Energinet did not answer in time");

        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(503)).mount(&server).await;
        let status = client().get(server.uri()).send().await.unwrap().error_for_status().unwrap_err();
        assert_eq!(failure("Energinet", &status), "Energinet answered 503 Service Unavailable");
    }
}
