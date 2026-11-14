//! Implements Cursor analytics endpoints and event handling.
use axum::{
    body::{Body, Bytes},
    extract::Extension,
    http::{header, HeaderValue, Request, Response, StatusCode},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::{BufMut, BytesMut};
use prost::Message;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{api::cursor::proxy, Error, Result};

pub const BOOTSTRAP_STATSIG_PATH: &str = "/aiserver.v1.AnalyticsService/BootstrapStatsig";
const AGENT_RETRIES_GATE: &str = "nal_agent_retries";
const AGENT_WEBSOCKET_GATE: &str = "nal_websocket_client";
const RUN_INFERENCE_WEBSOCKET_CONFIG: &str = "run_inference_websocket_client";
const AGENT_HOST_LOCAL_LOOP_GATE: &str = "agent_host_local_loop";
/// Cursor 只有在这三个开关都打开时，才会把后台子代理的完成通知派回父对话。
/// 登录状态下 Cursor 服务端已经下发为开（2026-11-14 本机实测）；未登录时
/// 服务端开关表被拒，Cursor 退回本地开关表，这里必须显式打开，否则后台子代理
/// 跑完了父对话也不会继续。
const BACKGROUND_TASK_GATES: [&str; 3] = [
    "long_running_jobs",
    "cursor_agent_host",
    "agent_store_sync_client",
];
const LOCAL_RULE: &str = "local_enabled";

#[derive(Clone, PartialEq, Message)]
struct BootstrapStatsigResponse {
    #[prost(string, tag = "1")]
    config: String,
    #[prost(uint64, tag = "2")]
    generated_at_ms: u64,
}

pub async fn bootstrap_statsig(
    Extension(upstream): Extension<proxy::CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    match proxy::forward_buffered(&upstream, request).await {
        Ok(response) if response.status.is_success() => match patch_upstream(response) {
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::warn!(%error, "Cursor Statsig bootstrap was invalid; using local bootstrap");
                local_response()
            }
        },
        Ok(response) => {
            tracing::warn!(status = %response.status, "Cursor Statsig bootstrap was rejected; using local bootstrap");
            local_response()
        }
        Err(error) => {
            tracing::warn!(%error, "Cursor Statsig bootstrap was unavailable; using local bootstrap");
            local_response()
        }
    }
}

fn patch_upstream(response: proxy::BufferedResponse) -> Result<Response<Body>> {
    let (framed, payload) = unary_payload(&response.body)?;
    let mut message = BootstrapStatsigResponse::decode(payload)?;
    let mut config = serde_json::from_str::<Value>(&message.config)?;
    enable_agent_retries(&mut config)?;
    disable_agent_websocket_transports(&mut config)?;
    message.config = serde_json::to_string(&config)?;
    Ok(response.with_body(encode_unary(&message, framed)))
}

fn local_response() -> Result<Response<Body>> {
    let generated_at_ms = chrono::Utc::now().timestamp_millis() as u64;
    let mut config = json!({
        "feature_gates": {},
        "dynamic_configs": {},
        "layer_configs": {},
        "user": {
            "userID": "local_ultra",
            "customIDs": { "localUserID": "local_ultra" }
        },
        "has_updates": true,
        "hash_used": "none",
        "sdkParams": {
            "stableID": "local_ultra",
            "disableDiagnosticsLogging": true
        },
        "time": generated_at_ms
    });
    enable_agent_retries(&mut config)?;
    disable_agent_websocket_transports(&mut config)?;
    for gate in BACKGROUND_TASK_GATES {
        set_gate(&mut config, gate, true)?;
    }
    let message = BootstrapStatsigResponse {
        config: serde_json::to_string(&config)?,
        generated_at_ms,
    };
    let body = message.encode_to_vec();
    let mut response = Response::new(Body::from(body.clone()));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        body.len()
            .to_string()
            .parse()
            .expect("body length is a valid header value"),
    );
    Ok(response)
}

fn enable_agent_retries(config: &mut Value) -> Result<()> {
    set_gate(config, AGENT_RETRIES_GATE, true)
}

fn set_gate(config: &mut Value, name: &str, value: bool) -> Result<()> {
    let gate_key = statsig_key(config, name);
    let root = config
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("Statsig bootstrap config must be an object".into()))?;
    let gates = root
        .entry("feature_gates")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("Statsig feature_gates must be an object".into()))?;
    gates.insert(gate_key.clone(), enabled_gate(&gate_key, value));
    Ok(())
}

/// Cursor 3.29 can carry Agent Run traffic over `wss://api2.cursor.sh/agent/v1/run`
/// (`nal_websocket_client`) and RunInference over its own socket
/// (`run_inference_websocket_client`). Both dial Cursor directly and skip the
/// local proxy that owns the bridge, so a run started that way never reaches the
/// model catalogue and hangs until the client gives up. Turning the two gates
/// off keeps Agent traffic on the HTTP transports the bridge implements.
fn disable_agent_websocket_transports(config: &mut Value) -> Result<()> {
    let gate_key = statsig_key(config, AGENT_WEBSOCKET_GATE);
    let config_key = statsig_key(config, RUN_INFERENCE_WEBSOCKET_CONFIG);
    let local_loop_key = statsig_key(config, AGENT_HOST_LOCAL_LOOP_GATE);
    let root = config
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("Statsig bootstrap config must be an object".into()))?;
    let gates = root
        .entry("feature_gates")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("Statsig feature_gates must be an object".into()))?;
    gates.insert(gate_key.clone(), enabled_gate(&gate_key, false));
    // `agent_host_local_loop` moves the agent loop (Task subagents first) into
    // Cursor and sends only RunInferenceClientMessage over BidiAppend, which the
    // bridge cannot decode as AgentClientMessage. Cursor's own error text says
    // to disable it to get the backend loop back, which the bridge implements.
    gates.insert(local_loop_key.clone(), enabled_gate(&local_loop_key, false));
    // RunInference reads its dynamic config's `enabled` instead of a gate.
    let configs = root
        .entry("dynamic_configs")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| Error::Protocol("Statsig dynamic_configs must be an object".into()))?;
    configs.insert(
        config_key,
        json!({ "enabled": false, "treatment_percent": 0, "maxConnections": 1, "fallbackOnColdPool": false }),
    );
    Ok(())
}

