use sdk::async_trait::async_trait;
use sdk::http::Uri;
use std::collections::HashMap;
use sdk::time::format_description::well_known;
use sdk::time::OffsetDateTime;

/// 异步展开处理器 trait，用于定义如何异步展开一个 Item
#[async_trait]
#[allow(dead_code)]
pub trait ExpandHandler<T, U>: Send + Sync {
    /// 异步展开一个 Item 为多个 Items
    async fn expand(&self, item: T) ->  IterationResult<U>;
}

/// AsyncExpandIterator - 支持异步 trait 实现的展开迭代器
/// 用于展开迭代中需要执行异步操作（如网络请求）的场景
#[allow(dead_code)]
pub struct AsyncExpandIterator<T, U> {
    items: Vec<T>,
    limit: usize,
    current_index: usize,
    current_expanded: Vec<U>,
    expander: Box<dyn ExpandHandler<T, U>>,
}

impl<T: Send + 'static, U: Send + 'static> AsyncExpandIterator<T, U> {
    /// 创建新的 AsyncExpandIterator，接受实现 ExpandHandler trait 的对象
    ///
    /// # 使用示例
    /// ```ignore
    /// struct MyExpander;
    ///
    /// #[async_trait]
    /// impl ExpandHandler<SourceItem, PointedItem> for MyExpander {
    ///     async fn expand(&self, item: SourceItem) -> IterationResult<PointedItem> {
    ///         let fansub_items = fetch_fansub_items(&item).await;
    ///         IterationResult {
    ///             items: fansub_items,
    ///             continue_expand: true,
    ///         }
    ///     }
    /// }
    ///
    /// let expand_iter = AsyncExpandIterator::new(items, 100, Arc::new(MyExpander));
    /// let expanded_items = expand_iter.collect_all().await;
    /// ```
    #[allow(dead_code)]
    pub fn new(items: Vec<T>, limit: usize, expander: Box<dyn ExpandHandler<T, U>>) -> Self {
        Self {
            items,
            limit: if limit == 0 { usize::MAX } else { limit },
            current_index: 0,
            current_expanded: Vec::new(),
            expander,
        }
    }

    /// 异步迭代，返回所有展开后的 Items
    #[allow(dead_code)]
    pub async fn collect_all(mut self) -> Vec<U> {
        let mut result = Vec::new();

        while let Some(item) = self.next().await {
            result.push(item);
            if result.len() >= self.limit {
                break;
            }
        }

        result
    }

    /// 获取下一个展开的 Item
    #[allow(dead_code)]
    pub async fn next(&mut self) -> Option<U> {
        loop {
            // 如果当前展开的列表有元素，直接返回
            if !self.current_expanded.is_empty() {
                return Some(self.current_expanded.remove(0));
            }

            // 如果已经处理完所有初始 Items，结束迭代
            if self.current_index >= self.items.len() {
                return None;
            }

            // 从初始 Items 中取出下一个 Item 进行异步展开
            let item = self.items.remove(self.current_index);
            let result = self.expander.expand(item).await;

            self.current_expanded = result.items;

            // 如果展开器返回 false，停止继续展开
            if !result.continue_expand {
                return self.current_expanded.pop();
            }

            // 检查是否达到了 limit
            if self.current_expanded.len() > 0 {
                continue;
            }
        }
    }
}

/// 迭代结果，包含展开后的 Items 和是否继续标志
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct IterationResult<T> {
    pub items: Vec<T>,
    pub continue_expand: bool,
}

/// ExpandIterator - 用于展开迭代的核心结构
/// 将初始的 Items 通过闭包进行展开，每个 Item 可以扩展为多个子 Items
/// 类似 Kotlin 版本的 ExpandIterator<SourceItem, PointedItem<ItemPointer>>
#[allow(dead_code)]
pub struct ExpandIterator<T, U> {
    items: Vec<T>,
    limit: usize,
    current_index: usize,
    current_expanded: Vec<U>,
    expander: Box<dyn Fn(T) -> IterationResult<U>>,
}

impl<T, U> ExpandIterator<T, U> {
    /// 创建新的 ExpandIterator
    ///
    /// # 参数
    /// - `items`: 初始的 Items 列表
    /// - `limit`: 最多返回的 Items 数量
    /// - `expander`: 闭包函数，用于展开每个 Item
    #[allow(dead_code)]
    pub fn new<F>(items: Vec<T>, limit: usize, expander: F) -> Self
    where
        F: Fn(T) -> IterationResult<U> + 'static,
    {
        Self {
            items,
            limit: if limit == 0 { usize::MAX } else { limit },
            current_index: 0,
            current_expanded: Vec::new(),
            expander: Box::new(expander),
        }
    }
}

impl<T, U> Iterator for ExpandIterator<T, U> {
    type Item = U;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // 如果当前展开的列表有元素，直接返回
            if !self.current_expanded.is_empty() {
                return Some(self.current_expanded.remove(0));
            }

            // 如果已经处理完所有初始 Items，结束迭代
            if self.current_index >= self.items.len() {
                return None;
            }

            // 从初始 Items 中取出下一个 Item 进行展开
            let item = self.items.remove(self.current_index);
            let result = (self.expander)(item);

            self.current_expanded = result.items;

            // 如果展开器返回 false，停止继续展开
            if !result.continue_expand {
                return self.current_expanded.pop();
            }

            // 检查是否达到了 limit
            if self.current_expanded.len() > 0 {
                continue;
            }
        }
    }
}

pub fn query_map(uri: &Uri) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(query) = uri.query() else {
        return map;
    };

    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let key = it.next().unwrap_or("");
        let value = it.next().unwrap_or("");

        if !key.is_empty() {
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}


/// 从 RFC 2822 格式的日期字符串解析为 OffsetDateTime
/// RSS 的 pub_date 通常采用 RFC 2822 格式
pub fn parse_rfc2822_datetime(date_str: &str) -> Result<OffsetDateTime, Box<dyn std::error::Error>> {
    // 解析 RFC 2822 格式的日期字符串
    // RSS 的 pub_date 格式: "Wed, 16 Dec 2025 12:30:45 +0800"
    let dt: OffsetDateTime = OffsetDateTime::parse(date_str, &well_known::Rfc2822)?;
    Ok(dt)
}
