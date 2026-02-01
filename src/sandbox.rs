use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use derive_builder::Builder;
use serde_json::Value;
use thiserror::Error;
use tracing::{debug, trace};
use v8;

use crate::tool::Tool;
use crate::ts_interface::ToolInterfaceGenerator;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("v8 error: {0}")]
    V8(String),
    #[error("tool call error: {0}")]
    Tool(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone, Builder)]
#[builder(pattern = "owned")]
pub struct SandboxConfig {
    #[builder(default = "30000")]
    pub timeout_ms: u64,
    #[builder(default = "128")]
    pub max_heap_mb: usize,
    #[builder(setter(custom))]
    pub runtime_handle: tokio::runtime::Handle,
}

impl SandboxConfigBuilder {
    pub fn runtime_handle(mut self, handle: tokio::runtime::Handle) -> Self {
        self.runtime_handle = Some(handle);
        self
    }
}

impl SandboxConfig {
    pub fn new(runtime_handle: tokio::runtime::Handle) -> Self {
        Self {
            timeout_ms: 30000,
            max_heap_mb: 128,
            runtime_handle,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub result: Value,
}

pub struct Sandbox {
    config: SandboxConfig,
}

impl Sandbox {
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    pub fn execute(
        &self,
        code: &str,
        tools: &[&Tool],
        interface_generator: &ToolInterfaceGenerator,
        callers: &HashMap<String, crate::client::ToolCallerEntry>,
    ) -> Result<ExecutionResult, SandboxError> {
        init_v8();
        let mut isolate = v8::Isolate::new(
            v8::CreateParams::default()
                .heap_limits(0, self.config.max_heap_mb * 1024 * 1024),
        );
        let scope = std::pin::pin!(v8::HandleScope::new(&mut isolate));
        let scope = &mut scope.init();
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        let global = context.global(scope);

        let (tx, rx) = mpsc::channel::<Completion>();
        let mut state = SandboxState::new(tx);
        let shared_ptr = state.shared_ptr();

        let interfaces = tools
            .iter()
            .map(|tool| interface_generator.tool_to_typescript_interface(tool))
            .collect::<Vec<String>>()
            .join("\n\n");
        debug!(interfaces = %interfaces, "sandbox tool interfaces");

        inject_tools(
            scope,
            global,
            tools,
            interface_generator,
            callers,
            self.config.runtime_handle.clone(),
            shared_ptr,
            &mut state,
        )?;

        let wrapped = format!("(async function() {{ {} }})()", code);
        let result = run_script(scope, &wrapped)?;
        let result = resolve_value(scope, result, rx, shared_ptr, self.config.timeout_ms)?;
        let result = v8_value_to_json(scope, result)?;

        trace!(result = %format_value(&result), "sandbox execute done");
        Ok(ExecutionResult { result })
    }
}

#[allow(clippy::too_many_arguments)]
fn inject_tools<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    global: v8::Local<'a, v8::Object>,
    tools: &[&Tool],
    interface_generator: &ToolInterfaceGenerator,
    callers: &HashMap<String, crate::client::ToolCallerEntry>,
    runtime_handle: tokio::runtime::Handle,
    shared_state: *const AsyncSharedState,
    state: &mut SandboxState,
) -> Result<(), SandboxError> {
    for tool in tools {
        let access_path = interface_generator.tool_access_path(tool);
        let parts = access_path.split('.').collect::<Vec<&str>>();
        if parts.is_empty() {
            continue;
        }

        let mut target = global;
        for part in &parts[..parts.len() - 1] {
            target = ensure_namespace(scope, target, part)?;
        }

        let caller_entry = callers.get(&tool.name);
        let (async_caller, sync_caller) = match caller_entry {
            Some(crate::client::ToolCallerEntry {
                caller: crate::client::CallerKind::Async(caller),
                ..
            }) => (Some(caller.clone()), None),
            Some(crate::client::ToolCallerEntry {
                caller: crate::client::CallerKind::Sync(caller),
                ..
            }) => (None, Some(caller.clone())),
            None => (None, None),
        };
        let raw_name = caller_entry
            .map(|entry| entry.raw_name.clone())
            .unwrap_or_else(|| tool.name.clone());
        let tool_state = Box::new(ToolCallbackState {
            tool_name: tool.name.clone(),
            raw_name,
            async_caller,
            sync_caller,
            runtime: runtime_handle.clone(),
            shared: shared_state,
            is_async: tool.is_async,
        });
        let tool_external = v8::External::new(scope, &*tool_state as *const _ as *mut c_void);
        let tool_fn = v8::Function::builder(tool_callback)
            .data(tool_external.into())
            .build(scope)
            .ok_or_else(|| SandboxError::V8("tool function".to_string()))?;
        let fn_key = v8::String::new(scope, parts[parts.len() - 1])
            .ok_or_else(|| SandboxError::V8("tool name string".to_string()))?;
        target.set(scope, fn_key.into(), tool_fn.into());
        state.tool_states.push(tool_state);
    }

    Ok(())
}

struct ToolCallbackState {
    tool_name: String,
    raw_name: String,
    async_caller: Option<std::sync::Arc<dyn crate::tool::AsyncToolCaller>>,
    sync_caller: Option<std::sync::Arc<dyn crate::tool::SyncToolCaller>>,
    runtime: tokio::runtime::Handle,
    shared: *const AsyncSharedState,
    is_async: bool,
}

#[allow(clippy::vec_box)]
struct SandboxState {
    // Box is required here for stable heap addresses - V8 callbacks hold pointers to these
    tool_states: Vec<Box<ToolCallbackState>>,
    shared: Box<AsyncSharedState>,
}

impl SandboxState {
    fn new(sender: mpsc::Sender<Completion>) -> Self {
        Self {
            tool_states: Vec::new(),
            shared: Box::new(AsyncSharedState::new(sender)),
        }
    }

