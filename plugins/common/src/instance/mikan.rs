use moka::future::Cache;
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use source_downloader_sdk::component::format_error_chain;
use source_downloader_sdk::http::header;
use std::sync::{Arc, LazyLock};

// 常量定义
const TOKEN_COOKIE: &str = ".AspNetCore.Identity.Application";

#[allow(dead_code)]
static BANGUMI_CACHE: LazyLock<Cache<UrlKey, BangumiPageInfo>> =
    LazyLock::new(|| Cache::builder().max_capacity(500).build());
static EPISODE_CACHE: LazyLock<Cache<UrlKey, EpisodePageInfo>> =
    LazyLock::new(|| Cache::builder().max_capacity(500).build());

#[derive(Clone)]
pub struct MikanClient {
    token: Option<String>,
    http_client: Client,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct UrlKey {
    url: String,
    token: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct BangumiPageInfo {
    pub bgm_tv_subject_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct EpisodePageInfo {
    pub bangumi_title: Option<String>,
    pub mikan_href: Option<String>,
    pub fansub_rss: Option<String>,
}

impl MikanClient {
    pub fn new(token: Option<String>, http_client: Client) -> Self {
        Self { token, http_client }
    }

    /// 获取 Bangumi 页面信息
    #[allow(dead_code)]
    pub async fn get_bangumi_page_info(
        &self,
        url: &str,
    ) -> Result<BangumiPageInfo, Arc<reqwest::Error>> {
        let key = UrlKey { url: url.to_string(), token: self.token.clone() };
        let future = self.fetch_bangumi_page_info(key.url.clone());
        BANGUMI_CACHE.try_get_with(key, future).await
    }

    /// 获取 Episode 页面信息
    pub async fn get_episode_page_info(
        &self,
        url: &str,
    ) -> Result<EpisodePageInfo, String> {
        let future = self.fetch_episode_page_info(url);
        // 访问全局缓存
        EPISODE_CACHE
            .try_get_with(
                UrlKey { url: url.to_owned(), token: self.token.to_owned() },
                future,
            )
            .await
            .map_err(|error| format_error_chain(error.as_ref()))
    }

    // --- 静态抓取逻辑 (Private Static Methods) ---

    #[allow(dead_code)]
    async fn fetch_bangumi_page_info(
        &self,
        url_str: String,
    ) -> Result<BangumiPageInfo, reqwest::Error> {
        let html =
            Self::fetch_html(&self.http_client, &url_str, self.token.as_deref()).await?;
        let document = Html::parse_document(&html);
        let selector = Selector::parse(".bangumi-info a").unwrap();
        let subject_id = document.select(&selector).find_map(|element| {
            let text = element.text().collect::<String>();
            if !text.is_empty()
                && text.contains("/subject/")
                && let Ok(uri) = Url::parse(&text)
            {
                return uri.path_segments()?.next_back().map(str::to_owned);
            }
            None
        });

        Ok(BangumiPageInfo { bgm_tv_subject_id: subject_id })
    }

    async fn fetch_episode_page_info(
        &self,
        uri: &str,
    ) -> Result<EpisodePageInfo, reqwest::Error> {
        let base_url = Url::parse(uri).unwrap();

        let html =
            Self::fetch_html(&self.http_client, uri, self.token.as_deref()).await?;
        let document = Html::parse_document(&html);

        let title_selector = Selector::parse(".bangumi-title a").unwrap();
        let title_element = document.select(&title_selector).next();

        let bangumi_title =
            title_element.map(|el| el.text().collect::<String>().trim().to_string());

        let mikan_href = title_element
            .and_then(|el| el.value().attr("href"))
            .map(|href| resolve_url(&base_url, href));

        let rss_selector = Selector::parse(".mikan-rss").unwrap();
        let fansub_rss = document
            .select(&rss_selector)
            .next()
            .and_then(|el| el.value().attr("href"))
            .map(|href| resolve_url(&base_url, href));

        Ok(EpisodePageInfo { bangumi_title, mikan_href, fansub_rss })
    }

    async fn fetch_html(
        client: &Client,
        url: &str,
        token: Option<&str>,
    ) -> Result<String, reqwest::Error> {
        let mut req = client.get(url);

        // 设置 Cookie
        if let Some(t) = token {
            req = req.header(header::COOKIE, format!("{}={}", TOKEN_COOKIE, t));
        }

        let resp = req.send().await?;
        let text = resp.text().await?;
        Ok(text)
    }
}

// 辅助函数：处理绝对路径拼接 (模拟 Jsoup 的 abs:href)
fn resolve_url(base: &Url, relative: &str) -> String {
    match base.join(relative) {
        Ok(u) => u.to_string(),
        Err(_) => relative.to_string(), // fallback
    }
}
