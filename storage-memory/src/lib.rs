use async_trait::async_trait;
use source_downloader_sdk::storage::{
    Error, ProcessingContent, ProcessingContentQuery, ProcessingStorage, ProcessingTargetPath,
    ProcessorSourceState,
};
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Default)]
#[allow(dead_code)]
pub struct MemoryProcessingStorage {
    contents: RwLock<HashMap<i64, ProcessingContent>>,
}

impl MemoryProcessingStorage {
    pub fn new() -> Self {
        Self {
            contents: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ProcessingStorage for MemoryProcessingStorage {
    async fn save_processing_content(&self, _: &ProcessingContent) -> Result<i64, Error> {
        todo!()
    }

    async fn processing_content_exists(&self, _: &str, _: &str) -> Result<bool, Error> {
        todo!()
    }

    async fn delete_processing_content(&self, _: i64) -> Result<(), Error> {
        todo!()
    }

    async fn find_by_name_and_hash(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<ProcessingContent>, Error> {
        todo!()
    }

    async fn find_content_by_id(&self, _: i64) -> Result<Option<ProcessingContent>, Error> {
        todo!()
    }

    async fn query_processing_content(
        &self,
        _: &ProcessingContentQuery,
    ) -> Result<Vec<ProcessingContent>, Error> {
        todo!()
    }

    async fn save_file_contents(&self, _: i64, _: Vec<u8>) -> Result<(), Error> {
        todo!()
    }

    async fn find_file_contents(&self, _: i64) -> Result<Option<Vec<u8>>, Error> {
        todo!()
    }

    async fn find_processor_source_state(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<ProcessorSourceState>, Error> {
        todo!()
    }

    async fn save_processor_source_state(
        &self,
        _: &ProcessorSourceState,
    ) -> Result<ProcessorSourceState, Error> {
        todo!()
    }

    async fn save_paths(&self, _: Vec<ProcessingTargetPath>) -> Result<(), Error> {
        todo!()
    }
}

#[cfg(test)]
mod tests {}
