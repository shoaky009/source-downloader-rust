use crate::expression::cel::{CelCompiledExpressionFactory, FACTORY};
use crate::expression::{CompiledExpression, CompiledExpressionFactory};
use regex::Regex;
use serde_json::{Map, Value, json};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    FileContent, FileContentStatus, PatternVariables, SourceFile, Trimmer, VariableReplacer,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, LazyLock, OnceLock};

pub static EXPRESSION_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(.+?)}|:\{(.+?)}").unwrap());
pub static EXTENSION_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.([a-zA-Z0-9]{1,10})$").unwrap());
pub static VARIABLE_PATTERN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(?P<normal>.+?)}|:\{(?P<optional>.+?)}").unwrap());
pub const OPTIONAL_EXPRESSION_PREFIX: &str = ":";

#[derive(Clone)]
pub struct RawFileContent<'a> {
    pub save_path: &'a Path,
    pub download_path: &'a Path,
    pub variables: &'a PatternVariables,
    pub save_path_pattern: &'a PathPattern,
    pub filename_pattern: &'a PathPattern,
    pub source_file: &'a SourceFile,
}

impl<'a> RawFileContent<'a> {
    pub fn file_download_path(&self) -> PathBuf {
        self.download_path.join(&self.source_file.path)
    }

    pub fn get_path_original_layout(&self) -> Vec<String> {
        let path = if self.source_file.path.is_absolute() {
            self.source_file
                .path
                .strip_prefix(&self.download_path)
                .unwrap_or(&self.source_file.path)
        } else {
            &self.source_file.path
        };

        path.components()
            .skip(1) // drop(1)
            .collect::<Vec<_>>()
            .split_last()
            .map(|(_, head)| head)
            .unwrap_or(&[])
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    }
}

#[cfg_attr(test, derive(Clone))]
#[allow(unused)]
pub struct Renamer {
    pub variable_error_strategy: VariableErrorStrategy,
    pub variable_replacers: Vec<Arc<dyn VariableReplacer>>,
    pub variable_process_chain: Vec<String>,
    pub trimming: HashMap<String, Vec<Arc<dyn Trimmer>>>,
    pub path_name_length_limit: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VariableErrorStrategy {
    Original,
    ToUnresolved,
    #[allow(unused)]
    Pattern,
}

#[derive(Debug, Clone)]
pub struct RenameVariables {
    pub variables: Map<String, Value>,
    pub processed_variables: HashMap<String, String>,
    pub pattern_variables: HashMap<String, String>,
    pub trim_variables: HashMap<String, String>,
    all_variables_cache: OnceLock<Map<String, Value>>,
}

impl RenameVariables {
    /// 返回所有变量的组合 Map
    pub fn all_variables(&self) -> &Map<String, Value> {
        self.all_variables_cache.get_or_init(|| {
            let mut all = self.variables.clone();
            for (k, v) in &self.processed_variables {
                all.insert(k.clone(), Value::String(v.clone()));
            }
            for (k, v) in &self.pattern_variables {
                all.insert(k.clone(), Value::String(v.clone()));
            }
            for (k, v) in &self.trim_variables {
                all.insert(k.clone(), Value::String(v.clone()));
            }
            all
        })
    }
}

impl Default for RenameVariables {
    fn default() -> Self {
        Self {
            variables: Map::new(),
            processed_variables: HashMap::new(),
            pattern_variables: HashMap::new(),
            trim_variables: HashMap::new(),
            all_variables_cache: OnceLock::new(),
        }
    }
}

struct ParseResult {
    pub path: String,
    pub success: bool,
    pub failed_expressions: Vec<String>,
}

// 这里的流程都可以放到Processor里，但是不方便单元测试
impl Default for Renamer {
    fn default() -> Self {
        Self {
            variable_error_strategy: VariableErrorStrategy::Original,
            variable_replacers: vec![],
            variable_process_chain: vec![],
            trimming: HashMap::new(),
            path_name_length_limit: 255,
        }
    }
}

pub struct PathPattern {
    pub pattern: String,
    expressions: Vec<ExpressionWrapper>,
}

impl PathPattern {
    pub fn new(pattern: String, fac: &CelCompiledExpressionFactory) -> Self {
        if pattern.is_empty() {
            return Self {
                pattern,
                expressions: vec![],
            };
        }
        let expressions = Self::compile_expressions(&pattern, fac);
        Self {
            pattern,
            expressions,
        }
    }

