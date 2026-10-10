//! Tokio context for separately linked dynamic plugins.
//!
//! Enable `plugin-runtime` and use [`crate::export_plugin!`] instead of exporting
//! `create_plugin` manually. Each plugin shares one local runtime; component
//! construction, calls and returned source streams enter it on every poll.

use crate::SourceItem;
use crate::component::*;
use crate::instance::{InstanceFactory, InstanceFactoryMetadata};
use crate::plugin::{Plugin, PluginContext, PluginDescription};
use crate::serde_json::{Map, Value};
use async_trait::async_trait;
use futures_util::{Stream, future::poll_fn};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::runtime::{Handle, Runtime};

struct PluginRuntime(Option<Runtime>);

impl PluginRuntime {
    fn new() -> Result<Self, ComponentError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|error| {
                ComponentError::new(format!("Create plugin runtime: {error}"))
            })?;
        Ok(Self(Some(runtime)))
    }

    fn with<T>(&self, call: impl FnOnce() -> T) -> T {
        let _guard = self.handle().enter();
        call()
    }

    fn handle(&self) -> &Handle {
        // The runtime is only taken while dropping its final owner.
        match &self.0 {
            Some(runtime) => runtime.handle(),
            None => unreachable!("plugin runtime has already been dropped"),
        }
    }

    async fn scope<F: Future>(&self, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        poll_fn(|cx| self.with(|| future.as_mut().poll(cx))).await
    }

    fn stream<S: Stream + Send + 'static>(
        self: Arc<Self>,
        stream: S,
    ) -> impl Stream<Item = S::Item> + Send {
        RuntimeStream { inner: Box::pin(stream), runtime: self }
    }
}

struct RuntimeStream<S> {
    // Drop the stream before releasing its runtime.
    inner: Pin<Box<S>>,
    runtime: Arc<PluginRuntime>,
}

impl<S: Stream> Stream for RuntimeStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let _guard = this.runtime.handle().enter();
        this.inner.as_mut().poll_next(cx)
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            if Handle::try_current().is_ok() {
                // Tokio cannot shut down a runtime inside an async context. Join
                // shutdown before returning so the library can be safely unloaded.
                std::thread::scope(|scope| {
                    scope.spawn(move || drop(runtime));
                });
            } else {
                drop(runtime);
            }
        }
    }
}

/// A dynamic plugin whose components use a shared plugin-local Tokio runtime.
pub struct RuntimePlugin<P> {
    inner: P,
    runtime: Arc<PluginRuntime>,
}

impl<P: Plugin> RuntimePlugin<P> {
    /// Create the runtime, then construct the plugin inside its context.
    pub fn new(factory: impl FnOnce() -> P) -> Result<Self, ComponentError> {
        let runtime = Arc::new(PluginRuntime::new()?);
        let inner = runtime.with(factory);
        Ok(Self { inner, runtime })
    }
}

impl<P: Plugin> Plugin for RuntimePlugin<P> {
    fn init(&self, context: Arc<dyn PluginContext>) {
        self.runtime.with(|| self.inner.init(context));
    }

    fn destroy(&self, context: Arc<dyn PluginContext>) {
        self.runtime.with(|| self.inner.destroy(context));
    }

    fn get_instance_factories(&self) -> Vec<Arc<dyn InstanceFactory>> {
        self.runtime
            .with(|| self.inner.get_instance_factories())
            .into_iter()
            .map(|inner| {
                Arc::new(RuntimeBound { inner, runtime: self.runtime.clone() })
                    as Arc<dyn InstanceFactory>
            })
            .collect()
    }

    fn get_component_suppliers(&self) -> Vec<Arc<dyn ComponentSupplier>> {
        self.runtime
            .with(|| self.inner.get_component_suppliers())
            .into_iter()
            .map(|inner| {
                Arc::new(RuntimeBound { inner, runtime: self.runtime.clone() })
                    as Arc<dyn ComponentSupplier>
            })
            .collect()
    }

    fn description(&self) -> PluginDescription {
        self.runtime.with(|| self.inner.description())
    }
}