    fn shared_ptr(&self) -> *const AsyncSharedState {
        // SAFETY: This pointer is valid as long as SandboxState is alive.
        // The pointer is only used during sandbox execution and never stored beyond that.
        &*self.shared as *const AsyncSharedState
    }
}

struct AsyncSharedState {
    next_id: AtomicU64,
    pending: Cell<usize>,
    resolvers: RefCell<HashMap<u64, v8::Global<v8::PromiseResolver>>>,
    sender: mpsc::Sender<Completion>,
}

impl AsyncSharedState {
    fn new(sender: mpsc::Sender<Completion>) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            pending: Cell::new(0),
            resolvers: RefCell::new(HashMap::new()),
            sender,
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

struct Completion {
    id: u64,
    result: Result<Value, String>,
}

fn resolve_value<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    value: v8::Local<'a, v8::Value>,
    rx: mpsc::Receiver<Completion>,
    shared: *const AsyncSharedState,
    timeout_ms: u64,
) -> Result<v8::Local<'a, v8::Value>, SandboxError> {
    if !value.is_promise() {
        return Ok(value);
    }

    let promise = v8::Local::<v8::Promise>::try_from(value)
        .map_err(|_| SandboxError::V8("promise cast".to_string()))?;
    let start = Instant::now();

    loop {
        drain_completions(scope, &rx, shared)?;
        scope.perform_microtask_checkpoint();

        if promise.state() != v8::PromiseState::Pending {
            if promise.state() == v8::PromiseState::Rejected {
                let message = promise
                    .result(scope)
                    .to_string(scope)
                    .map(|val| val.to_rust_string_lossy(scope))
                    .unwrap_or_else(|| "promise rejected".to_string());
                return Err(SandboxError::Tool(message));
            }
            return Ok(promise.result(scope));
        }

        if start.elapsed() > Duration::from_millis(timeout_ms) {
            return Err(SandboxError::V8("execution timeout".to_string()));
        }

        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(completion) => {
                apply_completion(scope, shared, completion)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Err(SandboxError::V8("execution incomplete".to_string()))
}

fn drain_completions(
    scope: &mut v8::PinScope<'_, '_>,
    rx: &mpsc::Receiver<Completion>,
    shared: *const AsyncSharedState,
) -> Result<(), SandboxError> {
    loop {
        match rx.try_recv() {
            Ok(completion) => apply_completion(scope, shared, completion)?,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

fn apply_completion(
    scope: &mut v8::PinScope<'_, '_>,
    shared: *const AsyncSharedState,
    completion: Completion,
) -> Result<(), SandboxError> {
    // SAFETY: The shared pointer is valid as long as SandboxState is alive.
    // This function is only called during sandbox execution while the state exists.
    let shared = unsafe { &*shared };
    let Some(resolver) = shared.resolvers.borrow_mut().remove(&completion.id) else {
        return Ok(());
    };
    shared.pending.set(shared.pending.get().saturating_sub(1));
    let resolver = v8::Local::new(scope, &resolver);

    match completion.result {
        Ok(value) => {
            if let Some(value) = json_to_v8(scope, &value) {
                resolver.resolve(scope, value);
            } else {
                let message = v8::String::new(scope, "failed to serialize tool result")
                    .ok_or_else(|| SandboxError::V8("error string".to_string()))?;
                let exception = v8::Exception::error(scope, message);
                resolver.reject(scope, exception);
            }
        }
        Err(message) => {
            let message = v8::String::new(scope, &message)
                .ok_or_else(|| SandboxError::V8("error string".to_string()))?;
            let exception = v8::Exception::error(scope, message);
            resolver.reject(scope, exception);
        }
    }

    Ok(())
}

fn init_v8() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

fn run_script<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    source: &str,
) -> Result<v8::Local<'a, v8::Value>, SandboxError> {
    let code = v8::String::new(scope, source)
        .ok_or_else(|| SandboxError::V8("script source".to_string()))?;
    let script = v8::Script::compile(scope, code, None)
        .ok_or_else(|| SandboxError::V8("script compile".to_string()))?;
    script
        .run(scope)
        .ok_or_else(|| SandboxError::V8("script run".to_string()))
}

fn tool_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let external = match v8::Local::<v8::External>::try_from(args.data()) {
        Ok(external) => external,
        Err(_) => return,
    };
    let state_ptr = external.value() as *const ToolCallbackState;
    if state_ptr.is_null() {
        return;
    }
    // SAFETY: The state pointer points to a Box<ToolCallbackState> stored in SandboxState.tool_states.
    // It remains valid for the entire duration of sandbox execution.
    let state = unsafe { &*state_ptr };
    let args_value = args.get(0);
    let args_json = v8::json::stringify(scope, args_value)
        .map(|val| val.to_rust_string_lossy(scope))
        .unwrap_or_else(|| "{}".to_string());
    let parsed_args: Value = serde_json::from_str(&args_json).unwrap_or(Value::Null);
    trace!(tool = state.tool_name.as_str(), args = %format_value(&parsed_args), "sandbox call_tool");

    if state.is_async {
        // SAFETY: state.shared points to AsyncSharedState which is valid as long as SandboxState is alive.
        let shared = unsafe { &*state.shared };
        let resolver = match v8::PromiseResolver::new(scope) {
            Some(resolver) => resolver,
            None => {
                throw_error(scope, "failed to create promise resolver");
                return;
            }
        };
        let promise = resolver.get_promise(scope);
        let id = shared.next_id();
        shared
            .resolvers
            .borrow_mut()
            .insert(id, v8::Global::new(scope, resolver));
        shared.pending.set(shared.pending.get() + 1);

        let sender = shared.sender.clone();
        let tool_name = state.raw_name.clone();
        let caller = match state.async_caller.clone() {
            Some(caller) => caller,
            None => {
                throw_error(scope, "async caller missing");
                return;
            }
        };
        state.runtime.spawn(async move {
            let result = caller.call_tool_async(&tool_name, parsed_args).await;
            let completion = Completion {
                id,
                result: result.map_err(|err| err.to_string()),
            };
            let _ = sender.send(completion);
        });

        rv.set(promise.into());
    } else {
        let sync = match &state.sync_caller {
            Some(sync) => sync,
            None => {
                throw_error(scope, "sync caller missing");
                return;
            }
        };
        let result = sync.call_tool_sync(&state.raw_name, parsed_args);
        match result {
            Ok(value) => {
                if let Some(value) = json_to_v8(scope, &value) {
                    rv.set(value);
                } else {
                    throw_error(scope, "failed to serialize tool result");
                }
            }
            Err(err) => {
                throw_error(scope, &err.to_string());
            }
        }
    }
}

fn ensure_namespace<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    parent: v8::Local<'a, v8::Object>,
    name: &str,
) -> Result<v8::Local<'a, v8::Object>, SandboxError> {
    let key = v8::String::new(scope, name)
        .ok_or_else(|| SandboxError::V8(format!("namespace key '{name}'")))?;
    if let Some(existing) = parent.get(scope, key.into())
        && existing.is_object()
    {
        return existing
            .to_object(scope)
            .ok_or_else(|| SandboxError::V8("namespace object".to_string()));
    }
    let obj = v8::Object::new(scope);
    parent.set(scope, key.into(), obj.into());
    Ok(obj)
}

fn v8_value_to_json(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<v8::Value>,
) -> Result<Value, SandboxError> {
    if value.is_undefined() || value.is_null() {
        return Ok(Value::Null);
    }

    let json = v8::json::stringify(scope, value)
        .map(|val| val.to_rust_string_lossy(scope))
        .ok_or_else(|| SandboxError::Serialization("result stringify".to_string()))?;
    serde_json::from_str(&json).map_err(|err| SandboxError::Serialization(err.to_string()))
}

fn format_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

fn json_to_v8<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    value: &Value,
) -> Option<v8::Local<'a, v8::Value>> {
    let json = serde_json::to_string(value).ok()?;
    let json = v8::String::new(scope, &json)?;
    v8::json::parse(scope, json)
}

fn throw_error(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    if let Some(message) = v8::String::new(scope, message) {
        let exception = v8::Exception::error(scope, message);
        scope.throw_exception(exception);
    }
}
