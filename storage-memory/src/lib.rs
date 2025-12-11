use async_trait::async_trait;
use sdk::{
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
    async fn save_processing_content(
        &self,
        _: &ProcessingContent,
    ) -> Result<ProcessingContent, Error> {
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