/// Export a dynamic plugin with Tokio context supplied by the SDK.
///
/// Runtime creation is performed once at plugin startup. The existing loader
/// protocol cannot return an initialization error, so startup failure is fatal.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:expr) => {
        #[unsafe(no_mangle)]
        pub extern "Rust" fn create_plugin() -> std::boxed::Box<dyn $crate::plugin::Plugin> {
            match $crate::plugin_runtime::RuntimePlugin::new(|| $plugin) {
                Ok(plugin) => std::boxed::Box::new(plugin),
                Err(error) => panic!("Failed to initialize plugin runtime: {error}"),
            }
        }
    };
}

struct RuntimeBound<T: ?Sized> {
    inner: Arc<T>,
    runtime: Arc<PluginRuntime>,
}

impl<T: Debug + ?Sized> Debug for RuntimeBound<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.runtime.with(|| self.inner.fmt(f))
    }
}

impl<T: Display + ?Sized> Display for RuntimeBound<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.runtime.with(|| self.inner.fmt(f))
    }
}

macro_rules! conversions {
    ($($method:ident => $capability:ident),* $(,)?) => {
        $(fn $method(self: Arc<Self>) -> Result<Arc<dyn $capability>, ComponentError> {
            let inner = self.runtime.with(|| self.inner.clone().$method())?;
            Ok(Arc::new(RuntimeBound { inner, runtime: self.runtime.clone() }))
        })*
    };
}

impl<T: SdComponent + ?Sized> SdComponent for RuntimeBound<T> {
    conversions! {
        as_trigger => Trigger,
        as_source => Source,
        as_item_file_resolver => ItemFileResolver,
        as_downloader => Downloader,
        as_async_downloader => AsyncDownloader,
        as_file_mover => FileMover,
        as_process_listener => ProcessListener,
        as_source_item_filter => SourceItemFilter,
        as_source_file_filter => SourceFileFilter,
        as_item_content_filter => ItemContentFilter,
        as_file_content_filter => FileContentFilter,
        as_file_tagger => FileTagger,
        as_file_replacement_decider => FileReplacementDecider,
        as_file_exists_detector => FileExistsDetector,
        as_variable_provider => VariableProvider,
        as_variable_replacer => VariableReplacer,
        as_trimmer => Trimmer,
    }

    fn as_stateful(self: Arc<Self>) -> Option<Arc<dyn Stateful>> {
        let inner = self.runtime.with(|| self.inner.clone().as_stateful())?;
        Some(Arc::new(RuntimeBound { inner, runtime: self.runtime.clone() }))
    }
}

