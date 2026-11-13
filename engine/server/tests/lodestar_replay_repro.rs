//! Regression coverage for reused user IDs with a newer wire history.
//! Temporary SQLite + FakeProvider only; read results are simulated, never executed.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{sync::Arc, time::Duration};

use cursor_server::{
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{connect, proto::agent::v1 as pb},
        TransportCommand, TransportRegistry,
    },
    model::{ConversationId, ModelConfigInput},
    provider::{FinishReason, ModelEvent},
    store::Store,
};
use prost::Message;

#[derive(Default)]
struct Output {
    checkpoints: Vec<pb::ConversationStateStructure>,
    end: String,
    reads: usize,
    turn_ended: usize,
}

async fn collect(registry: &TransportRegistry, id: &str, message: pb::AgentClientMessage) -> Output {
    let handle = registry.get_or_create(id).await.unwrap();
    let mut receiver = handle.subscribe();
    handle
        .command(TransportCommand::Append { seqno: 0, message: Box::new(message) })
        .await.unwrap();
    let mut seqno = 1;
    let mut output = Output::default();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await.unwrap().unwrap();
        for (flags, payload) in connect::decode_frames(&frame).unwrap() {
            if flags & connect::END_STREAM_FLAG != 0 {
                output.end = String::from_utf8_lossy(&payload).into_owned();
                return output;
            }
            let server = pb::AgentServerMessage::decode(payload).unwrap();
            let reply = match server.message {
                Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                    assert!(matches!(kv.message, Some(pb::kv_server_message::Message::SetBlobArgs(_))));
                    Some(pb::agent_client_message::Message::KvClientMessage(pb::KvClientMessage {
                        id: kv.id,
                        message: Some(pb::kv_client_message::Message::SetBlobResult(pb::SetBlobResult { error: None })),
                    }))
                }
                Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                    assert!(matches!(exec.message, Some(pb::exec_server_message::Message::ReadArgs(_))));
                    output.reads += 1;
                    Some(pb::agent_client_message::Message::ExecClientMessage(pb::ExecClientMessage {
                        id: exec.id,
                        message: Some(pb::exec_client_message::Message::ReadResult(pb::ReadResult {
                            result: Some(pb::read_result::Result::Success(pb::ReadSuccess {
                                path: "/tmp/not-executed".into(),
                                output: Some(pb::read_success::Output::Content("simulated-result".into())),
                                ..Default::default()
                            })),
                        })),
                        ..Default::default()
                    }))
                }
                Some(pb::agent_server_message::Message::ConversationCheckpointUpdate(state)) => {
                    output.checkpoints.push(state);
                    None
                }
                Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                    if matches!(update.message, Some(pb::interaction_update::Message::TurnEnded(_))) {
                        output.turn_ended += 1;
                    }
                    None
                }
                _ => None,
            };
            if let Some(message) = reply {
                handle.command(TransportCommand::Append {
                    seqno,
                    message: Box::new(pb::AgentClientMessage { message: Some(message) }),
                }).await.unwrap();
                seqno += 1;
            }
        }
    }
}

fn request(model: &str, text: &str, state: Option<pb::ConversationStateStructure>) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(pb::AgentRunRequest {
            requested_model: Some(pb::RequestedModel { model_id: model.into(), ..Default::default() }),
            conversation_id: Some("offline-lodestar-repro".into()),
            conversation_state: state,
            action: Some(pb::ConversationAction {
                action: Some(pb::conversation_action::Action::UserMessageAction(pb::UserMessageAction {
                    user_message: Some(pb::UserMessage {
                        text: text.into(), message_id: "same-user-id".into(),
                        mode: pb::AgentMode::Agent as i32, ..Default::default()
                    }),
                    request_context: Some(pb::RequestContext::default()),
                    ..Default::default()
                })),
                ..Default::default()
            }),
            ..Default::default()
        })),
    }
}

fn answer(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Start { model_call_id: text.into() },
        ModelEvent::TextStart,
        ModelEvent::TextDelta(text.into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]
}

