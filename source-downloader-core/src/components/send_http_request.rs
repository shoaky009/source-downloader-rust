use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContentStatus, ItemContent,
    ProcessContext, ProcessListener, ProcessingError, ProcessorInfo, SdComponent,
    SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

pub struct SendHttpRequestSupplier;
pub const SUPPLIER: SendHttpRequestSupplier = SendHttpRequestSupplier;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct HttpRequestConfig {
    url: String,
    #[serde(default = "default_method")]
    method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    body: Option<String>,
    #[serde(default, alias = "withContentBody")]
    with_content_body: bool,
}

fn default_method() -> String {
    String::from("POST")
}

impl ComponentSupplier for SendHttpRequestSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::listener("http".to_owned())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: HttpRequestConfig =
            serde_json::from_value(Value::Object(props.clone())).map_err(|error| {
                ComponentError::new(format!("Invalid HTTP request config: {error}"))
            })?;
        match config.method.as_str() {
            "GET" | "POST" | "PUT" | "DELETE" => {}
            _ => {
                return Err(ComponentError::new(format!(
                    "Invalid HTTP request method '{}'",
                    config.method
                )));
            }
        }
        reqwest::Url::parse(&config.url).map_err(|error| {
            ComponentError::new(format!("Invalid HTTP request URL: {error}"))
        })?;
        Ok(Arc::new(SendHttpRequest { config, client: reqwest::Client::new() }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(ProcessListener)]
pub struct SendHttpRequest {
    config: HttpRequestConfig,
    client: reqwest::Client,
}

impl Display for SendHttpRequest {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("http")
    }
}

impl SendHttpRequest {
    fn send(
        &self,
        url: String,
        headers: HashMap<String, String>,
        body: Option<String>,
        timeout: Option<Duration>,
    ) -> Result<reqwest::StatusCode, ProcessingError> {
        let client = self.client.clone();
        let method = self
            .config
            .method
            .parse::<reqwest::Method>()
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))?;
        block_on_sync(async move {
            let mut request = client.request(method, url);
            if let Some(timeout) = timeout {
                request = request.timeout(timeout);
            }
            for (name, value) in headers {
                request = request.header(name, value);
            }
            if let Some(body) = body {
                request = request.body(body);
            }
            let response = request
                .send()
                .await
                .map_err(|error| ProcessingError::non_retryable(error.to_string()))?;
            Ok(response.status())
        })
    }

    fn build_url(&self, vars: &[(&str, &str)]) -> String {
        let mut parts = self.config.url.splitn(2, '?');
        let base = parts.next().unwrap_or_default();
        let Some(query) = parts.next() else {
            return base.to_owned();
        };
        let query = query
            .split('&')
            .enumerate()
            .map(|(index, key_value)| {
                let mut pair = key_value.split('=');
                let name = pair.next().unwrap_or_default();
                let mut value = pair.next().unwrap_or_default().to_owned();
                for (key, replacement) in vars {
                    value = value.replace(&format!("{{{key}}}"), replacement);
                }
                let prefix = if index == 0 { '?' } else { '&' };
                format!("{prefix}{name}={}", escape_fragment(&value))
            })
            .collect::<String>();
        format!("{base}{query}")
    }

    fn item_request(
        &self,
        context: &dyn ProcessContext,
        item_content: &ItemContent,
    ) -> Result<(String, HashMap<String, String>, Option<String>), ProcessingError> {
        let summary = summary_content(item_content);
        let url = self.build_url(&[("summary", &summary)]);
        let mut headers = self.config.headers.clone();
        let body =
            if self.config.body.as_deref().is_some_and(|body| !body.trim().is_empty()) {
                Some(
                    self.config
                        .body
                        .as_deref()
                        .unwrap_or_default()
                        .replace("{summary}", &summary),
                )
            } else if self.config.with_content_body {
                headers.insert(
                    String::from("Content-Type"),
                    String::from("application/json"),
                );
                Some(
                    serde_json::to_string(&serde_json::json!({
                        "content": item_content_value(item_content),
                        "processor": processor_value(context.processor()),
                    }))
                    .map_err(|error| ProcessingError::non_retryable(error.to_string()))?,
                )
            } else {
                None
            };
        Ok((url, headers, body))
    }
}

impl ProcessListener for SendHttpRequest {
    fn on_item_success(
        &self,
        context: &dyn ProcessContext,
        item_content: &ItemContent,
    ) -> Result<(), ProcessingError> {
        let (url, headers, body) = self.item_request(context, item_content)?;
        let status =
            self.send(url.clone(), headers, body, Some(Duration::from_secs(30)))?;
        if !status.is_success() {
            tracing::warn!(url = %url, status = %status, "HTTP request returned a non-2xx status");
        }
        Ok(())
    }