// Expand before async_trait so every delegated async method keeps the SDK's
// boxed-future contract. Enter guards only live for one poll, never an await.
macro_rules! delegate {
    ($capability:ident {
        $(fn $sync:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*
        $(async fn $name:ident(&self $(, $async_arg:ident: $async_ty:ty)*) -> $async_ret:ty;)*
        $(custom { $($extra:item)* })?
    }) => {
        #[async_trait]
        impl<T: $capability + ?Sized> $capability for RuntimeBound<T> {
            $(fn $sync(&self $(, $arg: $ty)*) -> $ret {
                self.runtime.with(|| self.inner.$sync($($arg),*))
            })*
            $(async fn $name(&self $(, $async_arg: $async_ty)*) -> $async_ret {
                self.runtime.scope(async { self.inner.$name($($async_arg),*).await }).await
            })*
            $($($extra)*)?
        }
    };
}

delegate!(ComponentSupplier {
    fn supply_types(&self) -> Vec<ComponentType>;
    fn compatibility_rules(&self) -> Vec<ComponentCompatibilityRule>;
    fn is_support_no_props(&self) -> bool;
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>>;
    custom {
        fn apply(&self, context: &dyn ComponentCreateContext, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
            let inner = self.runtime.with(|| self.inner.apply(context, props))?;
            Ok(Arc::new(RuntimeBound { inner, runtime: self.runtime.clone() }))
        }
    }
});

delegate!(InstanceFactory {
    fn instance_type_id(&self) -> TypeId;
    fn factory_name(&self) -> String;
    fn get_metadata(&self) -> Option<Box<InstanceFactoryMetadata>>;
    fn create_instance(&self, props: &Map<String, Value>) -> Result<Arc<dyn Any + Send + Sync>, ComponentError>;
});

delegate!(Stateful {
    fn get_state_detail(&self) -> Option<Map<String, Value>>;
});

delegate!(Trigger {
    fn start(&self) -> ();
    fn stop(&self) -> ();
    fn restart(&self) -> ();
    fn add_task(&self, task: Arc<dyn ProcessTask>) -> ();
    fn remove_task(&self, task: Arc<dyn ProcessTask>) -> ();
});

delegate!(Source {
    fn default_pointer(&self) -> Box<dyn SourcePointer>;
    fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer>;
    fn headers(&self, item: &SourceItem) -> Option<HashMap<String, String>>;
    fn group(&self) -> Option<String>;
    custom {
        async fn fetch(&self, pointer: &dyn SourcePointer, limit: u32) -> Result<SourceItemStream, ProcessingError> {
            let stream = self.runtime.scope(async { self.inner.fetch(pointer, limit).await }).await?;
            Ok(Box::pin(self.runtime.clone().stream(stream)))
        }
    }
});

delegate!(ItemFileResolver {
    async fn resolve_files(&self, item: &SourceItem) -> Result<Vec<SourceFile>, ProcessingError>;
});

delegate!(Downloader {
    fn default_download_path(&self) -> &str;
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError>;
    async fn cancel(&self, item: &SourceItem, files: &[SourceFile]) -> Result<(), ProcessingError>;
});

delegate!(AsyncDownloader {
    async fn is_finished(&self, item: &SourceItem) -> Option<bool>;
});

delegate!(FileMover {
    async fn move_file(&self, item: &SourceItem, file: &FileContent) -> Result<(), ProcessingError>;
    async fn exists(&self, paths: &[&PathBuf]) -> Vec<bool>;
    async fn create_directories(&self, path: &Path) -> Result<(), ProcessingError>;
    async fn replace(&self, item: &SourceItem, files: &[&FileContent]) -> Result<(), ProcessingError>;
    async fn list_files(&self, path: &Path) -> Result<Vec<PathBuf>, ProcessingError>;
    async fn path_metadata(&self, path: &Path) -> Result<SourceFile, ProcessingError>;
    async fn is_supported_batch_move(&self) -> bool;
    async fn batch_move(&self, item: &SourceItem, files: &[&FileContent]) -> Result<(), ProcessingError>;
});

delegate!(ProcessListener {
    fn on_item_success(&self, context: &dyn ProcessContext, item: &ItemContent) -> Result<(), ProcessingError>;
    fn on_item_error(&self, context: &dyn ProcessContext, item: &SourceItem, error: &ProcessingError) -> Result<(), ProcessingError>;
    fn on_process_completed(&self, context: &dyn ProcessContext) -> Result<(), ProcessingError>;
});

delegate!(SourceItemFilter {
    async fn filter(&self, item: &SourceItem) -> bool;
});

delegate!(SourceFileFilter {
    fn filter(&self, file: &SourceFile) -> bool;
});

delegate!(ItemContentFilter {
    async fn filter(&self, item: &ItemContent) -> bool;
});

delegate!(FileContentFilter {
    fn filter(&self, file: &FileContent) -> bool;
});

delegate!(FileTagger {
    async fn tag(&self, file: &SourceFile) -> Option<String>;
});

delegate!(FileReplacementDecider {
    fn should_replace(&self, item: &SourceItem, current: &FileContent, before: Option<&InProcessingItem>, existing: &SourceFile) -> bool;
});

#[async_trait]
impl<T: FileExistsDetector + ?Sized> FileExistsDetector for RuntimeBound<T> {
    async fn exists<'a>(
        &self,
        mover: &'a dyn FileMover,
        item: &'a SourceItem,
        files: &'a [FileContent],
    ) -> HashMap<&'a PathBuf, Option<PathBuf>> {
        self.runtime.scope(async { self.inner.exists(mover, item, files).await }).await
    }
}

delegate!(VariableProvider {
    fn accuracy(&self) -> i32;
    fn primary_variable_name(&self) -> Option<String>;
    async fn item_variables(&self, item: &SourceItem) -> Result<HashMap<String, String>, ProcessingError>;
    async fn file_variables(&self, item: &SourceItem, variables: &PatternVariables, files: &[SourceFile]) -> Result<Vec<PatternVariables>, ProcessingError>;
    async fn extract_from(&self, item: &SourceItem, value: &str) -> Result<Option<HashMap<String, Value>>, ProcessingError>;
});

