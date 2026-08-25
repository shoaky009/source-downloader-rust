use crate::ApplicationContext;
use crate::error_handle::AppError;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use source_downloader_core::config::InstanceConfig;
use source_downloader_sdk::serde_json::{Map, Value};
use std::sync::Arc;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/instance",
            Router::new().route("/", get(query_instances).post(create_instance)).route(
                "/{name}",
                get(get_instance).put(update_instance).delete(delete_instance),
            ),
        )
        .with_state(ctx.core.clone())
}

async fn query_instances(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
) -> Json<Vec<InstanceInfo>> {
    let mut instances = core
        .config_operator
        .get_all_instance_config()
        .into_iter()
        .map(|config| {
            let loaded = core.instance_manager.is_instance_loaded(&config.name);
            InstanceInfo::from_config(config, loaded)
        })
        .collect::<Vec<_>>();
    instances.sort_by(|left, right| left.name.cmp(&right.name));
    Json(instances)
}

async fn get_instance(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
    Path(name): Path<String>,
) -> Result<Json<InstanceInfo>, AppError> {
    core.config_operator
        .get_all_instance_config()
        .into_iter()
        .find(|config| config.name == name)
        .map(|config| {
            Json(InstanceInfo::from_config(
                config,
                core.instance_manager.is_instance_loaded(&name),
            ))
        })
        .ok_or_else(|| AppError::NotFound(format!("Instance not found: {name}")))
}

async fn create_instance(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
    Json(request): Json<InstanceSaveRequest>,
) -> Result<StatusCode, AppError> {
    validate_name(&request.name)?;
    if core
        .config_operator
        .get_all_instance_config()
        .iter()
        .any(|config| config.name == request.name)
    {
        return Err(AppError::BadRequest(format!(
            "Instance '{}' already exists",
            request.name
        )));
    }
    save_instance(&core, request)?;
    Ok(StatusCode::CREATED)
}

async fn update_instance(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
    Path(name): Path<String>,
    Json(request): Json<InstanceUpdateRequest>,
) -> Result<StatusCode, AppError> {
    validate_name(&name)?;
    if !core
        .config_operator
        .get_all_instance_config()
        .iter()
        .any(|config| config.name == name)
    {
        return Err(AppError::NotFound(format!("Instance not found: {name}")));
    }
    save_instance(
        &core,
        InstanceSaveRequest {
            name: name.clone(),
            factory_type: request.factory_type,
            props: request.props,
        },
    )?;
    core.instance_manager.destroy_instance(&name);
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_instance(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    if !core.config_operator.delete_instance(&name)? {
        return Err(AppError::NotFound(format!("Instance not found: {name}")));
    }
    core.instance_manager.destroy_instance(&name);
    Ok(StatusCode::NO_CONTENT)
}

fn save_instance(
    core: &source_downloader_core::application::CoreApplication,
    request: InstanceSaveRequest,
) -> Result<(), AppError> {
    core.instance_manager.validate_instance(&request.factory_type, &request.props)?;
    core.config_operator
        .save_instance(InstanceConfig { name: request.name, props: request.props })?;
    Ok(())
}

fn validate_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(AppError::BadRequest("Instance name must not be empty".to_owned()));
    }
    if name.contains(':') {
        return Err(AppError::BadRequest(
            "Instance name must not contain ':'".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstanceSaveRequest {
    name: String,
    #[serde(rename = "type")]
    factory_type: String,
    #[serde(default)]
    props: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstanceUpdateRequest {
    #[serde(rename = "type")]
    factory_type: String,
    #[serde(default)]
    props: Map<String, Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceInfo {
    name: String,
    props: Map<String, Value>,
    loaded: bool,
}

impl InstanceInfo {
    fn from_config(config: InstanceConfig, loaded: bool) -> Self {
        Self { name: config.name, props: config.props, loaded }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_name;

    #[test]
    fn instance_name_rejects_empty_and_separator() {
        assert!(validate_name(" ").is_err());
        assert!(validate_name("client:one").is_err());
        assert!(validate_name("client-one").is_ok());
    }
}
