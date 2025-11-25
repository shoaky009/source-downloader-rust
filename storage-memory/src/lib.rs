use sdk::{ProcessingContent, ProcessingStorage};
use std::collections::HashMap;
use std::sync::RwLock;

pub struct MemoryProcessingStorage {
    contents: RwLock<HashMap<String, ProcessingContent>>,
}

impl MemoryProcessingStorage {
    pub fn new() -> Self {
        Self {
            contents: RwLock::new(HashMap::new()),
        }
    }
}

impl ProcessingStorage for MemoryProcessingStorage {
    fn save_processing_content(&self, content: &ProcessingContent) {
        self.contents.write().unwrap().insert(content.id.clone(), content.clone());
    }

    fn find_rename_content(
        &self,
        processor_name: &str,
        rename_times_threshold: i32,
    ) -> Vec<ProcessingContent> {
        self.contents
            .read()
            .unwrap()
            .values()
            .filter(|c| {
                c.processor_name == processor_name && c.rename_times >= rename_times_threshold
            })
            .cloned()
            .collect::<Vec<ProcessingContent>>()
    }

    fn find_by_name_and_hash(
        &self,
        processor_name: &str,
        item_hash: &str,
    ) -> Option<ProcessingContent> {
        self.contents
            .read()
            .unwrap()
            .values()
            .find(|c| c.processor_name == processor_name && c.item_hash == item_hash)
            .cloned()
    }

    fn find_content_by_id(&self, id: &str) -> Option<ProcessingContent> {
        self.contents.read().unwrap().get(id).cloned()
    }
}

#[cfg(test)]
mod tests {

}
