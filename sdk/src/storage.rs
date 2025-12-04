use crate::SourceItem;
use serde_json::{Map, Value};
use time::PrimitiveDateTime;

pub trait ProcessingStorage: Send + Sync {
    fn save_processing_content(&self, content: &ProcessingContent);
    fn find_rename_content(
        &self,
        processor_name: &str,
        rename_times_threshold: i32,
    ) -> Vec<ProcessingContent>;
    fn find_by_name_and_hash(
        &self,
        processor_name: &str,
        item_hash: &str,
    ) -> Option<ProcessingContent>;
    fn find_content_by_id(&self, id: &str) -> Option<ProcessingContent>;
}

#[derive(Debug, Clone)]
pub struct ProcessingContent {
    pub id: String,
    pub processor_name: String,
    pub item_hash: String,
    pub item_identity: Option<String>,
    pub item_content: ItemContentLite,
    pub rename_times: i32,
    pub status: ProcessingStatus,
    pub failure_reason: Option<String>,
    pub created_at: PrimitiveDateTime,
    pub updated_at: Option<PrimitiveDateTime>,
}

#[derive(Debug, Clone)]
pub struct ItemContentLite {
    pub source_item: SourceItem,
    pub item_variables: Map<String, Value>,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStatus {
    /// 下载完成后重命名，可能包含替换的文件
    WaitingToRename = 0,

    /// 被 [ItemContentFilter] 过滤
    Filtered = 2,

    /// 下载失败，指从下载器获取 [SourceItem] 对应的信息失败，大概率是人工手动删除了
    DownloadFailed = 3,

    /// 全部目标文件存在
    TargetAlreadyExists = 4,

    /// 已重命名
    Renamed = 5,

    /// [SourceItem] 无文件
    NoFiles = 7,

    /// 处理失败
    Failure = 8,

    /// 取消
    Cancelled = 9,
}

impl From<i32> for ProcessingStatus {
    fn from(value: i32) -> Self {
        match value {
            0 => ProcessingStatus::WaitingToRename,
            2 => ProcessingStatus::Filtered,
            3 => ProcessingStatus::DownloadFailed,
            4 => ProcessingStatus::TargetAlreadyExists,
            5 => ProcessingStatus::Renamed,
            7 => ProcessingStatus::NoFiles,
            8 => ProcessingStatus::Failure,
            9 => ProcessingStatus::Cancelled,
            _ => ProcessingStatus::Failure,
        }
    }
}
