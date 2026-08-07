use crate::processing_record::Model;
use async_trait::async_trait;
use sea_orm::SqlxSqliteConnector;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sea_orm::*;
use serde_json::json;
use source_downloader_sdk::storage::{
    Error, ProcessingContent, ProcessingContentQuery, ProcessingStatus,
    ProcessingStorage, ProcessingTargetPath, ProcessorSourceState,
};
use std::str::FromStr;
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;

pub struct SeaProcessingStorage {
    db: DatabaseConnection,
}

#[allow(dead_code)]
impl SeaProcessingStorage {
    pub async fn new(database_url: &str) -> Result<Self, Error> {
        Self::connect(database_url, false).await
    }

    pub async fn new_with_wal(database_url: &str) -> Result<Self, Error> {
        Self::connect(database_url, true).await
    }

    async fn connect(database_url: &str, enable_wal: bool) -> Result<Self, Error> {
        let db = if database_url.starts_with("sqlite") {
            let mut opts = SqliteConnectOptions::from_str(database_url)
                .map_err(|x| Error { message: x.to_string() })?
                .create_if_missing(true);
            if enable_wal {
                opts = opts.journal_mode(SqliteJournalMode::Wal);
            }
            let sqlx_pool = SqlitePoolOptions::new()
                .connect_with(opts)
                .await
                .map_err(|x| Error { message: x.to_string() })?;

            sqlx::migrate!("migrations/sqlite")
                .run(&sqlx_pool)
                .await
                .map_err(|x| Error { message: x.to_string() })?;
            SqlxSqliteConnector::from_sqlx_sqlite_pool(sqlx_pool)
        } else {
            Database::connect(database_url)
                .await
                .map_err(|x| Error { message: x.to_string() })?
        };
        Ok(Self { db })
    }

    fn model_to_content(saved: Model) -> Result<ProcessingContent, Error> {
        Ok(ProcessingContent {
            id: Some(saved.id),
            processor_name: saved.processor_name,
            item_hash: saved.item_hash,
            item_identity: saved.item_identity,
            item_content: serde_json::from_value(saved.item_content)
                .map_err(|e| Error { message: e.to_string() })?,
            rename_times: saved.rename_times,
            status: ProcessingStatus::from(saved.status),
            failure_reason: saved.failure_reason,
            created_at: saved.created_at,
            updated_at: saved.updated_at,
        })
    }

    fn parse_saved_time(value: String) -> Result<OffsetDateTime, Error> {
        OffsetDateTime::parse(&value, &Iso8601::DEFAULT)
            .or_else(|_| {
                let normalized =
                    value.replacen(' ', "T", 1).replace(" +", "+").replace(" -", "-");
                OffsetDateTime::parse(&normalized, &Iso8601::DEFAULT)
            })
            .map_err(|error| Error {
                message: format!("Invalid processor source state time: {error}"),
            })
    }

