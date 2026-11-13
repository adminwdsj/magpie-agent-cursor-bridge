//! Explicit live smoke test: args are proxy URL and catalogue model hash.
use cursor_server::cursor::protocol::{connect,proto::{agent::v1 as pb,aiserver::v1 as ai}};
use prost::Message;
use futures_util::StreamExt;
use std::time::Duration;
async fn append(client:&reqwest::Client,id:&str,seq:i64,message:pb::AgentClientMessage) -> Result<(),Box<dyn std::error::Error>> {
 let body=ai::BidiAppendRequest{data_binary:message.encode_to_vec(),request_id:Some(ai::BidiRequestId{request_id:id.into()}),append_seqno:seq,..Default::default()};
 let r=client.post("https://api2.cursor.sh/aiserver.v1.BidiService/BidiAppend").header("Content-Type","application/proto").body(body.encode_to_vec()).send().await?;
 if !r.status().is_success(){return Err(format!("append {}: {}",r.status(),r.text().await?).into());} Ok(())
}
#[tokio::main]
async fn main()->Result<(),Box<dyn std::error::Error>>{
 let args:Vec<_>=std::env::args().collect();
 let client=reqwest::Client::builder().proxy(reqwest::Proxy::all(&args[1])?).timeout(Duration::from_secs(120)).build()?;
 let id=format!("bridge-smoke-{}",uuid::Uuid::new_v4());
 let message=pb::AgentClientMessage{message:Some(pb::agent_client_message::Message::RunRequest(pb::AgentRunRequest{
 action:Some(pb::ConversationAction{action:Some(pb::conversation_action::Action::UserMessageAction(pb::UserMessageAction{user_message:Some(pb::UserMessage{text:"Reply with exactly MAGPIE_BRIDGE_OK. Do not call any tools.".into(),message_id:format!("{id}-user"),mode:pb::AgentMode::Agent as i32,..Default::default()}),..Default::default()})),..Default::default()}),
 conversation_id:Some(id.clone()),run_id:Some(id.clone()),requested_model:Some(pb::RequestedModel{model_id:args[2].clone(),..Default::default()}),..Default::default()}))};
 append(&client,&id,0,message).await?;
 let response=client.post("https://api2.cursor.sh/agent.v1.AgentService/RunSSE").header("Content-Type","application/proto").body(pb::BidiRequestId{request_id:id.clone()}.encode_to_vec()).send().await?.error_for_status()?;
 let mut stream=response.bytes_stream();let mut buffer=Vec::new();let mut seq=1;let mut text=String::new();
 while let Some(chunk)=stream.next().await {
  buffer.extend_from_slice(&chunk?);
  while buffer.len()>=5 {
   let len=u32::from_be_bytes(buffer[1..5].try_into()?) as usize;
   if buffer.len()<len+5 {break;}
   let frame:Vec<_>=buffer.drain(..len+5).collect();
   if frame[0]&connect::END_STREAM_FLAG!=0 {
    let end:serde_json::Value=serde_json::from_slice(&frame[5..])?;
    if end.get("error").is_some(){return Err(format!("stream error: {end}").into());}
    if !text.contains("MAGPIE_BRIDGE_OK"){return Err(format!("unexpected response: {text}").into());}
    println!("TLS proxy -> Cursor Bidi/RunSSE -> Magpie -> model -> Cursor stream: {text}");return Ok(());
   }
   let server=pb::AgentServerMessage::decode(&frame[5..])?;
   let reply=match server.message {
    Some(pb::agent_server_message::Message::InteractionUpdate(u))=>{if let Some(pb::interaction_update::Message::TextDelta(t))=u.message{text.push_str(&t.text);}None},
    Some(pb::agent_server_message::Message::KvServerMessage(kv))=>Some(pb::AgentClientMessage{message:Some(pb::agent_client_message::Message::KvClientMessage(pb::KvClientMessage{id:kv.id,message:Some(pb::kv_client_message::Message::SetBlobResult(pb::SetBlobResult{error:None}))}))}),
    Some(pb::agent_server_message::Message::ExecServerMessage(exec))=>{
      if !matches!(exec.message,Some(pb::exec_server_message::Message::RequestContextArgs(_))){return Err("model requested an unexpected tool".into());}
      append(&client,&id,seq,pb::AgentClientMessage{message:Some(pb::agent_client_message::Message::ExecClientControlMessage(pb::ExecClientControlMessage{message:Some(pb::exec_client_control_message::Message::StreamClose(pb::ExecClientStreamClose{id:exec.id}))}))}).await?;seq+=1;
      Some(pb::AgentClientMessage{message:Some(pb::agent_client_message::Message::ExecClientMessage(pb::ExecClientMessage{id:exec.id,message:Some(pb::exec_client_message::Message::RequestContextResult(pb::RequestContextResult{result:Some(pb::request_context_result::Result::Success(pb::RequestContextSuccess{request_context:Some(pb::RequestContext::default()),..Default::default()}))})),..Default::default()}))})
    },_=>None
   };
   if let Some(reply)=reply{append(&client,&id,seq,reply).await?;seq+=1;}
  }
 }
 Err("stream closed before successful end".into())
}
