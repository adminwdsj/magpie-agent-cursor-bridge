//! Headless Cursor protocol bridge. No desktop/account writes or management API.
use std::{path::PathBuf, sync::Arc, time::Duration};
use cursor_server::{api, config, cursor::{prompting::{PromptAssets, PromptCompiler}, transport::TransportRegistry}, local_app::CursorHarness, model::ModelConfigInput, network::NetworkClients, plugin::{PluginRegistry, PluginRuntime}, provider::ProviderRouter, search::WebCache, store::Store};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct Connection { gateway: String, key: String, model: String, models: Vec<BridgeModel> }
#[derive(Deserialize)]
struct BridgeModel { id: String, #[serde(default)] name: String, context: Option<u64>, #[serde(default, rename = "maxOutput")] max_output: Option<u64> }

fn catalogue(bytes: &[u8]) -> Result<Vec<ModelConfigInput>, Box<dyn std::error::Error>> {
    let c: Connection = serde_json::from_slice(bytes)?;
    let url = url::Url::parse(&c.gateway)?;
    if url.scheme() != "http" || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("only a loopback Magpie gateway is supported".into());
    }
    if c.key.trim().is_empty() || !c.models.iter().any(|m| m.id == c.model) { return Err("missing gateway key or selected model".into()); }
    let mut models = c.models;
    models.sort_by_key(|m| m.id != c.model);
    let mut seen = std::collections::HashSet::new();
    models.into_iter().enumerate().map(|(i,m)| {
        if !seen.insert(m.id.clone()) { return Err("duplicate model id".into()); }
        Ok(serde_json::from_value(json!({
            "sort_order": i, "display_name": if m.name.is_empty() {m.id.clone()} else {m.name.clone()},
            "group_name":"Magpie", "type":"openai", "base_url": c.gateway.trim_end_matches('/'),
            "api_key":c.key, "tooltip_data":format!("Magpie / {}",m.id), "model_id":m.id,
            "context_window_tokens":m.context.filter(|v| *v > 0),
            "max_completion_tokens": m.max_output.map(|v| v.min(64000)).filter(|v| *v > 0),
            "custom_headers_enabled":true, "custom_headers":{"User-Agent":"cursor-magpie-bridge/0.2.1"}
        }))?)
    }).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argument = std::env::args().nth(1);
    if argument.as_deref() == Some("--version") {
        println!("0.2.1");
        return Ok(());
    }
    if argument.as_deref() == Some("--help") || argument.is_none() {
        println!("cursor-magpie-bridge 0.2.1\nUsage: cursor-magpie-bridge --init-ca | CONNECTION.json\nRequires CURSOR_BYOK_DATA_DIR (absolute path). --init-ca never modifies certificate trust.");
        return Ok(());
    }
    // A dedicated directory is mandatory: never open the desktop BYOK database.
    if std::env::var_os("CURSOR_BYOK_DATA_DIR").is_none() { return Err("CURSOR_BYOK_DATA_DIR is required".into()); }
    if argument.as_deref() == Some("--init-ca") {
        let state = cursor_server::local_app::initialize_bridge_ca()?;
        println!("{}", json!({"ca":state, "certificate":config::managed_data_dir()?.join("ca/ca.crt")}));
        return Ok(());
    }
    let path = PathBuf::from(argument.unwrap());
    let mut contents = tokio::fs::read(&path).await?;
    let inputs = catalogue(&contents)?;
    let root = config::managed_data_dir()?;
    let store = Store::connect(&format!("sqlite://{}",root.join("bridge.db").display())).await?;
    store.replace_bridge_models(&inputs).await?;
    let plugins = PluginRegistry::managed(store.clone(), PluginRuntime::managed()?, "1.0.1".into())?;
    let clients = NetworkClients::new(store.clone());
    let provider = Arc::new(ProviderRouter::new(store.clone(),plugins.clone(),clients.clone(),Duration::from_secs(3600),Duration::from_secs(1800)));
    let registry = TransportRegistry::with_plugins(store.clone(),provider,PromptCompiler::new(PromptAssets::embedded()?),WebCache::managed()?,plugins,root.join("rules"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    registry.web_cache().set_service_addr(address);
    let router = api::router(registry.clone(),clients)?
        .layer(axum::Extension(cursor_server::cursor::services::model_catalog::LocalCatalogOnly));
    let harness = CursorHarness::new(store.clone())?;
    harness.set_backend_addr(address);
    let proxy = harness.start_proxy_only().await?;
    let shutdown = CancellationToken::new();
    let cancel = shutdown.clone();
    let server = tokio::spawn(async move { axum::serve(listener,router).with_graceful_shutdown(async move {cancel.cancelled().await}).await });
    println!("{}",json!({"event":"ready","proxy":proxy,"backend":format!("http://{address}"),"models":inputs.len()}));
    // The supervisor owns stdin. Its death must not leave an orphan TLS proxy.
    let parent_closed = CancellationToken::new();
    let parent_signal = parent_closed.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 64];
        while matches!(std::io::stdin().read(&mut buf), Ok(n) if n > 0) {}
        parent_signal.cancel();
    });
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
            _ = parent_closed.cancelled() => break,
            _ = interval.tick() => {
                if server.is_finished() { break; }
                match tokio::fs::read(&path).await {
                    Ok(next) if next != contents => match catalogue(&next) {
                        Ok(inputs) => {
                            if store.replace_bridge_models(&inputs).await.is_ok() {
                                contents = next;
                                println!("{}",json!({"event":"catalogue","models":inputs.len()}));
                            } else { eprintln!("catalogue update failed; retaining last complete catalogue"); }
                        }
                        Err(_) => eprintln!("invalid catalogue; retaining last complete catalogue"),
                    },
                    _ => {},
                }
            }
        }
    }
    harness.stop_proxy_only().await;
    registry.shutdown().await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(10),server).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> serde_json::Value { json!({"gateway":"http://127.0.0.1:3425/v1","key":"test-only","model":"p/b","models":[{"id":"p/a","name":"A","context":200000,"maxOutput":128000},{"id":"p/b","name":"B","maxOutput":8000}]}) }
    #[test]
    fn selected_model_first_and_route_preserved() {
        let models=catalogue(&serde_json::to_vec(&config()).unwrap()).unwrap();
        assert_eq!(models[0].model_id,"p/b");
        assert_eq!(models[0].display_name,"B");
        assert_eq!(models[1].context_window_tokens,Some(200000));
        assert_eq!(models[0].base_url,"http://127.0.0.1:3425/v1");
        assert_eq!(models[0].api_key,"test-only");
        // Output cap passes through; a model above the ceiling is clamped to 64000.
        assert_eq!(models[0].max_completion_tokens,Some(8000));
        assert_eq!(models[1].max_completion_tokens,Some(64000));
    }
    #[test]
    fn rejects_remote_and_duplicate_models() {
        let mut c=config();c["gateway"]=json!("http://evil.example/v1");assert!(catalogue(&serde_json::to_vec(&c).unwrap()).is_err());
        let mut c=config();c["models"]=json!([{"id":"p/b"},{"id":"p/b"}]);assert!(catalogue(&serde_json::to_vec(&c).unwrap()).is_err());
    }
    #[tokio::test]
    async fn catalogue_update_is_atomic_and_retains_last_valid_models() {
        let dir=tempfile::tempdir().unwrap();
        let store=Store::connect(&format!("sqlite://{}",dir.path().join("test.db").display())).await.unwrap();
        let inputs=catalogue(&serde_json::to_vec(&config()).unwrap()).unwrap();
        store.replace_bridge_models(&inputs).await.unwrap();
        let duplicate=vec![inputs[0].clone(),inputs[0].clone()];
        assert!(store.replace_bridge_models(&duplicate).await.is_err());
        assert_eq!(store.models().await.unwrap().len(),2);
        let mut invalid=inputs.clone();invalid[1].api_key="".into();
        assert!(store.replace_bridge_models(&invalid).await.is_err());
        assert_eq!(store.models().await.unwrap().len(),2);
        store.replace_bridge_models(&inputs[..1]).await.unwrap();
        assert_eq!(store.models().await.unwrap().len(),1);
    }
}
