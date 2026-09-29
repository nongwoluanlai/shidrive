//! A second fixture, implemented entirely with the official SDK (not a model).
//! The production client and this peer exercise independent typed SDK roles.
use agent_client_protocol::{
    schema::{v1 as p, ProtocolVersion},
    Agent, Error, Lines,
};
use futures::{SinkExt, StreamExt};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let transport = Lines::new(
        SinkExt::<String>::sink_map_err(
            FramedWrite::new(tokio::io::stdout(), LinesCodec::new()),
            std::io::Error::other,
        ),
        FramedRead::new(
            tokio::io::stdin(),
            LinesCodec::new_with_max_length(32 * 1024 * 1024),
        )
        .map(|line| line.map_err(std::io::Error::other)),
    );
    Agent
        .builder()
        .on_receive_request(
            async |_: p::InitializeRequest, responder, _cx| {
                responder.respond(
                    p::InitializeResponse::new(ProtocolVersion::V1)
                        .agent_info(p::Implementation::new("official-sdk-test-peer", "2.2.0")),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |_: p::NewSessionRequest, responder, _cx| {
                responder.respond(p::NewSessionResponse::new("sdk-session"))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async |request: p::PromptRequest, responder, cx| {
                let task_cx = cx.clone();
                cx.spawn(async move {
                    let sid = request.session_id;
                    let permission = task_cx
                        .send_request(p::RequestPermissionRequest::new(
                            sid.clone(),
                            p::ToolCallUpdate::new("sdk-file", Default::default()),
                            vec![p::PermissionOption::new(
                                "allow",
                                "Allow",
                                p::PermissionOptionKind::AllowOnce,
                            )],
                        ))
                        .block_task()
                        .await?;
                    let stop = match permission.outcome {
                        p::RequestPermissionOutcome::Selected(_) => {
                            let Some(p::ContentBlock::Text(path)) = request.prompt.first() else {
                                return Err(Error::invalid_params());
                            };
                            task_cx
                                .send_request(p::WriteTextFileRequest::new(
                                    sid.clone(),
                                    &path.text,
                                    "SDK-to-SDK round trip\n",
                                ))
                                .block_task()
                                .await?;
                            let read = task_cx
                                .send_request(p::ReadTextFileRequest::new(sid.clone(), &path.text))
                                .block_task()
                                .await?;
                            task_cx.send_notification(p::SessionNotification::new(
                                sid,
                                p::SessionUpdate::AgentMessageChunk(p::ContentChunk::new(
                                    p::ContentBlock::Text(p::TextContent::new(read.content)),
                                )),
                            ))?;
                            p::StopReason::EndTurn
                        }
                        _ => p::StopReason::Cancelled,
                    };
                    responder.respond(p::PromptResponse::new(stop))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async |cx| {
            cx.incoming_closed().await;
            Ok(())
        })
        .await
}