    fn on_item_error(
        &self,
        _: &dyn ProcessContext,
        _: &SourceItem,
        _: &ProcessingError,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    fn on_process_completed(
        &self,
        context: &dyn ProcessContext,
    ) -> Result<(), ProcessingError> {
        let size = context.processed_items().len();
        let summary = format!("Processed {size} items");
        let url = self.build_url(&[("summary", &summary)]);
        let mut headers = self.config.headers.clone();
        let contents = context
            .processed_items()
            .map(|item| {
                context
                    .get_item_content(item)
                    .map(|content| item_content_value_from_in_processing(&content))
            })
            .collect::<Vec<_>>();
        let body =
            if self.config.body.as_deref().is_some_and(|body| !body.trim().is_empty()) {
                Some(
                    self.config
                        .body
                        .as_deref()
                        .unwrap_or_default()
                        .replace("{summary}", &summary),
                )
            } else if self.config.with_content_body {
                headers.insert(
                    String::from("Content-Type"),
                    String::from("application/json"),
                );
                Some(
                    serde_json::to_string(&serde_json::json!({
                        "contents": contents,
                        "processor": processor_value(context.processor()),
                    }))
                    .map_err(|error| ProcessingError::non_retryable(error.to_string()))?,
                )
            } else {
                None
            };
        let status = self.send(url.clone(), headers, body, None)?;
        if !status.is_success() {
            tracing::warn!(url = %url, status = %status, "HTTP request returned a non-2xx status");
        }
        Ok(())
    }
}

fn block_on_sync<F, T>(future: F) -> Result<T, ProcessingError>
where
    F: Future<Output = Result<T, ProcessingError>> + Send,
    T: Send,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        std::thread::scope(|scope| {
            scope.spawn(move || handle.block_on(future)).join().map_err(|_| {
                ProcessingError::non_retryable("HTTP request thread panicked")
            })?
        })
    } else {
        tokio::runtime::Runtime::new()
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))?
            .block_on(future)
    }
}

fn escape_fragment(value: &str) -> String {
    const SAFE: &[u8] = b"-._~!$&'()*+,;=:@/?#[]";
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || SAFE.contains(byte) {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

fn summary_content(item_content: &ItemContent) -> String {
    if item_content.file_contents.len() == 1
        && matches!(
            item_content.file_contents[0].status,
            FileContentStatus::Normal
                | FileContentStatus::Replaced
                | FileContentStatus::Replace
        )
        && let Some(name) = item_content.file_contents[0].target_path().file_name()
    {
        return format!("{} 处理完成", name.to_string_lossy());
    }

    let has_warning = item_content.file_contents.iter().any(|file| {
        matches!(
            file.status,
            FileContentStatus::VariableError
                | FileContentStatus::TargetExists
                | FileContentStatus::FileConflict
        )
    });
    if has_warning {
        let mut groups: Vec<(FileContentStatus, usize)> = Vec::new();
        for file in item_content.file_contents {
            let status = file.status.clone();
            if let Some((_, count)) =
                groups.iter_mut().find(|(known, _)| *known == status)
            {
                *count += 1;
            } else {
                groups.push((status, 1));
            }
        }
        let status_summary = groups
            .iter()
            .map(|(status, count)| format!("{}:{}个", status_name(status), count))
            .collect::<Vec<_>>()
            .join(",");
        return format!(
            "{}内的{}个文件处理完成 {}",
            item_content.source_item.title,
            item_content.file_contents.len(),
            status_summary
        );
    }

    format!(
        "{}内的{}个文件处理完成",
        item_content.source_item.title,
        item_content.file_contents.len()
    )
}

fn status_name(status: &FileContentStatus) -> &'static str {
    match status {
        FileContentStatus::Undetected => "UNDETECTED",
        FileContentStatus::Normal => "NORMAL",
        FileContentStatus::Downloaded => "DOWNLOADED",
        FileContentStatus::VariableError => "VARIABLE_ERROR",
        FileContentStatus::TargetExists => "TARGET_EXISTS",
        FileContentStatus::FileConflict => "FILE_CONFLICT",
        FileContentStatus::ReadyReplace => "READY_REPLACE",
        FileContentStatus::Replaced => "REPLACED",
        FileContentStatus::Replace => "REPLACE",
    }
}

fn item_content_value(item: &ItemContent) -> Value {
    serde_json::json!({
        "sourceItem": item.source_item,
        "fileContents": item.file_contents,
        "itemVariables": item.item_variables,
        "status": item.status,
    })
}

fn item_content_value_from_in_processing(
    content: &source_downloader_sdk::component::InProcessingItem<'_>,
) -> Value {
    serde_json::json!({
        "sourceItem": content.source_item,
        "fileContents": content.file_contents,
        "itemVariables": content.item_variables,
        "status": content.status,
    })
}

fn processor_value(processor: &ProcessorInfo) -> Value {
    serde_json::json!({
        "name": &processor.name,
        "downloadPath": &processor.download_path,
        "sourceSavePath": &processor.source_save_path,
        "tags": processor.tags.iter().collect::<Vec<_>>(),
        "category": &processor.category,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str) -> SendHttpRequest {
        SendHttpRequest {
            config: HttpRequestConfig {
                url: String::from(url),
                method: String::from("GET"),
                headers: HashMap::new(),
                body: None,
                with_content_body: false,
            },
            client: reqwest::Client::new(),
        }
    }

    #[test]
    fn build_url_replaces_query_variables_and_escapes_values() {
        let request =
            request("https://example.test/notify?summary={summary}&fixed=value");

        assert_eq!(
            request.build_url(&[("summary", "a b%")]),
            "https://example.test/notify?summary=a%20b%25&fixed=value"
        );
    }

    #[test]
    fn supplier_rejects_unsupported_http_methods() {
        let props = serde_json::json!({
            "url": "https://example.test/notify",
            "method": "PATCH"
        })
        .as_object()
        .unwrap()
        .clone();

        let error = SUPPLIER.apply(&props).unwrap_err();

        assert!(error.to_string().contains("Invalid HTTP request method"));
    }

    #[test]
    fn supplier_uses_http_component_for_valid_configuration() {
        let props = serde_json::json!({"url": "https://example.test/notify"})
            .as_object()
            .unwrap()
            .clone();

        let component = SUPPLIER.apply(&props).unwrap();

        assert_eq!(component.to_string(), "http");
    }
}
