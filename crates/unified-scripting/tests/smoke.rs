//! Smoke test: load a TypeScript module, transpile it, run it in
//! QuickJS, call one of its hooks and one of its routes, and assert
//! the results. No database, no server, no network.

use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::path::PathBuf;
use unified_scripting::engine::{RouteRequest, ScriptLimits, ScriptingEngine};
use unified_scripting::loader::{ModuleMeta, load_and_transpile};
use unified_scripting::sdk::SdkHost;

const MODULE_TS: &str = r#"
Unified.routes.get("/hello", (req) => {
  const name = req.query.name ?? "world";
  return { status: 200, body: { message: `Hello, ${name}!` } };
});

Unified.hooks.on("user.created", (event) => {
  Unified.db.kv.set("last-user-created", event.data);
});
"#;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("unified-scripting-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn ts_module_hook_and_route() {
    let base = temp_dir("smoke");
    let source_path = base.join("module.ts");
    std::fs::write(&source_path, MODULE_TS).expect("write module source");

    let host = SdkHost::for_tests(base.join("storage"));
    let engine = ScriptingEngine::new(host.clone(), ScriptLimits::default(), base.join("modules"));

    // Transpile (OXC, in memory) + load into QuickJS.
    let js = load_and_transpile(&source_path).expect("transpile module");
    assert!(js.contains("Unified.routes.get"));

    let meta = ModuleMeta {
        id: "smoke".to_string(),
        name: "Smoke".to_string(),
        version: "0.1.0".to_string(),
        description: "Smoke test module.".to_string(),
        dir: base.clone(),
    };
    let summary = engine.load_module(meta, js).expect("load module");
    assert_eq!(summary.id, "smoke");
    assert_eq!(summary.status, "loaded");
    assert_eq!(summary.routes.len(), 1);
    assert_eq!(summary.routes[0].method, "GET");
    assert_eq!(summary.routes[0].pattern, "/hello");
    assert_eq!(summary.hooks, vec!["user.created".to_string()]);

    // One route call.
    let request = RouteRequest {
        method: "GET".to_string(),
        path: "/hello".to_string(),
        query: HashMap::from([("name".to_string(), "world".to_string())]),
        headers: HashMap::new(),
        body: JsonValue::Null,
    };
    let response = engine
        .dispatch_route("smoke", "GET", "/hello", request, None)
        .expect("dispatch route");
    assert_eq!(response.status, 200);
    assert_eq!(response.body["message"], json!("Hello, world!"));

    // One hook call: the handler stores the event payload in kv.
    let results = engine
        .call_hook("smoke", "user.created", json!({"id": 42}), None)
        .expect("call hook");
    assert_eq!(results.len(), 1);
    let stored = host
        .kv
        .get(None, "smoke", "last-user-created")
        .expect("kv get");
    assert_eq!(stored, Some(json!({"id": 42})));

    // Reload swaps the implementation without a restart.
    let js_v2 = load_and_transpile(&source_path).expect("transpile again");
    let summary = engine.reload_module("smoke", js_v2).expect("reload module");
    assert_eq!(summary.status, "loaded");
    assert_eq!(summary.routes.len(), 1);

    let _ = std::fs::remove_dir_all(&base);
}