    pub fn new_cel(pattern: String) -> Self {
        Self::new(pattern, &FACTORY)
    }

    fn compile_expressions(
        pattern: &str,
        expression_factory: &CelCompiledExpressionFactory,
    ) -> Vec<ExpressionWrapper> {
        let mut expressions: Vec<ExpressionWrapper> = Vec::new();
        // 迭代所有正则匹配项
        for cap in VARIABLE_PATTERN_REGEX.captures_iter(pattern) {
            // 获取完整的原始文本，例如 "{name}" 或 ":{age}"
            let raw_full_text = cap.get(0).unwrap().as_str();

            // 判断是否为可选 (以 : 开头)
            let is_optional = raw_full_text.starts_with(OPTIONAL_EXPRESSION_PREFIX);

            // 提取中间的表达式内容
            // 如果是 Group 1 (normal) 有值则取它，否则取 Group 2 (optional)
            let expression_content = cap
                .name("normal")
                .or_else(|| cap.name("optional"))
                .map(|m| m.as_str())
                .unwrap_or("");

            // 调用工厂创建表达式对象
            let expression = expression_factory
                .create::<String>(expression_content)
                .unwrap();
            expressions.push(ExpressionWrapper {
                expression,
                optional: is_optional,
                original: expression_content.to_owned(),
            });
        }

        expressions
    }
}

struct ExpressionWrapper {
    expression: Box<dyn CompiledExpression<String>>,
    optional: bool,
    original: String,
}

impl Renamer {
    pub fn create_file_content(
        &self,
        source_item: &SourceItem,
        file: RawFileContent,
        extra_variables: &RenameVariables,
    ) -> FileContent {
        let mut variables = self.file_rename_variables(source_item, &file, extra_variables);
        let mut dir_result = self.save_directory_path(&file, &variables);
        let mut filename_result = self.target_filename(&file, &variables);

        let mut errors = Vec::new();
        errors.extend(dir_result.failed_expressions.clone());
        errors.extend(filename_result.failed_expressions.clone());

        // 处理 STAY 策略
        if !filename_result.success
            && self.variable_error_strategy == VariableErrorStrategy::Original
        {
            let dp = file.file_download_path();
            let filename = dp.file_name().and_then(|s| s.to_str()).unwrap();
            let parent = dp.parent().unwrap_or(Path::new(""));
            return FileContent {
                download_path: file.download_path.to_owned(),
                file_download_path: file.file_download_path(),
                source_save_path: file.save_path.to_owned(),
                pattern_variables: variables.processed_variables,
                target_filename: filename.to_owned(),
                target_save_path: parent.to_path_buf(),
                exist_target_path: None,
                tags: file.source_file.tags.to_owned(),
                attrs: file.source_file.attrs.to_owned(),
                file_uri: file.source_file.download_uri.to_owned(),
                errors,
                status: FileContentStatus::UNDETECTED,
            };
        }
        if !self.trimming.is_empty() {
            // 校验文件名长度 (UTF-8 bytes)
            if filename_result.path.as_bytes().len() > self.path_name_length_limit {
                let mut trim_vars = variables.trim_variables.clone();
                self.execute_trim(
                    &file.filename_pattern.pattern,
                    &filename_result.path,
                    &variables.variables,
                    &mut trim_vars,
                );
                variables.trim_variables = trim_vars;
                filename_result = self.target_filename(&file, &variables);
            }

            // 校验目录段长度
            let rel_path = Path::new(&dir_result.path)
                .strip_prefix(&file.save_path)
                .unwrap_or(Path::new(""));
            let mut current_trim_vars = variables.trim_variables.clone();
            let mut needs_recalc_dir = false;

            for (index, component) in rel_path.components().enumerate() {
                let segment_name = component.as_os_str().to_str().unwrap_or("");
                if segment_name.as_bytes().len() > self.path_name_length_limit {
                    let segments = file
                        .save_path_pattern
                        .pattern
                        .split("/")
                        .collect::<Vec<_>>();
                    if let Some(pattern_part) = segments.get(index) {
                        self.execute_trim(
                            pattern_part,
                            &segment_name,
                            &variables.variables,
                            &mut current_trim_vars,
                        );
                        needs_recalc_dir = true;
                    }
                }
            }
            if needs_recalc_dir {
                variables.trim_variables = current_trim_vars;
                dir_result = self.save_directory_path(&file, &variables);
            }
        }

        FileContent {
            download_path: file.download_path.to_owned(),
            file_download_path: file.file_download_path(),
            source_save_path: file.save_path.to_owned(),
            pattern_variables: variables.processed_variables,
            target_filename: filename_result.path,
            target_save_path: PathBuf::from_str(&dir_result.path).unwrap(),
            exist_target_path: None,
            tags: file.source_file.tags.to_owned(),
            attrs: file.source_file.attrs.to_owned(),
            file_uri: file.source_file.download_uri.to_owned(),
            errors,
            status: FileContentStatus::UNDETECTED,
        }
    }

