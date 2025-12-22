use crate::processing_record::Model;
use async_trait::async_trait;
use sdk::storage::{
    Error, ProcessingContent, ProcessingContentQuery, ProcessingStatus, ProcessingStorage,
    ProcessingTargetPath, ProcessorSourceState,
};
use sea_orm::SqlxSqliteConnector;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sea_orm::*;
use serde_json::json;
use std::str::FromStr;

pub struct SeaProcessingStorage {
    db: DatabaseConnection,
}

#[allow(dead_code)]
impl SeaProcessingStorage {
    pub async fn new(database_url: &str) -> Result<Self, Error> {
        let db = if database_url.starts_with("sqlite") {
            let opts = SqliteConnectOptions::from_str(database_url)
                .map_err(|x| Error {
                    message: x.to_string(),
                })?
                .create_if_missing(true);
            let sqlx_pool = SqlitePoolOptions::new()
                .connect_with(opts)
                .await
                .map_err(|x| Error {
                    message: x.to_string(),
                })?;

            sqlx::migrate!("migrations/sqlite")
                .run(&sqlx_pool)
                .await
                .map_err(|x| Error {
                    message: x.to_string(),
                })?;
            SqlxSqliteConnector::from_sqlx_sqlite_pool(sqlx_pool)
        } else {
            Database::connect(database_url).await.map_err(|x| Error {
                message: x.to_string(),
            })?
        };
        Ok(Self { db })
    }

    fn model_to_content(saved: Model) -> Result<ProcessingContent, Error> {
        Ok(ProcessingContent {
            id: Some(saved.id),
            processor_name: saved.processor_name,
            item_hash: saved.item_hash,
            item_identity: saved.item_identity,
            item_content: serde_json::from_value(saved.item_content).map_err(|e| Error {
                message: e.to_string(),
            })?,
            rename_times: saved.rename_times,
            status: ProcessingStatus::from(saved.status),
            failure_reason: saved.failure_reason,
            created_at: saved.created_at,
            updated_at: saved.updated_at,
        })
    }

    fn model_to_processor_source_state(
        saved: processor_source_state::Model,
    ) -> Result<ProcessorSourceState, Error> {
        Ok(ProcessorSourceState {
            id: Some(saved.id),
            processor_name: saved.processor_name,
            source_id: saved.source_id,
            last_pointer: saved.last_pointer_json,
        })
    }
}

#[allow(dead_code, unused)]
#[async_trait]
impl ProcessingStorage for SeaProcessingStorage {
    async fn save_processing_content(&self, content: &ProcessingContent) -> Result<i64, Error> {
        let model = processing_record::ActiveModel {
            id: if let Some(id) = content.id {
                Set(id)
            } else {
                NotSet
            },
            processor_name: Set(content.processor_name.to_owned()),
            item_hash: Set(content.item_hash.to_owned()),
            item_identity: Set(content.item_identity.to_owned()),
            item_content: Set(json!(content.item_content)),
            rename_times: Set(content.rename_times),
            status: Set(content.status as i32),
            failure_reason: Set(content.failure_reason.to_owned()),
            created_at: Set(content.created_at),
            updated_at: Set(content.updated_at),
        };
        let id = model
            .save(&self.db)
            .await
            .map(|x| x.id.unwrap())
            .map_err(|x| Error {
                message: x.to_string(),
            })?;
        Ok(id)
    }

    async fn processing_content_exists(&self, name: &str, hashing: &str) -> Result<bool, Error> {
        processing_record::Entity::find()
            .filter(
                processing_record::Column::ProcessorName
                    .eq(name)
                    .and(processing_record::Column::ItemHash.eq(hashing)),
            )
            .exists(&self.db)
            .await
            .map_err(|x| Error {
                message: x.to_string(),
            })
    }

    async fn delete_processing_content(&self, id: i64) -> Result<(), Error> {
        processing_record::Entity::delete_by_id(id)
            .exec(&self.db)
            .await
            .map_err(|e| Error {
                message: e.to_string(),
            })?;
        Ok(())
    }

    async fn find_by_name_and_hash(
        &self,
        processor_name: &str,
        item_hash: &str,
    ) -> Result<Option<ProcessingContent>, Error> {
        todo!()
    }

    async fn find_content_by_id(&self, id: i64) -> Result<Option<ProcessingContent>, Error> {
        let model = processing_record::Entity::find_by_id(id)
            .one(&self.db)
            .await
            .map_err(|e| Error {
                message: e.to_string(),
            })?;
        match model {
            None => Ok(None),
            Some(model) => Ok(Some(Self::model_to_content(model)?)),
        }
    }

    async fn query_processing_content(
        &self,
        query: &ProcessingContentQuery,
    ) -> Result<Vec<ProcessingContent>, Error> {
        todo!()
    }

    async fn save_file_contents(&self, content_id: i64, files: Vec<u8>) -> Result<(), Error> {
        let model = item_file_content::ActiveModel {
            id: Set(content_id),
            file_content: Set(files),
        };

        // 使用 Entity::insert 来构建查询
        item_file_content::Entity::insert(model)
            .on_conflict(
                // 定义冲突的目标列（通常是主键）
                OnConflict::column(item_file_content::Column::Id)
                    // 定义冲突时要更新的列
                    .update_column(item_file_content::Column::FileContent)
                    .to_owned(),
            )
            .exec(&self.db)
            .await
            .map(|_| ())
            .map_err(|e| Error {
                message: e.to_string(),
            })
    }

