use indexmap::IndexMap;
#[allow(dead_code, unused)]
use moka::sync::Cache;
use serde::{Deserialize, Deserializer, Serialize};
use source_downloader_sdk::component::{ComponentError, ComponentRootType, ComponentType};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use std::{env, fs};
use tracing::{error, info};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub name: String,
    pub props: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub component_type: String,
    #[serde(default)]
    pub props: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ProcessorConfig {
    /// 处理器名称
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub save_path: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    pub source: String,
    pub item_file_resolver: String,
    pub downloader: String,
    pub file_mover: String,
    #[serde(default)]
    pub options: ProcessorOptionConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub tags: HashSet<String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ProcessorOptionConfig {
    #[serde(skip_serializing_if = "is_default")]
    pub save_path_pattern: String,
    #[serde(skip_serializing_if = "is_default")]
    pub filename_pattern: String,
    #[serde(skip_serializing_if = "is_default")]
    pub variable_providers: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub item_filters: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub item_expression_exclusions: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub item_expression_inclusions: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub item_content_expression_exclusions: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub item_content_expression_inclusions: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub source_file_filters: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub file_content_filters: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub file_content_expression_exclusions: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub file_content_expression_inclusions: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub file_taggers: Vec<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub variable_conflict_strategy: Option<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub variable_name_replace: HashMap<String, String>,
    #[serde(skip_serializing_if = "Clone::clone")]
    pub save_processing_content: bool,
    #[serde(skip_serializing_if = "is_rename_task_interval_default")]
    pub rename_task_interval: String,
    #[serde(skip_serializing_if = "is_rename_times_threshold_default")]
    pub rename_times_threshold: u32,
    #[serde(skip_serializing_if = "is_parallelism_default")]
    pub parallelism: u32,
    #[serde(skip_serializing_if = "is_default")]
    pub task_group: Option<String>,
    #[serde(skip_serializing_if = "is_fetch_limit_default")]
    pub fetch_limit: u32,
    #[serde(skip_serializing_if = "is_pointer_batch_mode_default")]
    pub pointer_batch_mode: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub item_error_continue: bool,
    // 后面改名字 -> item_rule
    #[serde(skip_serializing_if = "is_default")]
    pub item_grouping: Vec<ItemRuleConfig>,
    // 后面改名字 -> file_rule
    #[serde(skip_serializing_if = "is_default")]
    pub file_grouping: Vec<FileRuleConfig>,
    pub download_options: DownloadOptions,
    #[serde(skip_serializing_if = "is_default")]
    pub process_listeners: Vec<ListenerConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ItemRuleConfig {
    pub tags: Option<HashSet<String>>,
    pub expression_matching: Option<String>,
    pub filename_pattern: Option<String>,
    pub save_path_pattern: Option<String>,
    pub variable_providers: Option<Vec<String>>,
    pub source_item_filters: Option<Vec<String>>,
    pub item_expression_exclusions: Option<Vec<String>>,
    pub item_expression_inclusions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct FileRuleConfig {
    pub tags: Option<HashSet<String>>,
    pub expression_matching: Option<String>,
    pub filename_pattern: Option<String>,
    pub save_path_pattern: Option<String>,
    pub file_content_filters: Option<Vec<String>>,
    pub file_content_expression_exclusions: Option<Vec<String>>,
    pub file_content_expression_inclusions: Option<Vec<String>>,
    // pub file_replacement_decider: Option<String>
}

#[derive(Debug, Deserialize, Clone, Serialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ListenerMode {
    Each,
    Batch,
}

impl Default for ListenerMode {
    fn default() -> Self {
        Self::Each
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ListenerConfig {
    pub id: String,
    pub mode: ListenerMode,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ListenerConfigWire {
    // 对应 - "test"
    Simple(String),
    // 对应 - id: "test1", mode: BATCH
    Full {
        id: String,
        #[serde(default)]
        mode: ListenerMode,
    },
    // 对应 - "http:test3": BATCH
    Map(BTreeMap<String, ListenerMode>),
}

impl<'de> Deserialize<'de> for ListenerConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // 步骤 A: 先让 Serde 解析到 Wire 枚举
        let wire = ListenerConfigWire::deserialize(deserializer)?;
        // 步骤 B: 根据匹配到的变体，手动构造 ListenerConfig
        let config = match wire {
            ListenerConfigWire::Simple(id) => ListenerConfig {
                id,
                mode: ListenerMode::Each,
            },
            ListenerConfigWire::Full { id, mode } => ListenerConfig { id, mode },
            ListenerConfigWire::Map(map) => {
                let (id, mode) = map
                    .into_iter()
                    .next()
                    .ok_or_else(|| serde::de::Error::custom("Config map cannot be empty"))?;
                ListenerConfig { id, mode }
            }
        };
        Ok(config)
    }
}

fn is_rename_times_threshold_default(value: &u32) -> bool {
    *value == ProcessorOptionConfig::default().rename_times_threshold
}

fn is_rename_task_interval_default(value: &String) -> bool {
    *value == ProcessorOptionConfig::default().rename_task_interval
}

fn is_parallelism_default(value: &u32) -> bool {
    *value == ProcessorOptionConfig::default().parallelism
}

fn is_fetch_limit_default(value: &u32) -> bool {
    *value == ProcessorOptionConfig::default().fetch_limit
}

fn is_pointer_batch_mode_default(value: &bool) -> bool {
    *value == ProcessorOptionConfig::default().pointer_batch_mode
}

fn is_default<T: PartialEq + Default>(val: &T) -> bool {
    val == &T::default()
}

impl Default for ProcessorOptionConfig {
    fn default() -> Self {
        ProcessorOptionConfig {
            save_path_pattern: "".to_string(),
            filename_pattern: "".to_string(),
            variable_providers: vec![],
            item_filters: vec![],
            item_expression_exclusions: vec![],
            item_expression_inclusions: vec![],
            item_content_expression_exclusions: vec![],
            item_content_expression_inclusions: vec![],
            source_file_filters: vec![],
            file_content_filters: vec![],
            file_content_expression_exclusions: vec![],
            file_content_expression_inclusions: vec![],
            file_taggers: vec![],
            variable_name_replace: HashMap::new(),
            variable_conflict_strategy: None,
            save_processing_content: true,
            rename_task_interval: "5m".to_string(),
            rename_times_threshold: 3,
            parallelism: 1,
            task_group: None,
            fetch_limit: 50,
            pointer_batch_mode: true,
            item_error_continue: false,
            item_grouping: vec![],
            file_grouping: vec![],
            download_options: DownloadOptions::default(),
            process_listeners: vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct DownloadOptions {
    #[serde(default, skip_serializing_if = "is_default")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tags: HashSet<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct Properties {
    pub inner: Map<String, Value>,
}

#[allow(dead_code, unused)]
impl Properties {
    pub fn new() -> Self {
        Properties { inner: Map::new() }
    }

    pub fn from_map(map: Map<String, Value>) -> Self {
        Properties { inner: map }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.inner.get(key)
    }
}

#[allow(dead_code, unused)]
pub trait ConfigOperator: Send + Sync {
    fn get_processor_config(&self, name: &str) -> Option<ProcessorConfig>;

    fn get_all_processor_config(&self) -> Vec<ProcessorConfig>;

    fn get_all_component_config(&self) -> IndexMap<String, Vec<ComponentConfig>>;

    fn save_component(
        &self,
        root_type: &ComponentRootType,
        component_config: ComponentConfig,
    ) -> Result<(), ComponentError>;

    fn save_processor(&self, processor_config: ProcessorConfig) -> Result<(), ComponentError>;

    fn delete_component(
        &self,
        root_type: &ComponentRootType,
        component_type: &str,
        name: &str,
    ) -> Result<(), ComponentError>;

    fn delete_processor(&self, name: &str) -> Result<bool, ComponentError>;

    fn get_instance_props(&self, name: &str) -> Result<Properties, ComponentError>;

    fn get_component_config(
        &self,
        component_type: &ComponentType,
        name: &str,
    ) -> Option<ComponentConfig>;
}

pub struct YamlConfigOperator {
    config_path: Box<Path>,
    config_cache: Cache<String, Result<Config, ComponentError>>,
}

#[allow(dead_code, unused)]
impl YamlConfigOperator {
    pub fn new(config_path: &str) -> Self {
        let config_cache: Cache<String, Result<Config, ComponentError>> = Cache::builder()
            .time_to_live(Duration::from_secs(5))
            .build();
        YamlConfigOperator {
            config_path: Path::new(config_path).into(),
            config_cache,
        }
    }

    pub fn new_path(config_path: &Path) -> Self {
        let config_cache: Cache<String, Result<Config, ComponentError>> = Cache::builder()
            .time_to_live(Duration::from_secs(5))
            .build();
        YamlConfigOperator {
            config_path: Path::new(config_path).into(),
            config_cache,
        }
    }

    pub fn init(&self) -> Result<(), ComponentError> {
        let config_path = Path::new("config.yaml");
        let display_path = match env::current_dir() {
            Ok(cwd) => cwd.join(config_path),
            Err(_) => config_path.to_path_buf(),
        };
        info!("Config file located at: {}", display_path.display());
        if let Some(parent) = self.config_path.parent().filter(|p| !p.exists()) {
            fs::create_dir_all(parent)
                .map_err(|e| ComponentError::new(format!("Failed to create directory: {}", e)))?;
        }

        if !self.config_path.exists() {
            info!("Config file not exists, creating a default config file");
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&self.config_path)
                .map_err(|e| ComponentError::new(format!("Failed to open config file: {}", e)))?;
            file.write_all(b"instances: []\ncomponents: \nprocessors: []")
                .map_err(|e| ComponentError::new(format!("Failed to write config file: {}", e)))?;
        }
        Ok(())
    }

    fn load_yaml(&self) -> Result<Config, ComponentError> {
        let file = File::open(&self.config_path)
            .map_err(|e| ComponentError::new(format!("Failed to open config file: {}", e)))?;
        let reader = std::io::BufReader::new(file);
        let yaml: Config = serde_yaml::from_reader(reader)
            .map_err(|e| ComponentError::new(format!("Failed to parse config cause: {}", e)))?;
        Ok(yaml)
    }

    fn get_config(&self) -> Result<Config, ComponentError> {
        let path = self.config_path.to_str().unwrap().to_string();
        self.config_cache.get_with(path, move || {
            self.load_yaml()
                .map_err(|e| ComponentError::new(format!("Failed to get config cause: {}", e)))
        })
    }

    fn write_config(&self, config: &Config) -> Result<(), ComponentError> {
        let file = File::create(&self.config_path).expect("File should exist");
        serde_yaml::to_writer(file, config)
            .map_err(|e| ComponentError::new(format!("Failed to write config file: {}", e)))?;
        self.config_cache
            .invalidate(self.config_path.to_str().unwrap());
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Config {
    #[serde(default)]
    instances: Vec<InstanceConfig>,
    #[serde(default)]
    components: IndexMap<String, Vec<ComponentConfig>>,
    #[serde(default)]
    processors: Vec<ProcessorConfig>,
}

impl ConfigOperator for YamlConfigOperator {
    fn get_processor_config(&self, name: &str) -> Option<ProcessorConfig> {
        self.get_config()
            .map(|config| config.processors.iter().find(|p| p.name == name).cloned())
            .unwrap_or_else(|e| {
                tracing::warn!("{}", e);
                None
            })
    }

    fn get_all_processor_config(&self) -> Vec<ProcessorConfig> {
        self.get_config()
            .map(|config| config.processors.clone())
            .unwrap_or_else(|e| {
                tracing::warn!("{}", e);
                vec![]
            })
    }

    fn get_all_component_config(&self) -> IndexMap<String, Vec<ComponentConfig>> {
        self.get_config()
            .map(|config| config.components.clone())
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to get config: {}", e);
                IndexMap::new()
            })
    }

    fn save_component(
        &self,
        root_type: &ComponentRootType,
        component_config: ComponentConfig,
    ) -> Result<(), ComponentError> {
        let mut config = self.get_config()?;
        let component_type = root_type.name().to_string();

        let components = config.components.entry(component_type).or_default();
        match components
            .iter_mut()
            .find(|c| c.name == component_config.name)
        {
            Some(existing) => {
                *existing = component_config;
            }
            None => {
                components.push(component_config);
            }
        }
        self.write_config(&config)
            .map_err(|e| ComponentError::new(format!("Failed to save config: {}", e)))?;
        Ok(())
    }

    fn save_processor(&self, processor_config: ProcessorConfig) -> Result<(), ComponentError> {
        let mut config = self.get_config()?;
        match config
            .processors
            .iter()
            .position(|p| p.name == processor_config.name)
        {
            Some(index) => {
                // 只更新 enabled 状态
                config.processors[index].enabled = processor_config.enabled;
            }
            None => {
                config.processors.push(processor_config);
            }
        }
        Ok(())
    }

    fn delete_component(
        &self,
        root_type: &ComponentRootType,
        component_type: &str,
        name: &str,
    ) -> Result<(), ComponentError> {
        let mut config = self.get_config()?;
        let root_type_name = root_type.name();

        if let Some(components) = config.components.get_mut(root_type_name)
            && let Some(pos) = components
                .iter()
                .filter(|c| c.component_type == component_type)
                .position(|c| c.name == name)
        {
            components.remove(pos);
            self.write_config(&config)?;
        }
        Ok(())
    }

    fn delete_processor(&self, name: &str) -> Result<bool, ComponentError> {
        let mut config = self.get_config()?;
        if let Some(pos) = config.processors.iter().position(|p| p.name == name) {
            config.processors.remove(pos);
            return Ok(true);
        }
        Ok(false)
    }

    fn get_instance_props(&self, name: &str) -> Result<Properties, ComponentError> {
        let config = self.get_config()?;
        if let Some(instance) = config.instances.iter().find(|i| i.name == name) {
            Ok(Properties::from_map(instance.props.clone()))
        } else {
            Ok(Properties::new())
        }
    }

    fn get_component_config(
        &self,
        component_type: &ComponentType,
        name: &str,
    ) -> Option<ComponentConfig> {
        let config = match self.get_config() {
            Ok(cfg) => cfg,
            Err(error) => {
                error!(
                    "Failed to get config {}:{} {}",
                    component_type.to_string(),
                    name,
                    error,
                );
                return None;
            }
        };

        let root_name = component_type.root_type.name();
        let type_name = component_type.name.clone();
        config
            .components
            .get(root_name)
            .and_then(|list| {
                list.iter()
                    .find(|c| c.component_type == type_name && c.name == name)
            })
            .cloned()
    }
}

#[cfg(test)]
mod test {
    use crate::config::{
        ComponentConfig, Config, ConfigOperator, ProcessorOptionConfig, YamlConfigOperator,
    };
    use source_downloader_sdk::component::ComponentRootType;
    use source_downloader_sdk::serde_json::Map;
    use std::fs;
    use std::path::Path;
    use tempfile::NamedTempFile;

    struct TestFileGuard<'a> {
        path: &'a Path,
    }

    static CONFIG_PATH: &str = "./tests/resources/config.yaml";

    impl<'a> Drop for TestFileGuard<'a> {
        fn drop(&mut self) {
            if self.path.exists() {
                fs::remove_file(self.path).ok();
                println!("初始化文件已清理: {:?}", self.path);
            }
        }
    }

    struct TempFileOperator {
        pub operator: YamlConfigOperator,
        _temp_file: NamedTempFile,
    }

    impl TempFileOperator {
        pub fn new_from_config(config_path: &str) -> Self {
            let temp_file = NamedTempFile::new().expect("无法创建临时文件");
            let temp_path = temp_file.path();
            fs::copy(config_path, temp_path).expect("无法复制配置文件到临时文件");
            let operator = YamlConfigOperator::new_path(temp_path);
            TempFileOperator {
                operator,
                _temp_file: temp_file,
            }
        }
    }

    #[test]
    fn given_file_not_exits_should_config_init() {
        let path_str = "./tests/resources/init-config.yaml";
        let path = Path::new(path_str);
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
        let _guard = TestFileGuard { path };

        let result = YamlConfigOperator::new(path_str).init();
        assert!(result.is_ok());
        assert!(path.exists());
    }

    #[test]
    fn test_deserialize_from_yaml() {
        let operator = YamlConfigOperator::new(CONFIG_PATH);
        let init_result = operator.init();
        assert!(init_result.is_ok());

        let load_result = operator.load_yaml();
        assert!(load_result.is_ok());
        let config = load_result.unwrap();
        assert!(config.processors.len() > 0);
        assert!(config.components.len() > 0);
        assert!(config.instances.len() > 0);
    }

    #[test]
    fn test_save_component() {
        let temp_operator = TempFileOperator::new_from_config(CONFIG_PATH);
        let operator = temp_operator.operator;

        let init_result = operator.init();
        assert!(init_result.is_ok());

        let component_config = ComponentConfig {
            name: "test_save_component".to_string(),
            component_type: "test".to_string(),
            props: Map::new(),
        };
        operator
            .save_component(&ComponentRootType::Source, component_config)
            .unwrap();

        let config = operator.get_config().expect("无法加载配置");
        let sources = config.components.get("source").expect("未找到 source 组件");
        assert!(
            sources
                .iter()
                .any(|c| c.name == "test_save_component" && c.component_type == "test")
        );
        // also check file content
        let cfg_from_file: Config = operator.load_yaml().unwrap();
        let sources = cfg_from_file
            .components
            .get("source")
            .expect("未找到 source 组件");
        assert!(
            sources
                .iter()
                .any(|c| c.name == "test_save_component" && c.component_type == "test")
        );
    }

    #[test]
    fn test_delete_component() {
        let temp_operator = TempFileOperator::new_from_config(CONFIG_PATH);
        println!("temp_operator: {:?}", temp_operator._temp_file);
        let operator = temp_operator.operator;

        let init_result = operator.init();
        assert!(init_result.is_ok());

        let delete_result =
            operator.delete_component(&ComponentRootType::Source, "system-file", "test");
        assert!(delete_result.is_ok());

        let config = operator.get_config().expect("无法加载配置");
        let sources = config.components.get("source").expect("未找到 source 组件");
        assert!(!sources.iter().any(|c| c.name == "system-file"));
        // also check file content
        let cfg_from_file: Config = operator.load_yaml().unwrap();
        let sources = cfg_from_file
            .components
            .get("source")
            .expect("未找到 source 组件");
        assert!(!sources.iter().any(|c| c.name == "system-file"));
    }
    #[test]
    fn ser_de_config() {
        let c = serde_json::from_str::<ProcessorOptionConfig>(
            r#"{"save-path-pattern": "/mnt/p1/{name}", "fetch-limit": 51, "rename-task-interval": "5m"}"#,
        )
        .unwrap();
        assert_eq!(
            c.rename_times_threshold,
            ProcessorOptionConfig::default().rename_times_threshold
        );
        assert_eq!(c.rename_task_interval, "5m");
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("\"rename-task-interval\":\"5m\""));
        assert!(s.contains("\"fetch-limit\":51"));
    }
}