fn statsig_key(config: &Value, name: &str) -> String {
    match config.get("hash_used").and_then(Value::as_str) {
        Some("djb2") => djb2(name),
        Some("sha256") => STANDARD.encode(Sha256::digest(name.as_bytes())),
        _ => name.to_owned(),
    }
}

fn djb2(value: &str) -> String {
    value
        .encode_utf16()
        .fold(0_u32, |hash, character| {
            hash.wrapping_mul(31).wrapping_add(u32::from(character))
        })
        .to_string()
}

fn enabled_gate(name: &str, value: bool) -> Value {
    json!({
        "name": name,
        "value": value,
        "rule_id": LOCAL_RULE,
        "ruleID": LOCAL_RULE,
        "group_name": LOCAL_RULE,
        "groupName": LOCAL_RULE,
        "secondary_exposures": [],
        "secondaryExposures": [],
        "undelegated_secondary_exposures": [],
        "undelegatedSecondaryExposures": [],
        "is_device_based": false,
        "isDeviceBased": false,
        "id_type": "userID",
        "idType": "userID"
    })
}

fn unary_payload(body: &Bytes) -> Result<(bool, &[u8])> {
    if body.len() < 5 {
        return Ok((false, body));
    }
    let flags = body[0];
    let length = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    if length != body.len() - 5 {
        return Ok((false, body));
    }
    if flags != 0 {
        return Err(Error::Protocol(format!(
            "cannot patch compressed or terminal Statsig frame: flags={flags}"
        )));
    }
    Ok((true, &body[5..]))
}

fn encode_unary(message: &impl Message, framed: bool) -> Bytes {
    let payload = message.encode_to_vec();
    if !framed {
        return Bytes::from(payload);
    }
    let mut output = BytesMut::with_capacity(5 + payload.len());
    output.put_u8(0);
    output.put_u32(payload.len() as u32);
    output.extend_from_slice(&payload);
    output.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patched() -> Value {
        let mut config = json!({
            "feature_gates": {},
            "dynamic_configs": {},
            "hash_used": "none"
        });
        enable_agent_retries(&mut config).unwrap();
        disable_agent_websocket_transports(&mut config).unwrap();
        config
    }

    #[test]
    fn disables_every_socket_path_that_bypasses_the_bridge() {
        let config = patched();
        assert_eq!(config["feature_gates"][AGENT_RETRIES_GATE]["value"], json!(true));
        assert_eq!(config["feature_gates"][AGENT_WEBSOCKET_GATE]["value"], json!(false));
        assert_eq!(config["feature_gates"][AGENT_HOST_LOCAL_LOOP_GATE]["value"], json!(false));
        assert_eq!(config["dynamic_configs"][RUN_INFERENCE_WEBSOCKET_CONFIG]["enabled"], json!(false));
    }

    #[test]
    fn overrides_a_server_enabled_local_loop_gate() {
        // Captured from Cursor 3.29.15: the server rolled this gate out at 100%.
        let key = djb2(AGENT_HOST_LOCAL_LOOP_GATE);
        assert_eq!(key, "1967931573");
        let mut config = json!({
            "feature_gates": { key.clone(): { "name": key.clone(), "value": true } },
            "dynamic_configs": {},
            "hash_used": "djb2"
        });
        disable_agent_websocket_transports(&mut config).unwrap();
        assert_eq!(config["feature_gates"][key]["value"], json!(false));
    }

    #[tokio::test]
    async fn local_bootstrap_lets_background_subagents_wake_the_parent() {
        let response = local_response().unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let message = BootstrapStatsigResponse::decode(body.as_ref()).unwrap();
        let config: Value = serde_json::from_str(&message.config).unwrap();
        for gate in BACKGROUND_TASK_GATES {
            assert_eq!(config["feature_gates"][gate]["value"], json!(true), "{gate}");
        }
        assert_eq!(config["feature_gates"][AGENT_HOST_LOCAL_LOOP_GATE]["value"], json!(false));
    }

    #[test]
    fn honours_a_hashed_gate_key_scheme() {
        let mut config = json!({ "feature_gates": {}, "dynamic_configs": {}, "hash_used": "djb2" });
        disable_agent_websocket_transports(&mut config).unwrap();
        assert_eq!(config["feature_gates"][djb2(AGENT_WEBSOCKET_GATE)]["value"], json!(false));
        assert_eq!(config["dynamic_configs"][djb2(RUN_INFERENCE_WEBSOCKET_CONFIG)]["enabled"], json!(false));
    }
}
