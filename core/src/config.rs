use moka::sync::Cache;
use sdk::component::{ComponentError, ComponentRootType};
#[allow(dead_code, unused)]
use sdk::{Deserialize, Serialize, Value};
use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub name: String,
    pub props: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub component_type: String,
    pub props: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessorConfig {
    /// 处理器名称
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Properties {
    inner: HashMap<String, Value>,
}

#[allow(dead_code, unused)]
impl Properties {
    pub fn new() -> Self {
        Properties {
            inner: HashMap::new(),
        }
    }

    pub fn from_map(map: HashMap<String, Value>) -> Self {
        Properties { inner: map }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.inner.get(key)
    }
}

#[allow(dead_code, unused)]
pub trait ConfigOperator: Send + Sync {
    fn get_all_processor_config(&self) -> Vec<ProcessorConfig>;

    fn get_all_component_config(&self) -> HashMap<String, Vec<ComponentConfig>>;

    fn save_component(&self, root_type: ComponentRootType, component_config: ComponentConfig);

    fn save_processor(&self, name: String, processor_config: ProcessorConfig);

    fn delete_component(
        &self,
        top_type: ComponentRootType,
        component_type: String,
        name: String,
    ) -> bool;

    fn delete_processor(&self, name: String) -> bool;

    fn get_instance_props(&self, name: String) -> Properties;
}

pub struct YamlConfigOperator {
    config_path: PathBuf,
    config_cache: Cache<String, Config>,
}

#[allow(dead_code, unused)]
impl YamlConfigOperator {
    pub fn new<P: AsRef<Path>>(config_path: P) -> Self {
        let config_cache: Cache<String, Config> = Cache::builder()
            .time_to_live(Duration::from_secs(5))
            .build();
        YamlConfigOperator {
            config_path: config_path.as_ref().to_path_buf(),
            config_cache,
        }
    }

    pub fn init(&self) -> Result<(), ComponentError> {
        log::info!("Config path: {}", self.config_path.display());
        if let Some(parent) = self.config_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .map_err(|e| ComponentError::new(format!("Failed to create directory: {}", e)));
            }
        }

        if !self.config_path.exists() {
            log::info!("Config file not found, creating a new one");
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(self.config_path.as_path())
                .map_err(|e| ComponentError::new(format!("Failed to open config file: {}", e)))?;
            file.write_all(b"instances: []\ncomponents: []\nprocessors: []")
                .map_err(|e| ComponentError::new(format!("Failed to write config file: {}", e)))?;
        }
        Ok(())
    }

    fn load_yaml(&self) -> Result<Config, ComponentError> {
        let file = fs::File::open(&self.config_path)
            .map_err(|e| ComponentError::new(format!("Failed to open config file: {}", e)))?;
        let reader = std::io::BufReader::new(file);
        let yaml: Config = serde_yaml::from_reader(reader).unwrap();
        Ok(yaml)
    }

    fn get_config(&self) -> Result<Config, ComponentError> {
        let path = self.config_path.to_str().unwrap().to_string();
        let config = self
            .config_cache
            .get_with(path, move || self.load_yaml().unwrap());
        Ok(config)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Config {
    #[serde(default)]
    instances: Vec<InstanceConfig>,
    #[serde(default)]
    components: HashMap<String, Vec<ComponentConfig>>,
    #[serde(default)]
    processors: Vec<ProcessorConfig>,
}

impl ConfigOperator for YamlConfigOperator {
    fn get_all_processor_config(&self) -> Vec<ProcessorConfig> {
        self.get_config()
            .map(|config| config.processors.clone())
            .unwrap_or_default()
    }

    fn get_all_component_config(&self) -> HashMap<String, Vec<ComponentConfig>> {
        self.get_config()
            .map(|config| config.components.clone())
            .unwrap_or_default()
    }

    fn save_component(&self, root_type: ComponentRootType, component_config: ComponentConfig) {
        let mut config = self.get_config().unwrap();
        let component_type = root_type.name();

        // 使用 entry API 更优雅地处理存在/不存在的情况
        config.components
            .entry(String::from(component_type))
            .or_insert(vec![component_config]);
        
    }

    fn save_processor(&self, name: String, processor_config: ProcessorConfig) {
        let mut config = self.get_config().unwrap();
        match config.processors.iter().position(|p| p.name == name) {
            Some(index) => {
                // 只更新 enabled 状态
                config.processors[index].enabled = processor_config.enabled;
            },
            None => {
                config.processors.push(processor_config);
            }
        }

    }

    fn delete_component(
        &self,
        top_type: ComponentRootType,
        component_type: String,
        name: String,
    ) -> bool {
        let mut config = self.get_config().unwrap();
        if let Some(components) = config.components.get_mut(component_type.as_str()) {
            if let Some(pos) = components.iter().position(|c| c.name == name) {
                components.remove(pos);
                return true;
            }
        }
        false
    }

    fn delete_processor(&self, name: String) -> bool {
        let mut config = self.get_config().unwrap();
        if let Some(pos) = config.processors.iter().position(|p| p.name == name) {
            config.processors.remove(pos);
            return true;
        }
        false
    }

    fn get_instance_props(&self, name: String) -> Properties {
        let config = self.get_config().unwrap();
        if let Some(instance) = config.instances.iter().find(|i| i.name == name) {
            Properties::from_map(instance.props.clone())
        } else {
            Properties::new()
        }
    }
}

#[cfg(test)]
mod test {
    use crate::config::YamlConfigOperator;
    use std::fs;
    use std::path::Path;

    struct TestFileGuard<'a> {
        path: &'a Path,
    }

    impl<'a> Drop for TestFileGuard<'a> {
        fn drop(&mut self) {
            if self.path.exists() {
                fs::remove_file(self.path).ok();
                println!("初始化文件已清理: {:?}", self.path);
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
    fn load_file() {
        let path_str = "./tests/resources/config.yaml";
        let operator = YamlConfigOperator::new(path_str);
        let result = operator.init();
        assert!(result.is_ok());

        operator.load_yaml().unwrap();
    }
}