    fn json_key_path(prefix: &str, key: &str) -> String {
        format!("{prefix}.\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    }

    fn model_to_processor_source_state(
        saved: processor_source_state::Model,
    ) -> Result<ProcessorSourceState, Error> {
        let last_active_time =
            saved.last_active_at.map(Self::parse_saved_time).transpose()?;
        let retry_times = u32::try_from(saved.retry_times).map_err(|_| Error {
            message: format!("Invalid negative retry_times: {}", saved.retry_times),
        })?;
        Ok(ProcessorSourceState {
            id: Some(saved.id),
            processor_name: saved.processor_name,
            source_id: saved.source_id,
            last_pointer: saved.last_pointer_json,
            last_active_time,
            retry_times,
        })
    }
}

#[allow(dead_code, unused)]
#[async_trait]
impl ProcessingStorage for SeaProcessingStorage {
    async fn save_processing_content(
        &self,
        content: &ProcessingContent,
    ) -> Result<i64, Error> {
        let model = processing_record::ActiveModel {
            id: if let Some(id) = content.id { Set(id) } else { NotSet },
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
            .map_err(|x| Error { message: x.to_string() })?;
        Ok(id)
    }

    async fn processing_content_exists(
        &self,
        name: &str,
        hashing: &str,
    ) -> Result<bool, Error> {
        processing_record::Entity::find()
            .filter(
                processing_record::Column::ProcessorName
                    .eq(name)
                    .and(processing_record::Column::ItemHash.eq(hashing)),
            )
            .exists(&self.db)
            .await
            .map_err(|x| Error { message: x.to_string() })
    }

    async fn delete_processing_content(&self, id: i64) -> Result<(), Error> {
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        item_file_content::Entity::delete_by_id(id)
            .exec(&transaction)
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        processing_record::Entity::delete_by_id(id)
            .exec(&transaction)
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        transaction.commit().await.map_err(|error| Error { message: error.to_string() })
    }

    async fn delete_processing_contents_by_processor(
        &self,
        processor_name: &str,
    ) -> Result<u64, Error> {
        let transaction = self
            .db
            .begin()
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        let content_ids = processing_record::Entity::find()
            .select_only()
            .column(processing_record::Column::Id)
            .filter(processing_record::Column::ProcessorName.eq(processor_name))
            .into_tuple::<i64>()
            .all(&transaction)
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        if !content_ids.is_empty() {
            item_file_content::Entity::delete_many()
                .filter(item_file_content::Column::Id.is_in(content_ids))
                .exec(&transaction)
                .await
                .map_err(|error| Error { message: error.to_string() })?;
        }
        let deleted = processing_record::Entity::delete_many()
            .filter(processing_record::Column::ProcessorName.eq(processor_name))
            .exec(&transaction)
            .await
            .map_err(|error| Error { message: error.to_string() })?
            .rows_affected;
        transaction
            .commit()
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        Ok(deleted)
    }

    async fn find_by_name_and_hash(
        &self,
        processor_name: &str,
        item_hash: &str,
    ) -> Result<Option<ProcessingContent>, Error> {
        processing_record::Entity::find()
            .filter(
                processing_record::Column::ProcessorName
                    .eq(processor_name)
                    .and(processing_record::Column::ItemHash.eq(item_hash)),
            )
            .one(&self.db)
            .await
            .map_err(|error| Error { message: error.to_string() })?
            .map(Self::model_to_content)
            .transpose()
    }

    async fn find_content_by_id(
        &self,
        id: i64,
    ) -> Result<Option<ProcessingContent>, Error> {
        let model = processing_record::Entity::find_by_id(id)
            .one(&self.db)
            .await
            .map_err(|e| Error { message: e.to_string() })?;
        match model {
            None => Ok(None),
            Some(model) => Ok(Some(Self::model_to_content(model)?)),
        }
    }

    async fn query_processing_content(
        &self,
        query: &ProcessingContentQuery,
    ) -> Result<Vec<ProcessingContent>, Error> {
        let mut db_query = processing_record::Entity::find();
        if let Some(ids) = &query.id {
            db_query =
                db_query.filter(processing_record::Column::Id.is_in(ids.iter().copied()));
        }

        // 动态条件：processor_name
        if let Some(processor_names) = &query.processor_name {
            db_query = db_query.filter(
                processing_record::Column::ProcessorName
                    .is_in(processor_names.iter().cloned()),
            );
        }

        // 动态条件：item_hash
        if let Some(item_hashes) = &query.item_hash {
            db_query = db_query.filter(
                processing_record::Column::ItemHash.is_in(item_hashes.iter().cloned()),
            );
        }

        // 动态条件：item_identity
        if let Some(item_identities) = &query.item_identity {
            db_query = db_query.filter(
                processing_record::Column::ItemIdentity
                    .is_in(item_identities.iter().cloned()),
            );
        }

        // 动态条件：status
        if let Some(statuses) = &query.status {
            let status_codes: Vec<i32> = statuses.iter().map(|s| *s as i32).collect();
            db_query =
                db_query.filter(processing_record::Column::Status.is_in(status_codes));
        }
        if let Some(item) = &query.item {
            if let Some(title) = &item.title {
                db_query = db_query.filter(Expr::cust_with_values(
                    "json_extract(item_content, ?) GLOB ?",
                    ["$.source_item.title".to_owned(), format!("*{title}*")],
                ));
            }
            if let Some(attrs) = &item.attrs {
                for (key, value) in attrs {
                    db_query = db_query.filter(Expr::cust_with_values(
                        "json_extract(item_content, ?) = ?",
                        [
                            Self::json_key_path("$.source_item.attrs", key),
                            value.to_owned(),
                        ],
                    ));
                }
            }
            if let Some(variables) = &item.variables {
                for (key, value) in variables {
                    db_query = db_query.filter(Expr::cust_with_values(
                        "json_extract(item_content, ?) = ?",
                        [Self::json_key_path("$.item_variables", key), value.to_owned()],
                    ));
                }
            }
            if let Some(content_type) = &item.content_type {
                db_query = db_query.filter(Expr::cust_with_values(
                    "json_extract(item_content, ?) = ?",
                    ["$.source_item.contentType".to_owned(), content_type.to_owned()],
                ));
            }
            if let Some(tags) = &item.tags {
                for tag in tags {
                    db_query = db_query.filter(Expr::cust_with_values(
                        "EXISTS (SELECT 1 FROM json_each(item_content, \
                         '$.source_item.tags') WHERE value = ?)",
                        [tag.to_owned()],
                    ));
                }
            }
        }

        // 动态条件：rename_times_threshold
        if let Some(threshold) = query.rename_times_threshold {
            db_query =
                db_query.filter(processing_record::Column::RenameTimes.lt(threshold));
        }

        // 动态条件：created_at_start
        if let Some(start_time) = query.created_at_start {
            db_query =
                db_query.filter(processing_record::Column::CreatedAt.gte(start_time));
        }

        // 动态条件：created_at_end
        if let Some(end_time) = query.created_at_end {
            db_query =
                db_query.filter(processing_record::Column::CreatedAt.lte(end_time));
        }

        // 动态条件：max_id (用于分页)
        if let Some(max_id) = query.max_id {
            db_query = db_query.filter(processing_record::Column::Id.lt(max_id));
        }

        // 排序和分页
        db_query = db_query.order_by_desc(processing_record::Column::Id);

        if let Some(limit) = query.limit {
            db_query = db_query.limit(limit);
        }

        let models =
            db_query.all(&self.db).await.map_err(|e| Error { message: e.to_string() })?;

        models.into_iter().map(Self::model_to_content).collect()
    }

    async fn save_file_contents(
        &self,
        content_id: i64,
        files: Vec<u8>,
    ) -> Result<(), Error> {
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
            .map_err(|e| Error { message: e.to_string() })
    }

    async fn find_file_contents(
        &self,
        content_id: i64,
    ) -> Result<Option<Vec<u8>>, Error> {
        let model = item_file_content::Entity::find_by_id(content_id)
            .one(&self.db)
            .await
            .map_err(|e| Error { message: e.to_string() })?;
        if let Some(model) = model { Ok(Some(model.file_content)) } else { Ok(None) }
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
        Ok(Some(Self::model_to_processor_source_state(entity.unwrap())?))
    }

    async fn save_processor_source_state(
        &self,
        state: &ProcessorSourceState,
    ) -> Result<ProcessorSourceState, Error> {
        let retry_times = i32::try_from(state.retry_times).map_err(|_| Error {
            message: format!("retry_times is too large: {}", state.retry_times),
        })?;
        let last_active_at = state
            .last_active_time
            .map(|value| value.format(&Iso8601::DEFAULT))
            .transpose()
            .map_err(|error| Error {
                message: format!("Failed to format processor source state time: {error}"),
            })?;
        let model = processor_source_state::ActiveModel {
            id: if let Some(id) = state.id { Set(id) } else { NotSet },
            processor_name: Set(state.processor_name.to_owned()),
            source_id: Set(state.source_id.to_owned()),
            last_pointer_json: Set(state.last_pointer.clone()),
            retry_times: Set(retry_times),
            last_active_at: Set(last_active_at),
        };

        let saved = model
            .save(&self.db)
            .await
            .and_then(|x| x.try_into_model())
            .map_err(|x| Error { message: x.to_string() })?;
        Ok(Self::model_to_processor_source_state(saved)?)
    }

    async fn save_paths(&self, paths: Vec<ProcessingTargetPath>) -> Result<(), Error> {
        if paths.is_empty() {
            return Ok(());
        }
        let now = time::OffsetDateTime::now_utc();
        let models = paths.into_iter().map(|path| target_path::ActiveModel {
            id: Set(path.path),
            processor_name: Set(path.processor_name),
            item_hash: Set(path.item_hash),
            created_at: Set(now),
        });
        target_path::Entity::insert_many(models)
            .on_conflict(
                OnConflict::column(target_path::Column::Id)
                    .update_columns([
                        target_path::Column::ProcessorName,
                        target_path::Column::ItemHash,
                        target_path::Column::CreatedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        Ok(())
    }

    async fn find_paths(
        &self,
        paths: &[String],
    ) -> Result<Vec<ProcessingTargetPath>, Error> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        target_path::Entity::find()
            .filter(target_path::Column::Id.is_in(paths.iter().cloned()))
            .all(&self.db)
            .await
            .map_err(|error| Error { message: error.to_string() })
            .map(|models| {
                models
                    .into_iter()
                    .map(|model| ProcessingTargetPath {
                        path: model.id,
                        processor_name: model.processor_name,
                        item_hash: model.item_hash,
                    })
                    .collect()
            })
    }

    async fn delete_paths(
        &self,
        paths: &[String],
        item_hash: Option<&str>,
    ) -> Result<(), Error> {
        if paths.is_empty() {
            return Ok(());
        }
        let path_condition = paths.iter().fold(Condition::any(), |condition, path| {
            if path.ends_with('*') {
                condition.add(Expr::cust_with_values("id GLOB ?", [path.to_owned()]))
            } else {
                condition.add(target_path::Column::Id.eq(path))
            }
        });
        let mut condition = Condition::all().add(path_condition);
        if let Some(item_hash) = item_hash {
            condition = condition.add(target_path::Column::ItemHash.eq(item_hash));
        }
        target_path::Entity::delete_many()
            .filter(condition)
            .exec(&self.db)
            .await
            .map_err(|error| Error { message: error.to_string() })?;
        Ok(())
    }

    async fn delete_paths_by_processor(
        &self,
        processor_name: &str,
    ) -> Result<u64, Error> {
        target_path::Entity::delete_many()
            .filter(target_path::Column::ProcessorName.eq(processor_name))
            .exec(&self.db)
            .await
            .map(|result| result.rows_affected)
            .map_err(|error| Error { message: error.to_string() })
    }
}

#[cfg(test)]
mod test {
    use crate::SeaProcessingStorage;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    use source_downloader_sdk::SourceItem;
    use source_downloader_sdk::storage::{
        ItemContentCondition, ItemContentLite, ProcessingContent, ProcessingContentQuery,
        ProcessingStatus, ProcessingStorage, ProcessingTargetPath, ProcessorSourceState,
    };
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
    async fn test_new_with_wal_enables_write_ahead_log() {
        let database_path = std::env::temp_dir().join(format!("{}.db", Uuid::new_v4()));
        let database_url = format!("sqlite:{}", database_path.display());

        let storage = SeaProcessingStorage::new_with_wal(&database_url).await.unwrap();
        let journal_mode: String = storage
            .db
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                "PRAGMA journal_mode".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "journal_mode")
            .unwrap();

        assert_eq!(journal_mode, "wal");
        drop(storage);
        std::fs::remove_file(database_path).unwrap();
    }

    #[tokio::test]
    async fn test_save_processing_content_without_id() {
        let db_url = "sqlite::memory:";
        let s = SeaProcessingStorage::new(db_url).await.unwrap();

        let content =
            create_test_processing_content("test_processor", ProcessingStatus::Renamed);
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

        let mut content = create_test_processing_content(
            "test_processor_2",
            ProcessingStatus::WaitingToRename,
        );

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
    #[tokio::test]
    async fn test_query_by_id_and_delete_file_contents() {
        let storage = SeaProcessingStorage::new("sqlite::memory:").await.unwrap();
        let mut selected_content =
            create_test_processing_content("processor", ProcessingStatus::Renamed);
        selected_content.item_content.source_item.title = "Selected title".to_owned();
        selected_content
            .item_content
            .source_item
            .attrs
            .insert("language".to_owned(), serde_json::json!("zh"));
        selected_content.item_content.source_item.tags.push("anime".to_owned());
        selected_content
            .item_content
            .item_variables
            .insert("season".to_owned(), "2".to_owned());
        let selected_id =
            storage.save_processing_content(&selected_content).await.unwrap();
        let other_id = storage
            .save_processing_content(&create_test_processing_content(
                "processor",
                ProcessingStatus::Failure,
            ))
            .await
            .unwrap();
        storage.save_file_contents(selected_id, vec![1, 2, 3]).await.unwrap();

        let selected = storage
            .query_processing_content(&ProcessingContentQuery {
                id: Some(vec![selected_id]),
                item: Some(ItemContentCondition {
                    title: Some("Selected".to_owned()),
                    attrs: Some(HashMap::from([(
                        "language".to_owned(),
                        "zh".to_owned(),
                    )])),
                    variables: Some(HashMap::from([(
                        "season".to_owned(),
                        "2".to_owned(),
                    )])),
                    content_type: Some("text/html".to_owned()),
                    tags: Some(vec!["anime".to_owned()]),
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, Some(selected_id));
        assert_ne!(selected[0].id, Some(other_id));

        storage.delete_processing_content(selected_id).await.unwrap();
        assert!(storage.find_content_by_id(selected_id).await.unwrap().is_none());
        assert!(storage.find_file_contents(selected_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_processor_source_state_round_trip() {
        let storage = SeaProcessingStorage::new("sqlite::memory:").await.unwrap();
        let last_active_time = OffsetDateTime::now_utc();
        let state = ProcessorSourceState {
            id: None,
            processor_name: "processor".to_owned(),
            source_id: "source".to_owned(),
            last_pointer: serde_json::json!({"page": 3}),
            last_active_time: Some(last_active_time),
            retry_times: 2,
        };

        let saved = storage.save_processor_source_state(&state).await.unwrap();
        let loaded = storage
            .find_processor_source_state("processor", "source")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(saved.last_active_time, Some(last_active_time));
        assert_eq!(loaded.last_active_time, Some(last_active_time));
        assert_eq!(loaded.retry_times, 2);
    }

    #[tokio::test]
    async fn test_target_path_lifecycle() {
        let storage = SeaProcessingStorage::new("sqlite::memory:").await.unwrap();
        let path = "/target/file.txt".to_owned();
        storage
            .save_paths(vec![ProcessingTargetPath {
                path: path.clone(),
                processor_name: "processor".to_owned(),
                item_hash: "first".to_owned(),
            }])
            .await
            .unwrap();

        let saved = storage.find_paths(std::slice::from_ref(&path)).await.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].item_hash, "first");

        storage
            .save_paths(vec![ProcessingTargetPath {
                path: path.clone(),
                processor_name: "processor".to_owned(),
                item_hash: "second".to_owned(),
            }])
            .await
            .unwrap();
        storage.delete_paths(std::slice::from_ref(&path), Some("first")).await.unwrap();
        assert_eq!(
            storage.find_paths(std::slice::from_ref(&path)).await.unwrap()[0].item_hash,
            "second"
        );

        storage.delete_paths(std::slice::from_ref(&path), Some("second")).await.unwrap();
        assert!(storage.find_paths(&[path]).await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn test_delete_target_path_prefix() {
        let storage = SeaProcessingStorage::new("sqlite::memory:").await.unwrap();
        let matching = "/target/sub/file.txt".to_owned();
        let retained = "/other/file.txt".to_owned();
        storage
            .save_paths(vec![
                ProcessingTargetPath {
                    path: matching.clone(),
                    processor_name: "processor".to_owned(),
                    item_hash: "hash".to_owned(),
                },
                ProcessingTargetPath {
                    path: retained.clone(),
                    processor_name: "processor".to_owned(),
                    item_hash: "hash".to_owned(),
                },
            ])
            .await
            .unwrap();

        storage.delete_paths(&["/target/*".to_owned()], None).await.unwrap();

        assert!(storage.find_paths(&[matching]).await.unwrap().is_empty());
        assert_eq!(storage.find_paths(&[retained]).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_delete_processor_contents_and_paths() {
        let storage = SeaProcessingStorage::new("sqlite::memory:").await.unwrap();
        let first_id = storage
            .save_processing_content(&create_test_processing_content(
                "selected",
                ProcessingStatus::Renamed,
            ))
            .await
            .unwrap();
        let second_id = storage
            .save_processing_content(&create_test_processing_content(
                "selected",
                ProcessingStatus::Failure,
            ))
            .await
            .unwrap();
        let retained_id = storage
            .save_processing_content(&create_test_processing_content(
                "retained",
                ProcessingStatus::Renamed,
            ))
            .await
            .unwrap();
        storage
            .save_paths(vec![
                ProcessingTargetPath {
                    path: "/selected/one".to_owned(),
                    processor_name: "selected".to_owned(),
                    item_hash: "one".to_owned(),
                },
                ProcessingTargetPath {
                    path: "/selected/two".to_owned(),
                    processor_name: "selected".to_owned(),
                    item_hash: "two".to_owned(),
                },
                ProcessingTargetPath {
                    path: "/retained/one".to_owned(),
                    processor_name: "retained".to_owned(),
                    item_hash: "one".to_owned(),
                },
            ])
            .await
            .unwrap();

        assert_eq!(
            storage.delete_processing_contents_by_processor("selected").await.unwrap(),
            2
        );
        assert_eq!(storage.delete_paths_by_processor("selected").await.unwrap(), 2);

        assert!(storage.find_content_by_id(first_id).await.unwrap().is_none());
        assert!(storage.find_content_by_id(second_id).await.unwrap().is_none());
        assert!(storage.find_content_by_id(retained_id).await.unwrap().is_some());
        let retained_paths = storage
            .find_paths(&[
                "/selected/one".to_owned(),
                "/selected/two".to_owned(),
                "/retained/one".to_owned(),
            ])
            .await
            .unwrap();
        assert_eq!(retained_paths.len(), 1);
        assert_eq!(retained_paths[0].processor_name, "retained");
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

mod target_path {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "target_path")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub processor_name: String,
        pub item_hash: String,
        pub created_at: OffsetDateTime,
    }

    impl ActiveModelBehavior for ActiveModel {}
}
