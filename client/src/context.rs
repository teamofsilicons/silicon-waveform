use super::{Client, Error, LoginStatus, Result};
use silicon_iam_client::models::{
    ActorRefType, ApplicationAuthorizationActorType, OAuthTokenResponse,
};

impl LoginStatus {
    pub(super) fn from_authority(
        authority: silicon_iam_client::models::ApplicationAuthorization,
    ) -> Result<Self> {
        let status = Self {
            authenticated: true,
            actor: Some(super::LoginActor {
                actor_type: authority.actor_type.ok_or_else(invalid_context)?,
                public_id: authority.public_id.ok_or_else(invalid_context)?,
            }),
            org_id: Some(authority.org_id),
            testing_environment_id: authority.testing_environment_id.map(|id| id.to_string()),
        };
        status.validate()?;
        Ok(status)
    }
    /// Requires an authenticated canonical actor, a single organization and a valid world.
    pub fn validate(&self) -> Result<()> {
        let actor = self.actor.as_ref().ok_or_else(invalid_context)?;
        let prefix = match actor.actor_type {
            ApplicationAuthorizationActorType::Carbon => "c:",
            ApplicationAuthorizationActorType::Silicon => "si:",
            _ => return Err(invalid_context()),
        };
        if !self.authenticated
            || !actor.public_id.strip_prefix(prefix).is_some_and(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            })
            || !self.org_id.as_ref().is_some_and(|org| {
                !org.is_empty()
                    && org.bytes().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')
                    })
            })
            || self
                .testing_environment_id
                .as_ref()
                .is_some_and(|id| !uuid::Uuid::parse_str(id).is_ok_and(|id| !id.is_nil()))
        {
            return Err(invalid_context());
        }
        Ok(())
    }

    /// Rejects a status response belonging to another account, organization or world.
    pub fn ensure_matches(&self, current: &Self) -> Result<()> {
        self.validate()?;
        current.validate()?;
        if self.actor != current.actor
            || self.org_id != current.org_id
            || self.testing_environment_id != current.testing_environment_id
        {
            return Err(Error::Invalid(
                "login context changed; select the original profile, account and organization"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Validates login/refresh metadata before credentials can replace a saved family.
    pub fn verify_tokens(&self, tokens: &OAuthTokenResponse) -> Result<()> {
        self.validate()?;
        let actor = tokens.actor.as_ref().ok_or_else(invalid_context)?;
        let kind = match actor.type_field {
            ActorRefType::Carbon => ApplicationAuthorizationActorType::Carbon,
            ActorRefType::Silicon => ApplicationAuthorizationActorType::Silicon,
            _ => return Err(invalid_context()),
        };
        if tokens.org_id != self.org_id
            || !self.actor.as_ref().is_some_and(|expected| {
                expected.actor_type == kind && expected.public_id == actor.public_id
            })
        {
            return Err(Error::Invalid(
                "credential response changed actor or organization; saved login was not replaced"
                    .into(),
            ));
        }
        Ok(())
    }
}

fn invalid_context() -> Error {
    Error::Invalid("login must prove one canonical actor, organization and world; log in again with a separate profile".into())
}

impl Client {
    pub(super) fn verify_scope(&self, org: &str, actor: Option<&str>) -> Result<()> {
        if let Some(context) = &self.login_context
            && (context.org_id.as_deref() != Some(org)
                || actor.is_some_and(|actor| {
                    context
                        .actor
                        .as_ref()
                        .is_none_or(|expected| expected.public_id != actor)
                }))
        {
            return Err(Error::Invalid(
                "request actor or organization differs from the selected login profile".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> LoginStatus {
        serde_json::from_value(json!({"authenticated":true,"actor":{"actor_type":"silicon","public_id":"si:agent"},"org_id":"tos","testing_environment_id":null})).unwrap()
    }

    #[test]
    fn credential_successors_require_complete_matching_actor_and_org() {
        let expected = context();
        let valid = json!({"access_token":"access","refresh_token":"refresh","token_type":"Bearer","expires_in":1800,"scope":"","actor":{"type":"silicon","public_id":"si:agent"},"org_id":"tos"});
        expected
            .verify_tokens(&serde_json::from_value(valid.clone()).unwrap())
            .unwrap();
        for (field, value) in [
            ("org_id", json!("other")),
            ("org_id", json!(null)),
            ("actor", json!({"type":"carbon","public_id":"c:alice"})),
            ("actor", json!(null)),
        ] {
            let mut response = valid.clone();
            response[field] = value;
            assert!(
                expected
                    .verify_tokens(&serde_json::from_value(response).unwrap())
                    .is_err()
            );
        }
        let mut changed = expected.clone();
        changed.testing_environment_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(expected.ensure_matches(&changed).is_err());
        changed = expected.clone();
        changed.actor.as_mut().unwrap().public_id = "agent".into();
        assert!(changed.validate().is_err());
    }

    #[tokio::test]
    async fn captured_sdk_context_blocks_other_accounts_before_network() {
        let server = wiremock::MockServer::start().await;
        let expected = context();
        let selected = Client::new(&server.uri(), super::super::Auth::Anonymous)
            .unwrap()
            .with_bearer("fixture")
            .with_login_context(&expected)
            .unwrap();
        let original = selected.clone();
        let mut other = expected.clone();
        other.org_id = Some("other".into());
        assert!(selected.with_login_context(&other).is_err());
        let switched = Client::new(&server.uri(), super::super::Auth::Anonymous)
            .unwrap()
            .with_bearer("other-fixture")
            .with_login_context(&other)
            .unwrap();
        assert!(original.jobs("other", "si:agent", None).await.is_err());
        assert!(original.preferences("tos", "si:other").await.is_err());
        assert!(switched.preferences("tos", "si:agent").await.is_err());
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
