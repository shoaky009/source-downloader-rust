use super::{Question, Session};
use serde_json::{Value, json};
use source_downloader_core::application::CoreApplication;
use source_downloader_core::config_agent::{ConfigurationPlan, configuration_snapshot};
use source_downloader_sdk::component::{ComponentRootType, ComponentType};

pub const INSTRUCTIONS: &str = include_str!("instructions.md");

pub fn definitions() -> Vec<Value> {
    vec![
        function(
            "catalog",
            "List registered component and instance capabilities and existing configuration. Always consult before planning.",
            json!({"type":"object","properties":{}}),
        ),
        function(
            "inspect_capability",
            "Get authoritative input props schema, outputSchema (variables/attributes/tags and availability), and compatibility rules. Inspect component outputs before using variables or tags; missing outputSchema means undocumented, never invent outputs. kind=processor includes execution stages and template rules. kind=recipes is optional composition examples, not a component contract. For component pass rootType and typeName; for instance typeName is the exact factory name.",
            json!({"type":"object","properties":{"kind":{"type":"string","enum":["component","instance","processor","recipes"]},"rootType":{"type":"string"},"typeName":{"type":"string"}},"required":["kind"]}),
        ),
        function(
            "ask_user",
            "Pause to ask essential questions. Each question has id, title, options (strings, possibly empty), secret (boolean). Secret answers are stored locally; use the returned $secret:id reference as the complete property value. For secret URLs ask for the whole URL.",
            json!({"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":5,"items":{"type":"object","properties":{"id":{"type":"string"},"title":{"type":"string"},"options":{"type":"array","items":{"type":"string"}},"secret":{"type":"boolean"}},"required":["id","title","options","secret"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}),
        ),
        function(
            "propose_plan",
            "Validate and present a complete plan for user review. DOES NOT create anything. configuration is a JSON string with summary, instances [{name,factoryType,props}], components [{rootType,typeName,name,props}], processors [native kebab-case ProcessorConfig]. Include only NEW resources. Reference existing resources. Processors default enabled. All instances and components are created in list order before processors, after explicit UI approval only.",
            json!({"type":"object","properties":{"configuration":{"type":"string"}},"required":["configuration"],"additionalProperties":false}),
        ),
    ]
}

fn function(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type":"function", "name":name,"description":description,"parameters":parameters,"strict":false})
}

pub fn execute(
    core: &CoreApplication,
    session: &mut Session,
    name: &str,
    arguments: &str,
) -> Result<(Value, bool), String> {
    let args: Value = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid tool arguments: {e}"))?;
    match name {
        "catalog" => {
            let components: Vec<_> = core.component_manager.get_all_suppliers().iter().map(|supplier| {
                json!({"types":supplier.supply_types(),"noProps":supplier.is_support_no_props(),"description":supplier.get_metadata().map(|m| m.description)})
            }).collect();
            let instances: Vec<_> = core.instance_manager.get_instance_factories().iter().map(|factory| {
                json!({"typeName":factory.factory_name(),"metadata":factory.get_metadata()})
            }).collect();
            Ok((
                json!({"components":components,"instances":instances,"existing":redact(configuration_snapshot(core))}),
                false,
            ))
        }
        "inspect_capability" => {
            let value = match args["kind"].as_str() {
                Some("processor") => {
                    let mut metadata: Value = serde_json::from_str(include_str!(
                        "../../../resources/processor-metadata.json"
                    ))
                    .map_err(|e| e.to_string())?;
                    metadata["processingGuide"] =
                        json!(include_str!("processing-guide.md"));
                    metadata
                }
                Some("recipes") => json!({"guide":include_str!("recipes.md")}),
                Some("component") => {
                    let root = ComponentRootType::parse(
                        args["rootType"].as_str().ok_or("rootType required")?,
                    )
                    .map_err(|e| e.message)?;
                    let ty = ComponentType {
                        root_type: root,
                        name: args["typeName"]
                            .as_str()
                            .ok_or("typeName required")?
                            .into(),
                    };
                    let supplier = core
                        .component_manager
                        .get_supplier(&ty)
                        .ok_or("Component not registered")?;
                    json!({"types":supplier.supply_types(),"metadata":supplier.get_metadata(),"compatibility":core.component_manager.get_compatibility_rules(&ty)})
                }
                Some("instance") => {
                    let factory = core
                        .instance_manager
                        .get_factory_by_name(
                            args["typeName"].as_str().ok_or("typeName required")?,
                        )
                        .ok_or("Instance factory not registered")?;
                    json!({"typeName":factory.factory_name(),"metadata":factory.get_metadata()})
                }
                _ => return Err("Unknown capability kind".into()),
            };
            Ok((value, false))
        }
        "ask_user" => {
            let questions: Vec<Question> =
                serde_json::from_value(args["questions"].clone())
                    .map_err(|e| e.to_string())?;
            if questions.is_empty() || questions.len() > 5 {
                return Err("Ask between one and five questions".into());
            }
            let mut ids = std::collections::HashSet::new();
            for question in &questions {
                if question.id.is_empty()
                    || question.title.trim().is_empty()
                    || !ids.insert(question.id.clone())
                {
                    return Err("Questions require unique nonempty ids and titles".into());
                }
            }
            session.questions = questions;
            Ok((json!({"status":"waiting_for_user"}), true))
        }
        "propose_plan" => {
            let mut plan: ConfigurationPlan = serde_json::from_str(
                args["configuration"]
                    .as_str()
                    .ok_or("configuration JSON string required")?,
            )
            .map_err(|e| e.to_string())?;
            plan.normalize();
            let resolved = session.resolve_plan(&plan)?;
            resolved.validate(core)?;
            resolved.check_new_resources(core)?;
            session.revision += 1;
            session.plan = Some(plan);
            session.snapshot = Some(configuration_snapshot(core));
            session.questions.clear();
            session.steps.clear();
            session.status = "review".into();
            Ok((
                json!({"status":"awaiting_explicit_user_review","revision":session.revision}),
                true,
            ))
        }
        _ => Err(format!(
            "Unknown tool {name}. Only catalog, inspect_capability, ask_user and propose_plan are available before approval."
        )),
    }
}

pub fn redact(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                let key = key.to_lowercase();
                if [
                    "password",
                    "cookie",
                    "token",
                    "secret",
                    "authorization",
                    "api-key",
                    "apikey",
                    "api_key",
                    "api-hash",
                ]
                .iter()
                .any(|s| key.contains(s))
                {
                    *value = json!("[已配置]");
                } else {
                    *value = redact(value.take());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                *value = redact(value.take());
            }
        }
        Value::String(text) => {
            if let Ok(mut url) = reqwest::Url::parse(text)
                && matches!(url.scheme(), "http" | "https" | "socks5")
            {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                if url.query().is_some() {
                    url.set_query(Some("redacted"));
                }
                *text = url.to_string();
            }
        }
        _ => {}
    }
    value
}