    async fn find_processor_source_state(
        &self,
        processor_name: &str,
        source_id: &str,
    ) -> Result<Option<ProcessorSourceState>, Error> {
        let entity = processor_source_state::Entity::find()
            .filter(
                processor_source_state::Column::ProcessorName
                    .eq(processor_name)
                    .and(processor_source_state::Column::SourceId.eq(source_id)),
            )
            .one(&self.db)
            .await
            .map_err(|e| Error {
                message: format!("Failed to find source state {}", e),
            })?;
        if entity.is_none() {
            return Ok(None);
        }
        Ok(Some(Self::model_to_processor_source_state(
            entity.unwrap(),
        )?))
    }

    async fn save_processor_source_state(
        &self,
        state: &ProcessorSourceState,
    ) -> Result<ProcessorSourceState, Error> {
        let model = processor_source_state::ActiveModel {
            id: if let Some(id) = state.id {
                Set(id)
            } else {
                NotSet
            },
            processor_name: Set(state.processor_name.to_owned()),
            source_id: Set(state.source_id.to_owned()),
            last_pointer_json: Set(state.last_pointer.clone()),
            retry_times: Set(0),
            last_active_at: Set(Some(time::OffsetDateTime::now_utc().to_string())),
        };

        let saved = model
            .save(&self.db)
            .await
            .map(|x| x.try_into_model())
            .flatten()
            .map_err(|x| Error {
                message: x.to_string(),
            })?;
        Ok(Self::model_to_processor_source_state(saved)?)
    }

    async fn save_paths(&self, paths: Vec<ProcessingTargetPath>) -> Result<(), Error> {
        todo!()
    }
}

#[cfg(test)]
mod test {
    use crate::SeaProcessingStorage;
    use sdk::SourceItem;
    use sdk::storage::{ItemContentLite, ProcessingContent, ProcessingStatus, ProcessingStorage};
    use std::collections::HashMap;
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn create_test_processing_content(
        processor_name: &str,
        status: ProcessingStatus,
    ) -> ProcessingContent {
        ProcessingContent {
            id: None,
            processor_name: processor_name.to_string(),
            item_hash: Uuid::new_v4().to_string(),
            item_identity: Some(format!("identity_{}", Uuid::new_v4())),
            item_content: ItemContentLite {
                source_item: SourceItem {
                    title: "Test Title".to_string(),
                    link: "https://example.com".parse().unwrap(),
                    datetime: OffsetDateTime::now_utc(),
                    content_type: "text/html".to_string(),
                    download_uri: "https://example.com/download".parse().unwrap(),
                    attrs: Default::default(),
                    tags: Default::default(),
                    identity: None,
                },
                item_variables: HashMap::new(),
            },
            rename_times: 0,
            status,
            failure_reason: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn test_save_processing_content_without_id() {
        let db_url = "sqlite::memory:";
        let s = SeaProcessingStorage::new(db_url).await.unwrap();

        let content = create_test_processing_content("test_processor", ProcessingStatus::Renamed);
        let id = s.save_processing_content(&content).await.unwrap();

        let res = s.find_content_by_id(id).await.unwrap().unwrap();
        assert!(res.id.is_some());
        assert_eq!(res.processor_name, "test_processor");
        assert_eq!(res.item_hash, content.item_hash);
        assert_eq!(res.status, ProcessingStatus::Renamed);
        assert_eq!(res.rename_times, 0);
    }

    #[tokio::test]
    async fn test_save_processing_content_with_id() {
        let db_url = "sqlite::memory:";
        let s = SeaProcessingStorage::new(db_url).await.unwrap();

        let mut content =
            create_test_processing_content("test_processor_2", ProcessingStatus::WaitingToRename);

        // 第一次保存获取 ID
        let id = s.save_processing_content(&content).await.unwrap();

        // 使用获取的 ID 进行第二次保存
        content.id = Some(id);
        content.rename_times = 5;
        let updated_id = s.save_processing_content(&content).await.unwrap();

        // 验证更新
        assert_eq!(updated_id, id);
        let updated = s.find_content_by_id(id).await.unwrap().unwrap();
        assert_eq!(updated.rename_times, 5);
    }

    #[tokio::test]
    async fn test_save_processing_content_with_failure_reason() {
        let db_url = "sqlite::memory:";
        let s = SeaProcessingStorage::new(db_url).await.unwrap();

        let mut content =
            create_test_processing_content("test_processor_3", ProcessingStatus::Failure);
        content.failure_reason = Some("Download failed".to_string());

        let id = s.save_processing_content(&content).await.unwrap();

        let res = s.find_content_by_id(id).await.unwrap().unwrap();
        assert!(res.id.is_some());
        assert_eq!(res.failure_reason, Some("Download failed".to_string()));
        assert_eq!(res.status, ProcessingStatus::Failure);
    }
}

mod processing_record {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "processing_record")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = true)]
        pub id: i64,
        pub processor_name: String,
        pub item_hash: String,
        pub item_identity: Option<String>,
        pub item_content: Json,
        pub rename_times: u32,
        pub status: i32,
        pub failure_reason: Option<String>,
        pub created_at: OffsetDateTime,
        pub updated_at: Option<OffsetDateTime>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod processor_source_state {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "processor_source_state")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = true)]
        pub id: i64,
        pub processor_name: String,
        pub source_id: String,
        pub last_pointer_json: Json,
        pub retry_times: i32,
        pub last_active_at: Option<String>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

mod item_file_content {
    use sea_orm::entity::prelude::*;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "item_file_content")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub file_content: Vec<u8>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}
