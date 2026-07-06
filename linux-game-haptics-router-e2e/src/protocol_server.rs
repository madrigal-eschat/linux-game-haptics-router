use anyhow::{Context, Result};
use buttplug_core::message::{
    ButtplugMessage, ButtplugClientMessageV4, ButtplugServerMessageV4, DeviceFeature,
    DeviceFeatureOutput, DeviceFeatureOutputValueProperties, DeviceListV4, DeviceMessageInfoV4,
    OkV0, ServerInfoV4,
};
use buttplug_core::util::range::RangeInclusive;
use buttplug_core::util::small_vec_enum_map::SmallVecEnumMap;
use futures_util::{SinkExt, StreamExt};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// One `OutputCmd` received from the connected buttplug client, tagged with
/// the moment the fake server's read loop observed it.
#[derive(Debug, Clone)]
pub struct ReceivedCommand {
    pub device_index: u32,
    pub feature_index: u32,
    pub value: i32,
    pub at: Instant,
}

fn device_list_message(id: u32) -> ButtplugServerMessageV4 {
    let mut features = BTreeMap::new();
    features.insert(
        0u32,
        DeviceFeature::new(
            0,
            "Fake Vibrator",
            &SmallVecEnumMap::from_iter([DeviceFeatureOutput::Vibrate(
                DeviceFeatureOutputValueProperties::new(RangeInclusive::new(0, 100)),
            )]),
            &SmallVecEnumMap::default(),
        ),
    );
    let info = DeviceMessageInfoV4::new(0, "Fake Vibrator", &None, 0, &features);
    let mut msg = ButtplugServerMessageV4::DeviceList(DeviceListV4::new(vec![info]));
    if let ButtplugServerMessageV4::DeviceList(ref mut dl) = msg {
        dl.set_id(id);
    }
    msg
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    tx: mpsc::UnboundedSender<ReceivedCommand>,
) -> Result<()> {
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .context("websocket handshake failed")?;

    while let Some(msg) = ws.next().await {
        let msg = msg.context("websocket read error")?;
        let Message::Text(text) = msg else { continue };
        let incoming: Vec<ButtplugClientMessageV4> =
            serde_json::from_str(&text).context("failed to parse client message")?;

        for client_msg in incoming {
            let reply = match client_msg {
                ButtplugClientMessageV4::RequestServerInfo(req) => {
                    let mut info = ServerInfoV4::new(
                        "fake-buttplug-server",
                        buttplug_core::message::ButtplugMessageSpecVersion::Version4,
                        0,
                        0,
                    );
                    info.set_id(req.id());
                    ButtplugServerMessageV4::ServerInfo(info)
                }
                ButtplugClientMessageV4::RequestDeviceList(req) => device_list_message(req.id()),
                ButtplugClientMessageV4::OutputCmd(cmd) => {
                    let _ = tx.send(ReceivedCommand {
                        device_index: cmd.device_index(),
                        feature_index: cmd.feature_index(),
                        value: cmd.command().value(),
                        at: Instant::now(),
                    });
                    let mut ok = OkV0::default();
                    ok.set_id(cmd.id());
                    ButtplugServerMessageV4::Ok(ok)
                }
                other => {
                    log::warn!("fake buttplug server: unhandled client message {:?}", other);
                    continue;
                }
            };
            let text = serde_json::to_string(&[&reply])?;
            ws.send(Message::Text(text.into())).await?;
        }
    }
    Ok(())
}

/// Binds a loopback TCP listener, spawns the accept/handshake/command loop as
/// a background task, and returns the bound address plus a channel that
/// yields every `OutputCmd` the connected client sends.
pub async fn spawn_fake_server() -> Result<(SocketAddr, mpsc::UnboundedReceiver<ReceivedCommand>)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind fake buttplug server")?;
    let addr = listener.local_addr()?;
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("fake buttplug server: accept failed: {}", e);
                    continue;
                }
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, tx).await {
                    log::warn!("fake buttplug server: connection ended: {}", e);
                }
            });
        }
    });

    Ok((addr, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use buttplug_core::message::{
        ButtplugMessageSpecVersion, OutputCmdV4, RequestDeviceListV0, RequestServerInfoV4,
    };
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn handshake_then_output_cmd_is_captured() {
        let (addr, mut rx) = spawn_fake_server().await.unwrap();
        let url = format!("ws://{}", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();

        let req = RequestServerInfoV4::new("test-client", ButtplugMessageSpecVersion::Version4, 0);
        let req = ButtplugClientMessageV4::RequestServerInfo(req);
        ws.send(Message::Text(
            serde_json::to_string(&[&req]).unwrap().into(),
        ))
        .await
        .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert!(reply.into_text().unwrap().contains("ServerInfo"));

        let req = RequestDeviceListV0::default();
        let req = ButtplugClientMessageV4::RequestDeviceList(req);
        ws.send(Message::Text(
            serde_json::to_string(&[&req]).unwrap().into(),
        ))
        .await
        .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        let reply_text = reply.into_text().unwrap();
        assert!(reply_text.contains("DeviceList"));
        assert!(reply_text.contains("Vibrate"));

        let cmd = OutputCmdV4::new(
            0,
            0,
            buttplug_core::message::OutputCommand::Vibrate(
                buttplug_core::message::OutputValue::new(42),
            ),
        );
        let cmd = ButtplugClientMessageV4::OutputCmd(cmd);
        ws.send(Message::Text(
            serde_json::to_string(&[&cmd]).unwrap().into(),
        ))
        .await
        .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert!(reply.into_text().unwrap().contains("Ok"));

        let received = rx.recv().await.unwrap();
        assert_eq!(received.device_index, 0);
        assert_eq!(received.feature_index, 0);
        assert_eq!(received.value, 42);
    }
}
