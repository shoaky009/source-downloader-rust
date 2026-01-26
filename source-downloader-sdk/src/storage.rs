use crate::SourceItem;
use std::collections::HashMap;

use crate::serde_json::Value;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[async_trait]
pub trait ProcessingStorage: Send + Sync {
    /// If [content.id] is None, required to return a new [ProcessingContent] with id.
    async fn save_processing_content(&self, content: &ProcessingContent) -> Result<i64, Error>;
    async fn processing_content_exists(&self, name: &str, hashing: &str) -> Result<bool, Error>;
    async fn delete_processing_content(&self, id: i64) -> Result<(), Error>;
    async fn find_by_name_and_hash(
        &self,
        processor_name: &str,
        item_hash: &str,
    ) -> Result<Option<ProcessingContent>, Error>;
    async fn find_content_by_id(&self, id: i64) -> Result<Option<ProcessingContent>, Error>;
    async fn query_processing_content(
        &self,
        query: &ProcessingContentQuery,
    ) -> Result<Vec<ProcessingContent>, Error>;

    async fn save_file_contents(&self, content_id: i64, files: Vec<u8>) -> Result<(), Error>;

    async fn find_file_contents(&self, content_id: i64) -> Result<Option<Vec<u8>>, Error>;

    async fn find_processor_source_state(
        &self,
        processor_name: &str,
        source_id: &str,
    ) -> Result<Option<ProcessorSourceState>, Error>;
    /// If [state.id] is None, required to return a new [ProcessingContent] with id.
    async fn save_processor_source_state(
        &self,
        state: &ProcessorSourceState,
    ) -> Result<ProcessorSourceState, Error>;

    async fn save_paths(&self, paths: Vec<ProcessingTargetPath>) -> Result<(), Error>;
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessingContent {
    pub id: Option<i64>,
    pub processor_name: String,
    pub item_hash: String,
    pub item_identity: Option<String>,
    pub item_content: ItemContentLite,
    pub rename_times: u32,
    pub status: ProcessingStatus,
    pub failure_reason: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemContentLite {
    pub source_item: SourceItem,
    pub item_variables: HashMap<String, String>,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

    /// 初始
    Init = 10,
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

#[derive(Default)]
pub struct ProcessingContentQuery {
    pub processor_name: Option<Vec<String>>,
    pub rename_times_threshold: Option<u32>,
    pub item_hash: Option<Vec<String>>,
    pub item_identity: Option<Vec<String>>,
    pub status: Option<Vec<ProcessingStatus>>,
    pub created_at_start: Option<OffsetDateTime>,
    pub created_at_end: Option<OffsetDateTime>,
    pub max_id: Option<i64>,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessorSourceState {
    pub id: Option<i64>,
    pub processor_name: String,
    pub source_id: String,
    pub last_pointer: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessingTargetPath {
    pub path: String,
    pub processor_name: String,
    pub item_hash: String,
}

#[derive(Debug, Clone)]
pub struct Error {
    pub message: String,
}