    // =====
    fn execute_trim(
        &self,
        pattern: &str,
        path_name: &str,
        all_vars: &Map<String, Value>,
        trim_variables: &mut HashMap<String, String>,
    ) {
        for (var_name, trimmers) in &self.trimming {
            if !pattern.contains(var_name) {
                continue;
            }

            if let Some(Value::String(val)) = all_vars.get(var_name) {
                let expect_size = self.expect_variable_byte_size(path_name, val);
                let mut trimmed_val = val.clone();
                for trimmer in trimmers {
                    trimmed_val = trimmer.trim(trimmed_val, expect_size);
                }
                trim_variables.insert(var_name.clone(), trimmed_val);
            }
        }
    }

    fn expect_variable_byte_size(&self, too_long_path: &str, variable_val: &str) -> usize {
        let without_variable_size = too_long_path.replace(variable_val, "").as_bytes().len();
        if self.path_name_length_limit > without_variable_size {
            self.path_name_length_limit - without_variable_size
        } else {
            0
        }
    }

    fn parse(&self, variables: &RenameVariables, path_pattern: &PathPattern) -> ParseResult {
        if path_pattern.pattern.is_empty() {
            return ParseResult {
                path: "".to_owned(),
                success: true,
                failed_expressions: vec![],
            };
        }

        let mut failed_expressions = Vec::new();
        let mut success = true;
        let mut expression_index = 0;
        let mut last_match_end = 0;
        // TODO 要从上游引用表达式
        let expressions = &path_pattern.expressions;
        let mut path_builder = String::new();

        // 遍历所有匹配项
        let raw_pattern = &path_pattern.pattern;
        for mat in EXPRESSION_REGEX.find_iter(raw_pattern) {
            let expression = &expressions[expression_index];
            let value = expression.expression.execute(variables.all_variables());
            // 1. 添加当前匹配项之前的普通文本 (类似 matcher.appendReplacement 的非替换部分)
            path_builder.push_str(&raw_pattern[last_match_end..mat.start()]);
            if value.is_err() && !expression.optional {
                success = false;
                failed_expressions.push(format!(
                    "{} => {}",
                    expression.original,
                    value.as_ref().err().unwrap().clone()
                ));
            }
            // 2. 替换匹配到的内容
            if let Ok(val) = value {
                // Rust 的 String 拼接不需要 Matcher.quoteReplacement，直接放入即可
                path_builder.push_str(&val);
            } else if expression.optional {
                // 可选且为空，则替换为空字符串，即什么都不做
            } else {
                // 如果不可选且没有值，保留占位符或者按业务逻辑处理
                // Kotlin 原代码中此处什么都没做，可能会导致占位符直接消失，建议保留原样或标记失败
                path_builder.push_str(mat.as_str());
            }

            last_match_end = mat.end();
            expression_index += 1;
        }

        // 3. 添加剩余的文本 (类似 matcher.appendTail)
        path_builder.push_str(&raw_pattern[last_match_end..]);

        ParseResult {
            path: path_builder,
            success,
            failed_expressions,
        }
    }