async fn setup() -> (tempfile::TempDir, Store, TransportRegistry, fake_provider::FakeProvider, String) {
    let (directory, store) = fixtures::temp_store().await;
    let config: ModelConfigInput = serde_json::from_value(serde_json::json!({
        "display_name": "Offline test", "type": "openai", "base_url": "https://example.invalid/v1",
        "api_key": "test-only", "tooltip_data": "Offline test", "model_id": "test-model"
    })).unwrap();
    let model = store.create_model(&config).await.unwrap();
    let provider = fake_provider::FakeProvider::default();
    let assets = PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor")).unwrap();
    let registry = TransportRegistry::new(store.clone(), Arc::new(provider.clone()), PromptCompiler::new(assets));
    (directory, store, registry, provider, model.model_hash)
}

fn assert_success(output: &Output) {
    assert_eq!(serde_json::from_str::<serde_json::Value>(&output.end).unwrap(), serde_json::json!({}), "{}", output.end);
    assert_eq!(output.turn_ended, 1);
}

#[tokio::test]
async fn changed_text_with_reused_id_keeps_current_history_and_commits_tools() {
    let (_directory, store, registry, provider, model) = setup().await;
    provider.push(answer("earlier work completed"));
    provider.push(vec![
        ModelEvent::Start { model_call_id: "tool-answer".into() },
        ModelEvent::ToolCallStart { index: 0, call_id: "read-1".into(), name: "Read".into() },
        ModelEvent::ToolCallArgumentsDelta { index: 0, delta: "{\"path\":\"/tmp/not-executed\"}".into() },
        ModelEvent::ToolCallEnd { index: 0 },
        ModelEvent::Done(FinishReason::ToolUse),
    ]);
    provider.push(answer("continued from completed work"));
    let first = collect(&registry, "first", request(&model, "old request", None)).await;
    assert_success(&first);
    let state = first.checkpoints.last().unwrap().clone();
    let second = collect(&registry, "second", request(&model, "old request\nnew context", Some(state.clone()))).await;
    assert_success(&second);
    assert_eq!(second.reads, 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(serde_json::to_string(&requests[1].history).unwrap().contains("earlier work completed"));
    assert!(serde_json::to_string(&requests[2].history).unwrap().contains("simulated-result"));
    for checkpoint in &second.checkpoints {
        assert!(checkpoint.root_prompt_messages_json.starts_with(&state.root_prompt_messages_json));
    }
    let committed: i64 = sqlx::query_scalar("SELECT count(*) FROM tool_round_calls WHERE status='completed' AND committed_checkpoint_id IS NOT NULL")
        .fetch_one(store.pool()).await.unwrap();
    assert_eq!(committed, 1);
    println!("更新内容续聊：完整保留旧进度，模拟工具只执行一次并成功入账，终态正常");
    registry.shutdown().await;
}

#[tokio::test]
async fn unchanged_input_with_latest_history_does_not_duplicate_the_user() {
    let (_directory, store, registry, provider, model) = setup().await;
    provider.push(answer("first answer"));
    provider.push(answer("next answer"));
    let first = collect(&registry, "same-first", request(&model, "same request", None)).await;
    assert_success(&first);
    let second = collect(&registry, "same-next", request(&model, "same request", first.checkpoints.last().cloned())).await;
    assert_success(&second);
    let stored = store.load_current_messages(&ConversationId::new("offline-lodestar-repro")).await.unwrap();
    assert_eq!(stored.iter().filter(|m| m.runtime_event_id.is_some()).count(), 1);
    assert!(serde_json::to_string(&provider.requests()[1].history).unwrap().contains("first answer"));
    println!("同内容续聊：保留最新回答，不重复插入用户消息");
    registry.shutdown().await;
}

#[tokio::test]
async fn retry_without_wire_history_keeps_the_original_input_anchor() {
    let (_directory, store, registry, provider, model) = setup().await;
    provider.push(answer("first answer"));
    provider.push(answer("retry answer"));
    let first = collect(&registry, "stateless-first", request(&model, "same request", None)).await;
    assert_success(&first);
    let second = collect(&registry, "stateless-retry", request(&model, "same request", None)).await;
    assert_success(&second);
    let requests = provider.requests();
    assert_eq!(requests[0], requests[1], "no-history retries retain the original idempotent input");
    let stored = store.load_current_messages(&ConversationId::new("offline-lodestar-repro")).await.unwrap();
    assert_eq!(stored.iter().filter(|m| m.runtime_event_id.is_some()).count(), 1);
    println!("无历史重试：原去重行为保持不变");
    registry.shutdown().await;
}
