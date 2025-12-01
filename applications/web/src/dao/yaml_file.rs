use crate::dao::ComponentDao;
use crate::error::error_handle::AppError;
use core::CoreApplication;
use parking_lot::RwLock;
use std::sync::Arc;

pub struct YamlFileDao {
    core_application: Arc<RwLock<CoreApplication>>,
}

impl YamlFileDao {
    pub fn new(core_application: Arc<RwLock<CoreApplication>>) -> Self {
        YamlFileDao { core_application }
    }
}

impl ComponentDao for YamlFileDao {
    fn list_component_suppliers(&self) -> Result<Vec<String>, AppError> {
        // 尝试获取核心应用的读锁
        let app = self.core_application.read();

        // 访问组件管理器
        let component_manager = app.component_manager.read();

        // 获取所有供应商
        let suppliers = component_manager
            .get_all_suppliers()
            .map_err(|e| AppError::InternalError(format!("无法获取供应商: {}", e)))?;

        // 处理数据：提取所有供应商类型名称
        let supplier_types: Vec<String> = suppliers
            .iter()
            .flat_map(|supplier| {
                supplier
                    .supply_types()
                    .iter()
                    .map(|c_type| c_type.name.clone())
                    .collect::<Vec<String>>()
            })
            .collect();

        // 返回结果
        Ok(supplier_types)
    }
}
