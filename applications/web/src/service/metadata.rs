use crate::ApplicationContext;
use crate::error_handle::AppError;
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use source_downloader_core::compatibility::COMPATIBILITY_DSL_VERSION;
use source_downloader_sdk::component::{
    ComponentCompatibilityRule, ComponentRootType, ComponentType, SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Value, from_str};
use std::sync::{Arc, LazyLock};

static PROCESSOR_METADATA: LazyLock<Value> = LazyLock::new(|| {
    from_str(include_str!("../../resources/processor-metadata.json"))
        .expect("bundled processor metadata must be valid JSON")
});

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/metadata",
            Router::new()
                .route("/processor", get(processor_metadata))
                .route("/component-root-types", get(component_root_types))
                .route("/component-capabilities", get(component_capabilities))
                .route(
                    "/component-compatibility-rules",
                    get(component_compatibility_rules),
                )
                .route(
                    "/component-capabilities/{root_type}/{type_name}",
                    get(component_capability),
                )
                .route("/instance-capabilities", get(instance_capabilities))
                .route("/instance-capabilities/{type_name}", get(instance_capability)),
        )
        .with_state(ctx.core.clone())
}

async fn processor_metadata() -> Json<Value> {
    Json(PROCESSOR_METADATA.clone())
}

async fn component_root_types() -> Json<Vec<ComponentRootTypeMetadata>> {
    Json(
        ROOT_TYPES
            .iter()
            .map(|(root_type, component_interface, description)| {
                ComponentRootTypeMetadata {
                    root_type: root_type.clone(),
                    primary_name: root_type.name(),
                    aliases: Vec::new(),
                    component_interface,
                    description,
                }
            })
            .collect(),
    )
}

async fn component_capabilities(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
) -> Json<Vec<ComponentCapabilitySummary>> {
    Json(
        component_capability_details(&core)
            .into_iter()
            .map(ComponentCapabilitySummary::from)
            .collect(),
    )
}

async fn component_capability(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
    Path((root_type, type_name)): Path<(ComponentRootType, String)>,
) -> Result<Json<ComponentCapabilityDetail>, AppError> {
    component_capability_details(&core)
        .into_iter()
        .find(|detail| {
            detail.types.iter().any(|component_type| {
                component_type.root_type == root_type
                    && component_type.type_name == type_name
            })
        })
        .map(Json)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "Component capability not found: {root_type}:{type_name}"
            ))
        })
}

async fn instance_capabilities(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
) -> Json<Vec<InstanceCapabilitySummary>> {
    let mut capabilities = core
        .instance_manager
        .get_instance_factories()
        .into_iter()
        .map(|factory| {
            let type_name = factory.factory_name();
            InstanceCapabilitySummary {
                simple_name: simple_type_name(&type_name).to_owned(),
                type_name,
                description: None,
            }
        })
        .collect::<Vec<_>>();
    capabilities.sort_by(|left, right| left.type_name.cmp(&right.type_name));
    Json(capabilities)
}

async fn instance_capability(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
    Path(type_name): Path<String>,
) -> Result<Json<InstanceCapabilityDetail>, AppError> {
    core.instance_manager
        .get_instance_factories()
        .into_iter()
        .find(|factory| factory.factory_name() == type_name)
        .map(|factory| {
            let type_name = factory.factory_name();
            Json(InstanceCapabilityDetail {
                simple_name: simple_type_name(&type_name).to_owned(),
                type_name,
                description: None,
                metadata: None,
            })
        })
        .ok_or_else(|| {
            AppError::NotFound(format!("Instance capability not found: {type_name}"))
        })
}

async fn component_compatibility_rules(
    State(core): State<Arc<source_downloader_core::application::CoreApplication>>,
) -> Json<ComponentCompatibilityRules> {
    let mut rules = core.component_manager.get_all_compatibility_rules();
    rules.sort_by(|left, right| left.code.cmp(&right.code));
    Json(ComponentCompatibilityRules { dsl_version: COMPATIBILITY_DSL_VERSION, rules })
}