    fn target_filename(&self, file: &RawFileContent, variables: &RenameVariables) -> ParseResult {
        let file_download_path = file.file_download_path();
        let pattern = &file.filename_pattern;
        if pattern.pattern.is_empty() {
            return ParseResult {
                path: file_download_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap()
                    .to_owned(),
                success: true,
                failed_expressions: vec![],
            };
        }

        let mut result = self.parse(&variables, &file.filename_pattern);

        let file_name = file_download_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap()
            .to_owned();
        if result.success {
            if result.path.trim().is_empty() {
                result.path = file_name;
                return result;
            }

            let ext = EXTENSION_REGEX
                .captures(&file_name)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str());

            if let Some(e) = ext {
                if !result.path.ends_with(e) {
                    result.path = format!("{}.{}", result.path, e);
                }
            }
            return result;
        }

        // 错误策略处理
        match self.variable_error_strategy {
            VariableErrorStrategy::Original | VariableErrorStrategy::ToUnresolved => {
                result.path = file_name;
            }
            VariableErrorStrategy::Pattern => {
                let ext = file_download_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if !result.path.ends_with(ext) && !ext.is_empty() {
                    result.path = format!("{}.{}", result.path, ext);
                }
            }
        }
        result
    }

    fn save_directory_path(
        &self,
        file: &RawFileContent,
        variables: &RenameVariables,
    ) -> ParseResult {
        let mut parse = self.parse(&variables, &file.save_path_pattern);
        let source_path = &file.save_path;

        if parse.success {
            let mut final_path = source_path.join(&parse.path);
            if self.variable_error_strategy == VariableErrorStrategy::ToUnresolved {
                let file_parse = self.parse(&variables, &file.filename_pattern);
                if !file_parse.success {
                    final_path = final_path.join("unresolved");
                }
            }
            parse.path = final_path.to_string_lossy().into_owned();
            return parse;
        }

        let fallback_path = match self.variable_error_strategy {
            VariableErrorStrategy::Original => file
                .file_download_path()
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf(),
            VariableErrorStrategy::Pattern => source_path.join(&parse.path),
            VariableErrorStrategy::ToUnresolved => {
                let rel = file
                    .file_download_path()
                    .strip_prefix(&file.download_path)
                    .map(|p| p.parent().unwrap_or(Path::new("")))
                    .unwrap_or(Path::new(""))
                    .to_path_buf();
                source_path.join("unresolved").join(rel)
            }
        };

        parse.path = fallback_path.to_str().unwrap().to_owned();
        parse
    }

    fn file_rename_variables(
        &self,
        _: &SourceItem,
        file: &RawFileContent,
        extra: &RenameVariables,
    ) -> RenameVariables {
        let mut vars = Map::new();
        let file_pattern_vars = self.apply_replacers_to_map(&file.variables);
        for (k, v) in &file_pattern_vars {
            vars.insert(k.clone(), Value::String(v.clone()));
        }

        // 构建 file 对象变量
        let file_obj = json!({
            "name": self.apply_replacers("file.name", file.file_download_path().file_stem().unwrap().to_string_lossy().into_owned()),
            "attrs": file.source_file.attrs,
            "tags": file.source_file.tags,
            "originalLayout": file.get_path_original_layout().into_iter()
                .map(|s| self.apply_replacers("file.originalLayout", s))
                .collect::<Vec<_>>()
            // Windows?
                .join("/")
        });
        vars.insert("file".to_string(), file_obj);

        // 合并 extra
        for (k, v) in &extra.variables {
            vars.entry(k.to_owned()).or_insert(v.to_owned());
        }

        RenameVariables {
            variables: vars,
            processed_variables: extra.processed_variables.to_owned(),
            ..Default::default()
        }
    }

    fn apply_replacers(&self, name: &str, mut text: String) -> String {
        for replacer in &self.variable_replacers {
            text = replacer.replace(name, text);
        }
        text
    }

    fn apply_replacers_to_map(&self, map: &HashMap<String, String>) -> HashMap<String, String> {
        map.iter()
            .map(|(k, v)| (k.clone(), self.apply_replacers(k, v.clone())))
            .collect()
    }