delegate!(VariableReplacer {
    fn replace(&self, key: &str, value: String) -> String;
});

delegate!(Trimmer {
    fn trim(&self, value: String, size: usize) -> String;
});

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{StreamExt, stream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Wake, Waker};
    use std::time::Duration;

    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    #[test]
    fn timer_and_stream_run_without_tokio_executor() {
        let runtime = Arc::new(PluginRuntime::new().unwrap());
        block_on(runtime.scope(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }));
        let weak = Arc::downgrade(&runtime);
        let mut stream = Box::pin(runtime.stream(stream::once(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            42
        })));
        assert!(weak.upgrade().is_some());
        assert_eq!(block_on(stream.next()), Some(42));
        drop(stream);
        assert!(weak.upgrade().is_none());
        assert!(Handle::try_current().is_err());
    }

    #[tokio::test]
    async fn restores_host_context_and_shuts_down_inside_async_context() {
        let host = Handle::current().id();
        let runtime = Arc::new(PluginRuntime::new().unwrap());
        runtime
            .scope(async {
                assert_ne!(Handle::current().id(), host);
                tokio::time::sleep(Duration::from_millis(1)).await;
            })
            .await;
        assert_eq!(Handle::current().id(), host);
        drop(runtime);
        assert_eq!(Handle::current().id(), host);
    }

    #[test]
    fn cancelling_pending_future_drops_resources_and_restores_context() {
        struct OnDrop(Arc<AtomicBool>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let runtime = PluginRuntime::new().unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let probe = OnDrop(dropped.clone());
        let mut future = Box::pin(runtime.scope(async move {
            let _probe = probe;
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }));
        assert!(
            future.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending()
        );
        assert!(Handle::try_current().is_err());
        drop(future);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(Handle::try_current().is_err());
    }

    #[test]
    fn capability_conversions_preserve_shared_state_and_unsupported_errors() {
        #[derive(Debug)]
        struct Probe(AtomicUsize);
        impl Display for Probe {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str("probe")
            }
        }
        impl SdComponent for Probe {
            fn as_source(self: Arc<Self>) -> Result<Arc<dyn Source>, ComponentError> {
                Ok(self)
            }
            fn as_item_file_resolver(
                self: Arc<Self>,
            ) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
                Ok(self)
            }
        }
        #[async_trait]
        impl Source for Probe {
            async fn fetch(
                &self,
                _: &dyn SourcePointer,
                _: u32,
            ) -> Result<SourceItemStream, ProcessingError> {
                tokio::time::sleep(Duration::from_millis(1)).await;
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Box::pin(stream::empty()))
            }
            fn default_pointer(&self) -> Box<dyn SourcePointer> {
                Box::new(EmptyPointer)
            }
            fn parse_raw_pointer(&self, _: Value) -> Box<dyn SourcePointer> {
                Box::new(EmptyPointer)
            }
        }
        #[async_trait]
        impl ItemFileResolver for Probe {
            async fn resolve_files(
                &self,
                _: &SourceItem,
            ) -> Result<Vec<SourceFile>, ProcessingError> {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(vec![SourceFile::new(PathBuf::from(
                    self.0.load(Ordering::SeqCst).to_string(),
                ))])
            }
        }

        let component = Arc::new(RuntimeBound {
            inner: Arc::new(Probe(AtomicUsize::new(0))),
            runtime: Arc::new(PluginRuntime::new().unwrap()),
        });
        let source = component.as_source().unwrap();
        let resolver = source.clone().as_item_file_resolver().unwrap();
        assert!(resolver.clone().as_variable_provider().is_err());
        assert_eq!(resolver.to_string(), "probe");
        let pointer = source.default_pointer();
        let mut stream = block_on(source.fetch(pointer.as_ref(), 1)).unwrap();
        assert!(block_on(stream.next()).is_none());
        let item = SourceItem {
            title: "probe".into(),
            link: http::Uri::from_static("https://example.com"),
            datetime: time::OffsetDateTime::UNIX_EPOCH,
            content_type: "text/html".into(),
            download_uri: http::Uri::from_static("https://example.com"),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        };
        assert_eq!(
            block_on(resolver.resolve_files(&item)).unwrap()[0].path,
            PathBuf::from("1")
        );
    }
}
