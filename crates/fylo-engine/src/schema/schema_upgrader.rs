use std::path::Path;
use std::rc::Rc;

use boa_engine::module::{Module, SimpleModuleLoader};
use boa_engine::object::JsObject;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsValue, Source, js_string};
use serde_json::{Map, Value};

use super::schema_paths::SchemaPaths;
use super::utils::schema_files::SchemaFiles;
use super::utils::upgrader_source::UpgraderSource;

pub(super) struct SchemaUpgrader<'a> {
    paths: &'a SchemaPaths<'a>,
    from: &'a str,
    to: &'a str,
}

impl<'a> SchemaUpgrader<'a> {
    const LOOP_LIMIT: u64 = 10_000_000;

    pub(super) fn new(paths: &'a SchemaPaths<'a>, from: &'a str, to: &'a str) -> Self {
        Self { paths, from, to }
    }

    pub(super) fn run(&self, document: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        let path = self.paths.upgrader(self.from, self.to);
        let source = self.source(&path)?;
        let loader = Self::loader(self.paths.root)?;
        let mut context = Self::context(&loader)?;
        let module = Self::module(&path, &source, &loader, &mut context)?;
        let returned = self.call(&module, document, &mut context)?;
        let returned = self.resolve(returned, &mut context)?;
        self.document(&returned, &mut context)
    }

    fn source(&self, path: &Path) -> Result<String, String> {
        let bytes = SchemaFiles::read(path).map_err(|_| {
            format!(
                "Missing upgrader {}->{} for collection '{}' at {}",
                self.from,
                self.to,
                self.paths.collection,
                path.display()
            )
        })?;
        let source = String::from_utf8(bytes)
            .map_err(|_| format!("upgrader {} is not valid UTF-8", path.display()))?;
        Ok(UpgraderSource::normalize(&source))
    }

    fn loader(root: &Path) -> Result<Rc<SimpleModuleLoader>, String> {
        SimpleModuleLoader::new(root)
            .map(Rc::new)
            .map_err(|error| format!("cannot initialize schema module loader: {error}"))
    }

    fn context(loader: &Rc<SimpleModuleLoader>) -> Result<Context, String> {
        let mut context = Context::builder()
            .module_loader(loader.clone())
            .build()
            .map_err(|error| format!("cannot initialize schema upgrader runtime: {error}"))?;
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(Self::LOOP_LIMIT);
        Ok(context)
    }

    fn module(
        path: &Path,
        source: &str,
        loader: &SimpleModuleLoader,
        context: &mut Context,
    ) -> Result<Module, String> {
        let source = Source::from_reader(source.as_bytes(), Some(path));
        let module = Module::parse(source, None, context)
            .map_err(|error| format!("cannot parse upgrader {}: {error}", path.display()))?;
        loader.insert(path.to_owned(), module.clone());
        module
            .load_link_evaluate(context)
            .await_blocking(context)
            .map_err(|error| format!("cannot evaluate upgrader {}: {error}", path.display()))?;
        Ok(module)
    }

    fn call(
        &self,
        module: &Module,
        document: &Map<String, Value>,
        context: &mut Context,
    ) -> Result<JsValue, String> {
        let callable = module
            .get_value(js_string!("default"), context)
            .map_err(|error| format!("cannot load upgrader default export: {error}"))?;
        let function = callable.as_function().ok_or_else(|| {
            format!(
                "Upgrader at {} must default-export an async (doc) => doc function",
                self.paths.upgrader(self.from, self.to).display()
            )
        })?;
        let argument = JsValue::from_json(&Value::Object(document.clone()), context)
            .map_err(|error| format!("cannot encode upgrader document: {error}"))?;
        function
            .call(&JsValue::undefined(), &[argument], context)
            .map_err(|error| self.failed(error))
    }

    fn resolve(&self, returned: JsValue, context: &mut Context) -> Result<JsValue, String> {
        let promise = returned.as_object().and_then(Self::promise);
        self.await_result(promise, returned, context)
    }

    fn promise(object: JsObject) -> Option<JsPromise> {
        JsPromise::from_object(object).ok()
    }

    fn await_result(
        &self,
        promise: Option<JsPromise>,
        returned: JsValue,
        context: &mut Context,
    ) -> Result<JsValue, String> {
        match promise {
            Some(promise) => promise
                .await_blocking(context)
                .map_err(|error| self.failed(error)),
            None => Ok(returned),
        }
    }

    fn failed(&self, error: impl std::fmt::Display) -> String {
        format!("upgrader {}->{} failed: {error}", self.from, self.to)
    }

    fn document(
        &self,
        returned: &JsValue,
        context: &mut Context,
    ) -> Result<Map<String, Value>, String> {
        returned
            .to_json(context)
            .map_err(|error| {
                format!(
                    "upgrader {}->{} returned invalid JSON: {error}",
                    self.from, self.to
                )
            })?
            .as_ref()
            .and_then(Self::object)
            .ok_or_else(|| {
                format!(
                    "Upgrader {}->{} for '{}' must return an object",
                    self.from, self.to, self.paths.collection
                )
            })
    }

    fn object(value: &Value) -> Option<Map<String, Value>> {
        value.as_object().cloned()
    }
}
