//! Contract discovery regression against the selected current Briefcase client catalog.

use briefcase_client::{Client, Config, Error, ServiceVersion};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

const CURRENT: &str = include_str!("fixtures/briefcase-v1.1.0.json");

#[tokio::test]
async fn current_briefcase_contract_connects_without_disabling_negotiation()
-> Result<(), Box<dyn std::error::Error>> {
    let version: ServiceVersion = serde_json::from_str(CURRENT)?;
    version.check_compatibility()?;
    let server = MockServer::start().await;
    Mock::given(path("/api/version"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("briefcase-api-version", "v1")
                .set_body_string(CURRENT),
        )
        .expect(1)
        .mount(&server)
        .await;
    Client::connect(
        Config::new(&format!("{}/api/v1/", server.uri()), "client-workspace")?
            .with_auto_update(false),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn stale_link_contract_still_fails_before_any_authenticated_operation()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalog: serde_json::Value = serde_json::from_str(CURRENT)?;
    for operation in catalog["operations"]
        .as_array_mut()
        .ok_or("operations missing")?
    {
        if operation["version"] == "1.1.0" {
            operation["version"] = "1.0.0".into();
        }
    }
    let server = MockServer::start().await;
    Mock::given(path("/api/version"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("briefcase-api-version", "v1")
                .set_body_json(catalog),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = Client::connect(
        Config::new(&format!("{}/api/v1/", server.uri()), "client-workspace")?
            .with_auto_update(false),
    )
    .await;
    let Err(Error::Incompatible(error)) = result else {
        return Err("stale contract was accepted".into());
    };
    let expected = briefcase_client::OPERATIONS
        .iter()
        .filter(|operation| operation.version == "1.1.0")
        .map(|operation| operation.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(!expected.is_empty());
    assert_eq!(
        error
            .mismatched_operations
            .iter()
            .map(|operation| operation.id.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    let requests = server.received_requests().await.ok_or("requests missing")?;
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("authorization"));
    Ok(())
}