fn component_capability_details(
    core: &source_downloader_core::application::CoreApplication,
) -> Vec<ComponentCapabilityDetail> {
    let mut details = core
        .component_manager
        .get_all_suppliers()
        .into_iter()
        .map(|supplier| {
            let metadata = supplier.get_metadata();
            let description =
                metadata.as_ref().map(|metadata| metadata.description.clone());
            ComponentCapabilityDetail {
                support_no_args: supplier.is_support_no_props(),
                types: supplier
                    .supply_types()
                    .into_iter()
                    .map(ComponentCapabilityType::from)
                    .collect(),
                description,
                metadata,
            }
        })
        .collect::<Vec<_>>();
    details.sort_by(|left, right| {
        let left = left.types.first().map(|value| value.full_name.as_str()).unwrap_or("");
        let right =
            right.types.first().map(|value| value.full_name.as_str()).unwrap_or("");
        left.cmp(right)
    });
    details
}

fn simple_type_name(type_name: &str) -> &str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentRootTypeMetadata {
    root_type: ComponentRootType,
    primary_name: &'static str,
    aliases: Vec<&'static str>,
    component_interface: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentCapabilitySummary {
    support_no_args: bool,
    types: Vec<ComponentCapabilityType>,
    description: Option<String>,
}

impl From<ComponentCapabilityDetail> for ComponentCapabilitySummary {
    fn from(detail: ComponentCapabilityDetail) -> Self {
        Self {
            support_no_args: detail.support_no_args,
            types: detail.types,
            description: detail.description,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentCapabilityDetail {
    support_no_args: bool,
    types: Vec<ComponentCapabilityType>,
    description: Option<String>,
    metadata: Option<Box<SdComponentMetadata>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentCompatibilityRules {
    dsl_version: u32,
    rules: Vec<ComponentCompatibilityRule>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentCapabilityType {
    root_type: ComponentRootType,
    type_name: String,
    full_name: String,
}

impl From<ComponentType> for ComponentCapabilityType {
    fn from(component_type: ComponentType) -> Self {
        Self {
            full_name: component_type.to_string(),
            root_type: component_type.root_type,
            type_name: component_type.name,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceCapabilitySummary {
    #[serde(rename = "type")]
    type_name: String,
    simple_name: String,
    description: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstanceCapabilityDetail {
    #[serde(rename = "type")]
    type_name: String,
    simple_name: String,
    description: Option<String>,
    metadata: Option<SdComponentMetadata>,
}

const ROOT_TYPES: &[(ComponentRootType, &str, &str)] = &[
    (
        ComponentRootType::Trigger,
        "Trigger",
        "负责触发 processor 执行的组件，例如 cron、fixed。",
    ),
    (ComponentRootType::Source, "Source", "负责从外部系统拉取或遍历条目数据的组件。"),
    (ComponentRootType::Downloader, "Downloader", "负责下载、导入或提交文件任务的组件。"),
    (
        ComponentRootType::ItemFileResolver,
        "ItemFileResolver",
        "负责把 source item 解析成一个或多个 SourceFile 的组件。",
    ),
    (
        ComponentRootType::FileMover,
        "FileMover",
        "负责把下载结果从 download path 落到 save path 的组件。",
    ),
    (
        ComponentRootType::VariableProvider,
        "VariableProvider",
        "负责为命名模板和规则提供变量的组件。",
    ),
    (
        ComponentRootType::ProcessListener,
        "ProcessListener",
        "负责在处理过程中或处理后执行附加动作的监听组件。",
    ),
    (
        ComponentRootType::SourceItemFilter,
        "SourceItemFilter",
        "负责按 item 维度筛选条目的组件。",
    ),
    (
        ComponentRootType::SourceFileFilter,
        "SourceFileFilter",
        "负责按 source file 维度筛选文件的组件。",
    ),
    (
        ComponentRootType::ItemContentFilter,
        "ItemContentFilter",
        "负责按 item content 维度过滤处理结果的组件。",
    ),
    (
        ComponentRootType::FileContentFilter,
        "FileContentFilter",
        "负责按 file content 维度过滤文件结果的组件。",
    ),
    (ComponentRootType::FileTagger, "FileTagger", "负责为文件内容打标签的组件。"),
    (
        ComponentRootType::FileReplacementDecider,
        "FileReplacementDecider",
        "负责目标文件已存在时决定是否允许替换的组件。",
    ),
    (
        ComponentRootType::FileExistsDetector,
        "FileExistsDetector",
        "负责补充检测目标文件是否已存在的组件。",
    ),
    (
        ComponentRootType::VariableReplacer,
        "VariableReplacer",
        "负责对命名变量进行替换和清洗的组件。",
    ),
    (ComponentRootType::Trimmer, "Trimmer", "负责对变量值进行裁剪、去噪或规整的组件。"),
];
