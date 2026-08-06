use source_downloader_sdk::component::{PointedItem, Source, SourcePointer};
use source_downloader_sdk::serde_json::Value;

pub(crate) struct FetchRound {
    pub(crate) items: Vec<PointedItem>,
    pub(crate) pointer: Value,
}

pub(crate) async fn fetch_and_commit(
    source: &dyn Source,
    pointer: &mut dyn SourcePointer,
    limit: u32,
) -> FetchRound {
    let items = source.fetch(pointer, limit).await.unwrap();
    for item in &items {
        pointer.update(&item.source_item, item.item_pointer.as_ref());
    }
    FetchRound { pointer: pointer.dump(), items }
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::SourceItem;
    use source_downloader_sdk::async_trait::async_trait;
    use source_downloader_sdk::component::{
        EMPTY_POINTER, EmptyPointer, ProcessingError, SdComponent,
    };
    use std::fmt::{Display, Formatter};

    #[derive(Debug)]
    struct EmptySource;

    impl Display for EmptySource {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("empty-source")
        }
    }

    impl SdComponent for EmptySource {}

    #[async_trait]
    impl Source for EmptySource {
        async fn fetch<'pointer>(
            &self,
            _: &'pointer dyn SourcePointer,
            _: u32,
        ) -> Result<Vec<PointedItem>, ProcessingError> {
            Ok(vec![PointedItem {
                source_item: SourceItem {
                    title: "item".to_owned(),
                    link: "https://example.test/item".parse().unwrap(),
                    datetime: source_downloader_sdk::time::OffsetDateTime::UNIX_EPOCH,
                    content_type: "application/octet-stream".to_owned(),
                    download_uri: "https://example.test/item.bin".parse().unwrap(),
                    attrs: Default::default(),
                    tags: Vec::new(),
                    identity: None,
                },
                item_pointer: EMPTY_POINTER.clone(),
            }])
        }

        fn default_pointer(&self) -> Box<dyn SourcePointer> {
            Box::new(EmptyPointer {})
        }

        fn parse_raw_pointer(&self, _: Value) -> Box<dyn SourcePointer> {
            self.default_pointer()
        }
    }

    #[tokio::test]
    async fn fetch_round_commits_and_dumps_pointer() {
        let source = EmptySource;
        let mut pointer = source.default_pointer();

        let round = fetch_and_commit(&source, pointer.as_mut(), 1).await;

        assert_eq!(round.items.len(), 1);
        assert_eq!(round.pointer, Value::Object(Default::default()));
    }
}
