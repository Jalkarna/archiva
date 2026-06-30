use crate::core::error::{ArchivaError, Result};
use crate::core::json::{self, JsonValue};

pub const APPLICATION_NAME: &str = "archiva";
pub const APPLICATION_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const NPM_PACKAGE_NAME: &str = "@jalkarna/archiva";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
pub const DLOG_SCHEMA_VERSION: u32 = 1;

pub fn package_json_version(input: &str) -> Result<String> {
    let value = json::parse(input)?;
    let JsonValue::Object(object) = value else {
        return Err(ArchivaError::schema("package.json", "expected object"));
    };

    let name = object
        .get("name")
        .ok_or_else(|| ArchivaError::schema("package.json.name", "missing required field"))?;
    if name != &JsonValue::String(NPM_PACKAGE_NAME.to_string()) {
        return Err(ArchivaError::schema(
            "package.json.name",
            format!("expected {NPM_PACKAGE_NAME}"),
        ));
    }

    match object.get("version") {
        Some(JsonValue::String(version)) if !version.is_empty() => Ok(version.clone()),
        Some(_) => Err(ArchivaError::schema(
            "package.json.version",
            "expected non-empty string",
        )),
        None => Err(ArchivaError::schema(
            "package.json.version",
            "missing required field",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        package_json_version, APPLICATION_NAME, APPLICATION_VERSION, DLOG_SCHEMA_VERSION,
        MCP_PROTOCOL_VERSION,
    };

    #[test]
    fn exposes_compile_time_application_and_protocol_versions() {
        assert_eq!(APPLICATION_NAME, "archiva");
        assert!(!APPLICATION_VERSION.is_empty());
        assert_eq!(MCP_PROTOCOL_VERSION, "2024-11-05");
        assert_eq!(DLOG_SCHEMA_VERSION, 1);
    }

    #[test]
    fn cargo_package_version_matches_npm_package_version() {
        let npm_version = package_json_version(include_str!("../../package.json")).unwrap();
        assert_eq!(APPLICATION_VERSION, npm_version);
    }

    #[test]
    fn validates_package_json_identity_before_returning_version() {
        assert_eq!(
            package_json_version(r#"{"name":"@jalkarna/archiva","version":"0.2.0"}"#).unwrap(),
            "0.2.0"
        );
        assert!(
            package_json_version(r#"{"name":"other","version":"0.2.0"}"#)
                .unwrap_err()
                .user_message()
                .contains("package.json.name")
        );
        assert!(
            package_json_version(r#"{"name":"@jalkarna/archiva","version":1}"#)
                .unwrap_err()
                .user_message()
                .contains("package.json.version")
        );
    }
}
