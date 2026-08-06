use serde::de::DeserializeOwned;
use source_downloader_sdk::component::ComponentError;
use source_downloader_sdk::serde_json::{self, Map, Value};

pub(crate) fn parse<T: DeserializeOwned>(
    props: &Map<String, Value>,
    component_name: &str,
) -> Result<T, ComponentError> {
    serde_json::from_value(Value::Object(props.clone())).map_err(|error| {
        ComponentError::new(format!("Invalid {component_name} config: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use source_downloader_sdk::serde_json::json;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(rename_all = "kebab-case", deny_unknown_fields)]
    struct TestConfig {
        api_host: String,
        #[serde(default)]
        only_high_resolution: bool,
    }

    #[test]
    fn parse_maps_kebab_case_properties() {
        let props = serde_json::from_value(json!({
            "api-host": "https://example.test",
            "only-high-resolution": true
        }))
        .unwrap();

        let config: TestConfig = parse(&props, "test").unwrap();

        assert_eq!(
            config,
            TestConfig {
                api_host: "https://example.test".to_owned(),
                only_high_resolution: true,
            }
        );
    }

    #[test]
    fn parse_rejects_camel_case_aliases() {
        let props = serde_json::from_value(json!({
            "apiHost": "https://example.test"
        }))
        .unwrap();

        let error = parse::<TestConfig>(&props, "test").unwrap_err();

        assert!(error.to_string().contains("apiHost"));
    }
}
