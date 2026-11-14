//! Compiles non-interrupting runtime information into append-only Messages.
use std::collections::BTreeMap;

use crate::{
    cursor::{protocol::proto::agent::v1 as pb, services::blob_sync::BlobSynchronizer},
    model::CanonicalMessage,
    Error, Result,
};

pub(super) const FOLLOW_UP: &str = concat!(
    "Perform any necessary follow-up actions in response to the subagent completion above. ",
    "If no follow-up work is needed, no further action is required. ",
    "If you mention an agent or subagent in your response, link it with the `[Name](id)` ",
    "Don't use generic label such as `[agent]`, `[worker]`, or `[subagent]`. ",
    "For cloud subagents, when the agent has edited code, link to `[Review](bc-id#changes)`, ",
    "or, if you know the exact added and deleted line counts, `[Review +A −D](bc-id#changes)`, ",
    "replacing A and D with those counts. Never write A or D literally. ",
    "Use `[Try Live](bc-id#desktop)` only when the agent used computer use. ",
    "Don't repeat the same confirmation every time."
);

pub(super) const SHELL_FOLLOW_UP: &str = concat!(
    "Briefly inform the user about the task result and perform any follow-up actions (if needed). ",
    "If there's no follow-ups needed, don't explicitly say that."
);

#[derive(Debug)]
pub(super) struct Projection {
    pub context: String,
    pub turn_user: pb::UserMessage,
}

pub(super) fn project(
    action: &pb::BackgroundTaskCompletionAction,
    mode: i32,
) -> Result<Projection> {
    if action.completions.is_empty() {
        return Err(Error::Protocol(
            "background task completion action contains no completion".into(),
        ));
    }

    let mut completions = BTreeMap::new();
    let mut has_shell = false;
    let mut has_subagent = false;
    for completion in &action.completions {
        let kind = pb::BackgroundTaskKind::try_from(completion.kind).map_err(|_| {
            Error::Protocol(format!("unknown background task kind: {}", completion.kind))
        })?;
        if kind == pb::BackgroundTaskKind::Unspecified {
            return Err(Error::Protocol(format!(
                "background task completion has invalid kind: {}",
                kind.as_str_name()
            )));
        }
        let reason =
            pb::BackgroundTaskCompletionReason::try_from(completion.reason).map_err(|_| {
                Error::Protocol(format!(
                    "unknown background task completion reason: {}",
                    completion.reason
                ))
            })?;
        if reason != pb::BackgroundTaskCompletionReason::TaskFinished {
            // Progress and reparenting notifications are informational; the
            // client batches them together with the real finish notification.
            continue;
        }
        if completion.task_id.is_empty() || completion.title.is_empty() {
            return Err(Error::Protocol(
                "background task completion requires task_id and title".into(),
            ));
        }
        let agent_id = match kind {
            pb::BackgroundTaskKind::Shell => {
                has_shell = true;
                None
            }
            pb::BackgroundTaskKind::Subagent => {
                has_subagent = true;
                Some(
                    completion
                        .subagent_id
                        .as_deref()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| {
                            Error::Protocol(
                                "background subagent completion has no subagent_id".into(),
                            )
                        })?,
                )
            }
            pb::BackgroundTaskKind::Unspecified => unreachable!(),
        };
        let tool_call_id = completion
            .tool_call_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                Error::Protocol("background task completion has no tool_call_id".into())
            })?;
        let task_identity = agent_id.unwrap_or(&completion.task_id);
        let identity = format!("{}:{task_identity}:{tool_call_id}", kind.as_str_name());
        let context = completion_context(completion, kind, agent_id)?;
        if completions
            .insert(identity.clone(), (completion, context))
            .is_some()
        {
            return Err(Error::Protocol(format!(
                "duplicate background task completion: {identity}"
            )));
        }
    }

    let (first, _) = completions.values().next().ok_or_else(|| {
        Error::Protocol("background task notification contains no finished task".into())
    })?;
    let text = match (has_shell, has_subagent) {
        (true, false) => SHELL_FOLLOW_UP.into(),
        (false, true) => FOLLOW_UP.into(),
        (true, true) => format!("{SHELL_FOLLOW_UP}\n\n{FOLLOW_UP}"),
        (false, false) => unreachable!(),
    };
    Ok(Projection {
        context: completions
            .values()
            .map(|(_, context)| context.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
        turn_user: pb::UserMessage {
            text,
            message_id: format!(
                "background-completed:{}",
                completions.keys().cloned().collect::<Vec<_>>().join(":")
            ),
            mode,
            is_simulated_msg: Some(true),
            simulated_msg_reason: Some(pb::SimulatedMsgReason::BackgroundTaskCompletion as i32),
            simulated_message_metadata: Some(pb::user_message::SimulatedMessageMetadata {
                title: Some(first.title.clone()),
                task_id: Some(first.task_id.clone()),
                ..Default::default()
            }),
            ..Default::default()
        },
    })
}