    // TODO 未完成
    pub fn item_rename_variables(&self, item: &SourceItem, _: PatternVariables) -> RenameVariables {
        // val vars = mutableMapOf<String, Any>()
        // val replacedItemVars = itemVariables.variables().replaceVariables()
        // vars.putAll(replacedItemVars)
        // vars["item"] = buildSourceItemRenameVariables(sourceItem)
        // val (variables, _) = processVariable(sourceItem, vars, false)
        // return RenameVariables(vars, variables.replaceVariables(), replacedItemVars)
        let mut vars: Map<String, Value> = Map::new();
        vars.insert(
            "title".to_owned(),
            json!(self.apply_replacers("item.title", item.title.clone())),
        );
        vars.insert("datetime".to_owned(), json!(item.datetime));
        vars.insert("date".to_owned(), json!(item.datetime.date()));
        vars.insert("year".to_owned(), json!(item.datetime.year()));
        vars.insert("month".to_owned(), json!(item.datetime.month() as u8));
        vars.insert("contentType".to_owned(), json!(item.content_type));
        vars.insert("attrs".to_owned(), json!(item.attrs));
        let mut item: Map<String, Value> = Map::new();
        item.insert("item".to_owned(), vars.into());
        RenameVariables {
            variables: item,
            processed_variables: HashMap::new(),
            pattern_variables: HashMap::new(),
            trim_variables: HashMap::new(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
#[allow(clippy::redundant_clone)]
mod tests {
    use super::*;
    use crate::process::file::VariableErrorStrategy::Pattern;
    use maplit::hashmap;
    use std::str::FromStr;
    use std::sync::LazyLock;

    static DEFAULT_RENAMER: LazyLock<Renamer> = LazyLock::new(|| Renamer::default());
    static SOURCE_SAVE_PATH: LazyLock<&Path> =
        LazyLock::new(|| Path::new("src/test/resources/target"));
    static DOWNLOAD_PATH: LazyLock<&Path> =
        LazyLock::new(|| Path::new("src/test/resources/download"));
    static PATH_PATTERN: LazyLock<Arc<PathPattern>> =
        LazyLock::new(|| Arc::new(PathPattern::new_cel("".to_owned())));
    static SOURCE_FILE: LazyLock<SourceFile> = LazyLock::new(|| SourceFile {
        path: PathBuf::from_str("1.txt").unwrap(),
        attrs: Default::default(),
        download_uri: None,
        tags: Default::default(),
        data: None,
    });
    static PATTERN: LazyLock<PatternVariables> = LazyLock::new(|| PatternVariables::new());

    impl<'a> Default for RawFileContent<'a> {
        fn default() -> Self {
            Self {
                save_path: &SOURCE_SAVE_PATH,
                download_path: &DOWNLOAD_PATH,
                variables: &PATTERN,
                save_path_pattern: &PATH_PATTERN,
                filename_pattern: &PATH_PATTERN,
                source_file: &SOURCE_FILE,
            }
        }
    }

    #[test]
    fn given_empty_should_filename_use_origin_name() {
        let raw = RawFileContent::default();
        let content = DEFAULT_RENAMER.create_file_content(
            &SourceItem::default(),
            raw,
            &RenameVariables::default(),
        );
        assert_eq!("1.txt", content.target_filename);

        assert_eq!(
            SOURCE_SAVE_PATH.join(&content.target_filename),
            content.target_path()
        );
    }

    #[test]
    fn given_constant_pattern_should_filename_expected() {
        let raw = RawFileContent {
            filename_pattern: &PathPattern::new_cel("3".to_owned()),
            save_path_pattern: &PathPattern::new_cel("2".to_owned()),
            ..Default::default()
        };
        let content = DEFAULT_RENAMER.create_file_content(
            &SourceItem::default(),
            raw,
            &RenameVariables::default(),
        );
        assert_eq!("3.txt", content.target_filename);
        assert_eq!(SOURCE_SAVE_PATH.join("2/3.txt"), content.target_path());
    }

    #[test]
    fn given_vars_pattern_should_filename_expected() {
        let raw = RawFileContent {
            filename_pattern: &PathPattern::new_cel("{date} - {title}".to_owned()),
            save_path_pattern: &PathPattern::new_cel("{year}/{work}".to_owned()),
            variables: &hashmap! {
              "date".to_owned() => "2022-01-01".to_owned(),
              "work".to_owned() => "test".to_owned(),
              "year".to_owned() => "2022".to_owned(),
              "title".to_owned() => "123".to_owned(),
            },
            ..Default::default()
        };
        let content = DEFAULT_RENAMER.create_file_content(
            &SourceItem::default(),
            raw,
            &RenameVariables::default(),
        );
        assert_eq!(
            SOURCE_SAVE_PATH.join("2022/test/2022-01-01 - 123.txt"),
            content.target_path()
        )
    }

    #[test]
    fn given_extra_vars() {
        let raw = RawFileContent {
            save_path_pattern: &PathPattern::new_cel("{name}/S{season}".to_owned()),
            variables: &hashmap! {
              "name".to_owned() => "test".to_owned(),
            },
            ..Default::default()
        };
        let mut extra = RenameVariables::default();
        extra.variables.insert("season".to_string(), json!("01"));
        let content = DEFAULT_RENAMER.create_file_content(&SourceItem::default(), raw, &extra);
        assert_eq!(
            SOURCE_SAVE_PATH.join("test").join("S01"),
            content.target_save_path
        )
    }

    #[test]
    fn given_extension_pattern_should_expected() {
        let raw = RawFileContent {
            filename_pattern: &PathPattern::new_cel("{name} - {season}.mp4".to_owned()),
            variables: &hashmap! {
              "name".to_owned() => "test".to_owned(),
            },
            source_file: &SourceFile {
                path: PathBuf::from_iter([
                    "src",
                    "test",
                    "resources",
                    "downloads",
                    "easd",
                    "222",
                    "1.mp4",
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut extra = RenameVariables::default();
        extra.variables.insert("season".to_string(), json!("01"));
        let content = DEFAULT_RENAMER.create_file_content(&Default::default(), raw, &extra);
        assert_eq!("test - 01.mp4", content.target_filename);
    }

    #[test]
    fn test_variable_error_given_original_strategy() {
        let mut raw = RawFileContent {
            filename_pattern: &PathPattern::new_cel("{name} - {season}".to_owned()),
            save_path_pattern: &PathPattern::new_cel("{name}/S{season}".to_owned()),
            variables: &hashmap! {
              "season".to_owned() => "01".to_owned(),
            },
            ..Default::default()
        };
        let content = DEFAULT_RENAMER.create_file_content(
            &SourceItem::default(),
            raw.clone(),
            &RenameVariables::default(),
        );
        assert_eq!(content.file_download_path, content.target_path());
        assert_eq!(2, content.errors.len());

        // 1 depth
        let new_pattern = PathPattern::new_cel("S{season}".to_owned());
        raw.save_path_pattern = &new_pattern;
        let content = DEFAULT_RENAMER.create_file_content(
            &SourceItem::default(),
            raw,
            &RenameVariables::default(),
        );
        assert_eq!(content.file_download_path, content.target_path());
    }

    #[test]
    fn test_variable_error_given_pattern_strategy() {
        let raw = RawFileContent {
            filename_pattern: &PathPattern::new_cel("{name} - {season}".to_owned()),
            save_path_pattern: &PathPattern::new_cel("{name}/S{season}".to_owned()),
            variables: &hashmap! {
              "season".to_owned() => "01".to_owned(),
            },
            ..Default::default()
        };
        let renamer = Renamer {
            variable_error_strategy: Pattern,
            ..DEFAULT_RENAMER.clone()
        };
        let content =
            renamer.create_file_content(&SourceItem::default(), raw, &RenameVariables::default());
        assert_eq!(
            SOURCE_SAVE_PATH
                .join("{name}")
                .join("S01")
                .join("{name} - 01.txt"),
            content.target_path()
        );
    }

    #[test]
    fn given_unresolved_filename_with_dir_item() {
        let raw = RawFileContent {
            save_path_pattern: &PathPattern::new_cel("{title}/S{season}".to_owned()),
            filename_pattern: &PathPattern::new_cel("{title} S{season}E{episode}".to_owned()),
            variables: &hashmap! {
                "season".to_owned() => "01".to_owned(),
                "title".to_owned() => "test 01".to_owned(),
            },
            source_file: &SourceFile {
                path: PathBuf::from("1.txt"),
                ..Default::default()
            },
            ..Default::default()
        };

        let renamer = Renamer {
            variable_error_strategy: VariableErrorStrategy::ToUnresolved,
            ..DEFAULT_RENAMER.clone()
        };

        let content =
            renamer.create_file_content(&SourceItem::default(), raw, &RenameVariables::default());

        // 预期路径: test 01/S01/unresolved/1.txt
        let path = PathBuf::from_iter(["test 01", "S01", "unresolved", "1.txt"]);
        assert_eq!(SOURCE_SAVE_PATH.join(path), content.target_path());
    }

    #[test]
    fn given_unresolved_save_path_with_dir_item() {
        let raw = RawFileContent {
            source_file: &SourceFile {
                path: PathBuf::from_iter(["FATE", "AAAAA.mp4"]),
                ..Default::default()
            },
            save_path_pattern: &PathPattern::new_cel("{title}".to_string()),
            filename_pattern: &PathPattern::new_cel("S{season}E{episode}".to_string()),
            variables: &hashmap! {
                "season".to_owned() => "01".to_owned(),
                "episode".to_owned() => "02".to_owned(),
            },
            ..Default::default()
        };

        let renamer = Renamer {
            variable_error_strategy: VariableErrorStrategy::ToUnresolved,
            ..DEFAULT_RENAMER.clone()
        };

        let content =
            renamer.create_file_content(&SourceItem::default(), raw, &RenameVariables::default());

        // {title} 缺失，进入 unresolved 分支
        let path = PathBuf::from_iter(["unresolved", "FATE", "S01E02.mp4"]);
        assert_eq!(SOURCE_SAVE_PATH.join(path), content.target_path());
    }

    #[test]
    fn given_both_unresolved_with_dir_item() {
        let raw = RawFileContent {
            source_file: &SourceFile {
                path: PathBuf::from_iter(["FATE", "AAAAA.mp4"]),
                ..Default::default()
            },
            save_path_pattern: &PathPattern::new_cel("{Title}".to_string()),
            filename_pattern: &PathPattern::new_cel("S{Season}E{Episod}".to_string()),
            variables: &hashmap! {
                "season".to_owned() => "01".to_owned(),
                "episode".to_owned() => "02".to_owned(),
            },
            ..Default::default()
        };

        let renamer = Renamer {
            variable_error_strategy: VariableErrorStrategy::ToUnresolved,
            ..DEFAULT_RENAMER.clone()
        };

        let content =
            renamer.create_file_content(&SourceItem::default(), raw, &RenameVariables::default());

        // 全部缺失，回退到原始路径
        let path = PathBuf::from_iter(["unresolved", "FATE", "AAAAA.mp4"]);
        assert_eq!(SOURCE_SAVE_PATH.join(path), content.target_path());
    }

    #[test]
    fn normal_parse() {
        let mut variables = RenameVariables::default();
        variables.variables.insert("name".to_owned(), json!("111"));
        variables
            .variables
            .insert("title".to_owned(), json!("test"));
        let parse_result = DEFAULT_RENAMER.parse(
            &variables,
            &PathPattern::new_cel("{name}/{title}abc".to_string()),
        );
        assert_eq!("111/testabc", parse_result.path);
        assert!(parse_result.success && parse_result.failed_expressions.is_empty());
    }

    #[test]
    fn given_option_pattern_with_not_exists_variables() {
        let mut variables = RenameVariables::default();
        variables.variables.insert("name".to_owned(), json!("111"));
        // : 代表可选
        let parse_result = DEFAULT_RENAMER.parse(
            &variables,
            &PathPattern::new_cel("{name}/:{title}abc".to_string()),
        );
        assert_eq!("111/abc", parse_result.path);
        assert!(parse_result.success && parse_result.failed_expressions.is_empty());
    }

    #[test]
    fn given_expression() {
        let mut variables = RenameVariables::default();
        variables.variables.insert("name".to_owned(), json!("111"));
        variables.variables.insert("episode".to_owned(), json!("2"));
        variables.variables.insert("source".to_owned(), json!("1"));

        let pattern =
            PathPattern::new_cel("{'test '+name} E{episode + '1'}:{' - '+source}".to_string());

        let parse_result = DEFAULT_RENAMER.parse(&variables, &pattern);
        assert_eq!("test 111 E21 - 1", parse_result.path);

        // 测试 source 缺失的情况
        let mut variables = RenameVariables::default();
        variables.variables.insert("name".to_owned(), json!("111"));
        variables.variables.insert("episode".to_owned(), json!("2"));

        let result2 = DEFAULT_RENAMER.parse(&variables, &pattern);
        assert_eq!("test 111 E21", result2.path);
    }

    // TODO #[test]
    // fn test_replacement_given_pattern_variables_and_extra_variables() {
    //     let renamer = Renamer {
    //         variable_replacers: vec![
    //             Arc::new(RegexVariableReplacer::new(r"(?i)BDRIP", "BD")),
    //             Arc::new(RegexVariableReplacer::new(r"333", "111")),
    //         ],
    //         ..renamer().clone()
    //     };
    //
    //     // 模拟 itemRenameVariables 生成的变量
    //     let mut extra = RenameVariables::default();
    //     extra.variables.insert("item.attrs.title".to_string(), json!("333"));
    //
    //     let raw = RawFileContent {
    //         filename_pattern: "{item.attrs.title}-{source}".to_string(),
    //         variables: hashmap! { "source".to_owned() => "BDrip".to_owned() },
    //         ..Default::default()
    //     };
    //
    //     let content = renamer.create_file_content(&SourceItem::default(), raw, &extra);
    //
    //     // 333 -> 111, BDrip -> BD
    //     assert_eq!("111-BD", PathBuf::from(content.target_filename).file_stem().unwrap().to_str().unwrap());
    // }

    #[test]
    fn given_attr_variables() {
        let item = SourceItem {
            attrs: serde_json::from_str(r#"{"creatorId": "Idk111"}"#).unwrap(),
            ..Default::default()
        };
        let item_vars = DEFAULT_RENAMER.item_rename_variables(&item, hashmap! {});
        let raw = RawFileContent {
            variables: &hashmap! {
                "date".to_owned() => "2022-01-01".to_owned(),
                "work".to_owned() => "test".to_owned(),
                "year".to_owned() => "2022".to_owned(),
                "title".to_owned() => "123".to_owned(),
            },
            save_path_pattern: &PathPattern::new_cel("{item.attrs.creatorId}/{date}".to_owned()),
            filename_pattern: &PathPattern::new_cel("{file.attrs.seq}".to_owned()),
            // 假设 attrs 在 RawFileContent 中被放入了 file.attrs
            source_file: &SourceFile {
                path: PathBuf::from("2.txt"),
                attrs: serde_json::from_str(r#"{"seq": "2"}"#).unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };

        let content = DEFAULT_RENAMER.create_file_content(&SourceItem::default(), raw, &item_vars);
        let expected = SOURCE_SAVE_PATH
            .join("Idk111")
            .join("2022-01-01")
            .join("2.txt");
        assert_eq!(expected, content.target_path());
    }

    #[test]
    fn given_origin_layout_pattern() {
        // let renamer = Renamer {
        //     variable_replacers: vec![Box::new(WindowsPathReplacer)],
        //     ..renamer().clone()
        // };
        let raw = RawFileContent {
            source_file: &SourceFile {
                path: PathBuf::from_iter(["wp", "mp3", "origin", "1.mp3"]),
                ..Default::default()
            },
            save_path_pattern: &PathPattern::new_cel("wp-test/{file.originalLayout}".to_owned()),
            ..Default::default()
        };
        let content = DEFAULT_RENAMER.create_file_content(
            &SourceItem::default(),
            raw,
            &RenameVariables::default(),
        );
        let expected = SOURCE_SAVE_PATH
            .join("wp-test")
            .join("mp3")
            .join("origin")
            .join("1.mp3");
        assert_eq!(expected, content.target_path());
    }

    #[test]
    fn given_dot_filename_should_no_extension() {
        let raw = RawFileContent {
            source_file: &SourceFile {
                path: PathBuf::from_iter([
                    "downloads",
                    "xxx",
                    "_____padding_file_0_如果您看到此文件，请升级到BitComet(比特彗星)0.85或以上版本____",
                ]),
                ..Default::default()
            },
            filename_pattern: &PathPattern::new_cel("test".to_owned()),
            ..Default::default()
        };

        let content = DEFAULT_RENAMER.create_file_content(
            &SourceItem::default(),
            raw,
            &RenameVariables::default(),
        );
        assert_eq!("test", content.target_filename);
    }
}
