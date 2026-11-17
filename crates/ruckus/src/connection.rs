//! Validate optional login without storing credentials in the conversation model.

use ruckus_core::client::SaslCredentials;

pub(crate) fn sasl_credentials(
    account: &str,
    password: &str,
    tls: bool,
) -> Result<Option<SaslCredentials>, &'static str> {
    if account.is_empty() && password.is_empty() {
        return Ok(None);
    }
    if !tls {
        return Err("SASL login requires TLS. Enable TLS before connecting.");
    }
    if account.is_empty() || password.is_empty() {
        return Err("Enter both a SASL account and password, or leave both blank.");
    }
    Ok(Some(SaslCredentials {
        username: account.into(),
        password: password.into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_login_requires_a_complete_pair_over_tls() {
        assert!(sasl_credentials("", "", false).unwrap().is_none());
        assert!(sasl_credentials("account", "secret", false).is_err());
        assert!(sasl_credentials("account", "", true).is_err());
        assert!(sasl_credentials("", "secret", true).is_err());
        let credentials = sasl_credentials("account", "secret", true)
            .unwrap()
            .unwrap();
        assert_eq!(credentials.username, "account");
        assert_eq!(credentials.password, "secret");
        for error in [
            sasl_credentials("secret-account", "secret-password", false).unwrap_err(),
            sasl_credentials("", "secret-password", true).unwrap_err(),
        ] {
            assert!(!error.contains("secret"));
        }
    }
}