pub(super) const ASK_FOLLOW_UP: &str = concat!(
    "The user has responded to the questions above. ",
    "Continue the task using their answers; do not ask the same questions again."
);

/// 异步提问（AskQuestion 的 run_async）答完后，Cursor 发 AsyncAskQuestionCompletionAction
/// 让对话接着跑。把原题和回答放进上下文，模型才知道用户选了什么；以前这个动作没人处理，
/// 模型只看到旧上下文，回复和用户的选择对不上（2026-11-14 实测）。
pub(super) fn project_ask_question(
    action: &pb::AsyncAskQuestionCompletionAction,
    mode: i32,
) -> Result<Projection> {
    use pb::ask_question_result::Result as R;
    let tool_call_id = action.original_tool_call_id.as_str();
    if tool_call_id.is_empty() {
        return Err(Error::Protocol(
            "ask question completion has no original_tool_call_id".into(),
        ));
    }
    let args = action.original_args.clone().unwrap_or_default();
    let body = match action
        .result
        .as_ref()
        .and_then(|result| result.result.as_ref())
        .ok_or_else(|| Error::Protocol("ask question completion has no result".into()))?
    {
        R::Success(success) => {
            let mut lines = vec!["status: answered".to_string()];
            for answer in &success.answers {
                let question = args.questions.iter().find(|q| q.id == answer.question_id);
                let mut picked = answer
                    .selected_option_ids
                    .iter()
                    .map(|id| {
                        question
                            .and_then(|q| q.options.iter().find(|option| &option.id == id))
                            .map_or(id.as_str(), |option| option.label.as_str())
                            .to_string()
                    })
                    .collect::<Vec<_>>();
                if !answer.freeform_text.trim().is_empty() {
                    picked.push(answer.freeform_text.trim().to_string());
                }
                let prompt = question
                    .map(|q| q.prompt.as_str())
                    .filter(|prompt| !prompt.is_empty())
                    .unwrap_or(answer.question_id.as_str());
                let answer = if picked.is_empty() {
                    "(no answer)".to_string()
                } else {
                    picked.join("; ")
                };
                lines.push(format!("question: {prompt}\nanswer: {answer}"));
            }
            lines.join("\n")
        }
        R::Error(error) => format!("status: error\nerror: {}", error.error_message),
        R::Rejected(rejected) => format!("status: rejected\nreason: {}", rejected.reason),
        R::Async(_) => {
            return Err(Error::Protocol(
                "ask question completion is still pending".into(),
            ))
        }
    };
    let title = Some(args.title.as_str()).filter(|title| !title.is_empty());
    let header = title.map_or(String::new(), |title| format!("title: {title}\n"));
    Ok(Projection {
        context: format!(
            "<system_notification>\nThe questions you asked earlier (tool_call_id: {tool_call_id}) now have a response.\n\n<ask_question_result>\n{header}{body}\n</ask_question_result>\n</system_notification>"
        ),
        turn_user: pb::UserMessage {
            text: ASK_FOLLOW_UP.into(),
            message_id: format!("ask-question-completed:{tool_call_id}"),
            mode,
            is_simulated_msg: Some(true),
            simulated_message_metadata: Some(pb::user_message::SimulatedMessageMetadata {
                title: Some(title.unwrap_or("Questions answered").into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    })
}

/// 正在输出的对话收到提问完成通知时，编译成和新 Run 同样的消息。
pub(crate) async fn compile_ask_question_completion(
    action: &pb::AsyncAskQuestionCompletionAction,
    mode: i32,
    request_context: &pb::RequestContext,
    blobs: &BlobSynchronizer,
) -> Result<(String, pb::UserMessage, CanonicalMessage)> {
    compile_projection(project_ask_question(action, mode)?, request_context, blobs).await
}

/// 正在输出的父对话收到后台任务完成通知时，编译成和新 Run 同样的消息：
/// 返回事件 ID、Cursor 显示的模拟用户消息、模型读到的消息。
pub(crate) async fn compile_background_completion(
    action: &pb::BackgroundTaskCompletionAction,
    mode: i32,
    request_context: &pb::RequestContext,
    blobs: &BlobSynchronizer,
) -> Result<(String, pb::UserMessage, CanonicalMessage)> {
    compile_projection(project(action, mode)?, request_context, blobs).await
}

async fn compile_projection(
    mut projection: Projection,
    request_context: &pb::RequestContext,
    blobs: &BlobSynchronizer,
) -> Result<(String, pb::UserMessage, CanonicalMessage)> {
    let event_id = projection.turn_user.message_id.clone();
    let (message, text) = super::break_messages::compile_background(
        event_id.clone(),
        &projection.turn_user,
        request_context,
        &projection.context,
        blobs,
    )
    .await?;
    projection.turn_user.text = text;
    Ok((event_id, projection.turn_user, message))
}

fn status(completion: &pb::BackgroundTaskCompletion) -> Result<pb::BackgroundTaskStatus> {
    let status = pb::BackgroundTaskStatus::try_from(completion.status).map_err(|_| {
        Error::Protocol(format!(
            "unknown background task status: {}",
            completion.status
        ))
    })?;
    if status == pb::BackgroundTaskStatus::Unspecified {
        return Err(Error::Protocol(
            "background task completion has unspecified status".into(),
        ));
    }
    Ok(status)
}

fn completion_context(
    completion: &pb::BackgroundTaskCompletion,
    kind: pb::BackgroundTaskKind,
    agent_id: Option<&str>,
) -> Result<String> {
    let status = status(completion)?;
    let mut fields = vec![
        format!(
            "kind: {}",
            match kind {
                pb::BackgroundTaskKind::Shell => "shell",
                pb::BackgroundTaskKind::Subagent => "subagent",
                pb::BackgroundTaskKind::Unspecified => unreachable!(),
            }
        ),
        format!("status: {}", status_name(status)),
        format!("task_id: {}", completion.task_id),
        format!("title: {}", completion.title),
    ];
    optional_field(
        &mut fields,
        "tool_call_id",
        completion.tool_call_id.as_deref(),
    );
    optional_field(&mut fields, "agent_id", agent_id);
    optional_field(&mut fields, "detail", completion.detail.as_deref());
    optional_field(
        &mut fields,
        "output_path",
        completion.output_path.as_deref(),
    );
    optional_field(&mut fields, "thread_id", completion.thread_id.as_deref());
    Ok(format!(
        "<system_notification>\nThe following task has finished. If you were already aware, ignore this notification and do not restate prior responses.\n\n<task>\n{}\n</task>\n</system_notification>",
        fields.join("\n")
    ))
}

fn optional_field(fields: &mut Vec<String>, name: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        fields.push(format!("{name}: {value}"));
    }
}

fn status_name(status: pb::BackgroundTaskStatus) -> &'static str {
    match status {
        pb::BackgroundTaskStatus::Success => "success",
        pb::BackgroundTaskStatus::Error => "error",
        pb::BackgroundTaskStatus::Aborted => "aborted",
        pb::BackgroundTaskStatus::Unspecified => unreachable!(),
    }
}
