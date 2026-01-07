pub mod cel;

use sdk::component::{FileContent, SourceFile};
use sdk::serde_json::{Map, Value};
use std::any::Any;

pub trait CompiledExpressionFactory: Send + Sync {
    fn create<T>(&self, expression: &str) -> Result<Box<dyn CompiledExpression<T>>, String>
    where
        T: ExprValue;
}

pub trait CompiledExpression<T>: Send + Sync
where
    T: ExprValue,
{
    fn execute(&self, vars: &Map<String, Value>) -> Result<T, String>;
}

pub trait ExprValue: Send + Sync + Sized + 'static {
    fn from_value(value: &dyn Any) -> Result<Self, String>;
}

pub fn source_item_variables(item: &sdk::SourceItem) -> Map<String, Value> {
    let mut vars = Map::new();
    vars.insert("title".to_owned(), Value::from(item.title.to_owned()));
    vars.insert(
        "datetime".to_owned(),
        Value::from(item.datetime.to_string()),
    );
    vars.insert("year".to_owned(), Value::from(item.datetime.year()));
    vars.insert(
        "date".to_owned(),
        Value::from(item.datetime.date().to_string()),
    );
    vars.insert("month".to_owned(), Value::from(item.datetime.month() as u8));
    vars.insert("day".to_owned(), Value::from(item.datetime.day()));
    vars.insert("link".to_owned(), Value::from(item.link.to_string()));
    vars.insert(
        "downloadUri".to_owned(),
        Value::from(item.download_uri.to_string()),
    );
    vars.insert(
        "contentType".to_owned(),
        Value::from(item.content_type.to_string()),
    );
    vars.insert(
        "tags".to_owned(),
        Value::from(
            item.tags
                .iter()
                .map(|x| Value::from(x.to_string()))
                .collect::<Vec<Value>>(),
        ),
    );

    vars.insert("attrs".to_owned(), Value::from(item.attrs.to_owned()));

    let mut item_var = Map::new();
    item_var.insert("item".to_string(), Value::Object(vars));
    item_var
}

pub fn source_file_variables(file: &SourceFile) -> Map<String, Value> {
    let mut vars = Map::new();
    vars.insert(
        "name".to_owned(),
        Value::from(
            file.path
                .file_name()
                .map(|x| x.to_str())
                .flatten()
                .map(|x| x.to_string())
                .unwrap_or("".to_string()),
        ),
    );
    vars.insert(
        "extension".to_owned(),
        Value::from(
            file.path
                .extension()
                .map(|x| x.to_str())
                .flatten()
                .map(|x| x.to_string())
                .unwrap_or("".to_string()),
        ),
    );
    vars.insert(
        "tags".to_owned(),
        Value::from(
            file.tags
                .iter()
                .map(|x| Value::from(x.to_string()))
                .collect::<Vec<Value>>(),
        ),
    );
    vars.insert("attrs".to_owned(), Value::from(file.attrs.to_owned()));

    let mut file_var = Map::new();
    file_var.insert("file".to_string(), Value::Object(vars));
    file_var
}

pub fn file_content_variables(file: &FileContent) -> Map<String, Value> {
    let mut vars = Map::new();
    let path = &file.file_download_path;
    vars.insert(
        "name".to_owned(),
        Value::from(
            path.file_name()
                .map(|x| x.to_str())
                .flatten()
                .map(|x| x.to_string())
                .unwrap_or("".to_string()),
        ),
    );
    vars.insert(
        "extension".to_owned(),
        Value::from(
            path.extension()
                .map(|x| x.to_str())
                .flatten()
                .map(|x| x.to_string())
                .unwrap_or("".to_string()),
        ),
    );
    vars.insert(
        "tags".to_owned(),
        Value::from(
            file.tags
                .iter()
                .map(|x| Value::from(x.to_string()))
                .collect::<Vec<Value>>(),
        ),
    );
    vars.insert("attrs".to_owned(), Value::from(file.attrs.to_owned()));

    let pattern_variables: Map<String, Value> = file
        .pattern_variables
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();

    vars.insert("vars".to_owned(), Value::Object(pattern_variables));

    let path_segments: Vec<Value> = path
        .strip_prefix(&file.download_path)
        .ok()
        .map(|x| x.parent())
        .flatten()
        .into_iter()
        .flat_map(get_path_segments)
        .map(Value::String)
        .collect();
    vars.insert("paths".to_owned(), Value::Array(path_segments));

    let mut file_var = Map::new();
    file_var.insert("file".to_string(), Value::Object(vars));
    file_var
}

fn get_path_segments(path: &std::path::Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => os.to_str().map(String::from),
            _ => None,
        })
        .collect()
}
