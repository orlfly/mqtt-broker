use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{self, Duration};
use tracing::{info, warn, debug};

use bytes::Buf;

use crate::packet::{
    self, connack_v3, connack_v5, decode_packet, Packet,
    PublishPacket as WirePublish, SubscribePacket, UnsubscribePacket,
    Properties,
    RC_SUCCESS, RC_UNSPECIFIED_ERROR,
};
use crate::state::{
    ClientInfo, MqttProtocol, QoS,
    SharedBrokerState, PublishPacket as StorePublish,
    Subscription as StoreSubscription,
};
use crate::session::SessionManager;
use crate::subscription::SubscriptionTree;
use crate::auth::AuthProvider;

pub struct ConnectionHandler {
    state: SharedBrokerState,
    session_manager: Arc<SessionManager>,
    subscription_tree: Arc<SubscriptionTree>,
    auth_provider: Arc<dyn AuthProvider>,
}

impl ConnectionHandler {
    pub fn new(
        state: SharedBrokerState,
        session_manager: Arc<SessionManager>,
        subscription_tree: Arc<SubscriptionTree>,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Self {
        Self { state, session_manager, subscription_tree, auth_provider }
    }

    pub async fn handle_connection(&self, mut stream: TcpStream, addr: SocketAddr) {
        info!("New connection from {}", addr);

        let mut buf = BytesMut::with_capacity(4096);

        // ── Read CONNECT ────────────────────────────────────────────────
        let connect = loop {
            if buf.len() > 65536 {
                warn!("[{}] CONNECT packet too large", addr);
                let _ = stream.shutdown().await;
                return;
            }
            match self.read_more(&mut stream, &mut buf).await {
                Ok(()) => {}
                Err(_) => {
                    let _ = stream.shutdown().await;
                    return;
                }
            }
            match decode_packet(&buf) {
                Ok((Packet::Connect(c), consumed)) => {
                    buf.advance(consumed);
                    break c;
                }
                Ok((_, _)) => {
                    warn!("[{}] Expected CONNECT, got something else", addr);
                    // Send v3 CONNACK with protocol error
                    let _ = stream.write_all(&[0x20, 0x02, 0x00, 0x00]).await;
                    let _ = stream.shutdown().await;
                    return;
                }
                Err(packet::DecodeError::InsufficientData) => continue,
                Err(e) => {
                    warn!("[{}] Failed to decode CONNECT: {}", addr, e);
                    let _ = stream.write_all(&[0x20, 0x02, 0x00, 0x00]).await;
                    let _ = stream.shutdown().await;
                    return;
                }
            }
        };

        let is_v5 = connect.protocol_version == 5;
        let client_id = connect.client_id.clone();

        if client_id.is_empty() {
            warn!("[{}] Empty client ID", addr);
            self.send_connack_raw(&mut stream, is_v5, RC_UNSPECIFIED_ERROR, "empty client id").await;
            let _ = stream.shutdown().await;
            return;
        }

        // ── Authenticate ────────────────────────────────────────────────
        let username = connect.username.as_deref().unwrap_or("");
        let password = std::str::from_utf8(
            connect.password.as_deref().unwrap_or(b"")
        ).unwrap_or("");
        if !self.auth_provider.authenticate(username, password).await {
            warn!("[{}] Auth failed for client {}", addr, client_id);
            // MQTT v3.1.1 return code 5 = not authorized
            self.send_connack_raw(&mut stream, is_v5, 0x87, "not authorized").await;
            let _ = stream.shutdown().await;
            return;
        }

        // ── Register client ─────────────────────────────────────────────
        let protocol_version = if is_v5 { MqttProtocol::V500 } else { MqttProtocol::V311 };
        let client_info = ClientInfo {
            client_id: client_id.clone(),
            addr,
            protocol_version: protocol_version.clone(),
            connected_at: Instant::now(),
            clean_session: connect.clean_start,
            keep_alive: connect.keep_alive,
            username: connect.username.clone(),
        };

        {
            let mut state = self.state.write().await;

            // Disconnect existing session with same client ID
            if let Some(existing) = state.clients.get(&client_id) {
                info!("[{}] Replacing existing connection from {}", client_id, existing.addr);
            }

            state.clients.insert(client_id.clone(), client_info);
        }

        // Create/update session
        if connect.clean_start {
            self.session_manager.delete_session(&client_id).await;
        }
        let will = connect.will.map(|w| crate::state::WillMessage {
            topic: w.topic,
            payload: w.payload,
            qos: match w.qos { 0 => QoS::AtMostOnce, 1 => QoS::AtLeastOnce, _ => QoS::ExactlyOnce },
            retain: w.retain,
        });
        self.session_manager.create_session(&client_id, connect.clean_start, will).await;

        info!(
            "[{}] Client connected (v{}, clean_start={}, keep_alive={}s)",
            client_id, connect.protocol_version, connect.clean_start, connect.keep_alive,
        );

        // ── Send CONNACK ────────────────────────────────────────────────
        self.send_connack_raw(&mut stream, is_v5, RC_SUCCESS, "").await;

        // ── Main packet loop ────────────────────────────────────────────
        let keep_alive = connect.keep_alive;
        self.packet_loop(&mut stream, &mut buf, &client_id, keep_alive, is_v5).await;

        info!("[{}] Connection closed ({})", client_id, addr);
    }

    async fn packet_loop(
        &self,
        stream: &mut TcpStream,
        buf: &mut BytesMut,
        client_id: &str,
        keep_alive: u16,
        is_v5: bool,
    ) {
        let timeout = if keep_alive > 0 {
            Duration::from_secs((keep_alive as f64 * 1.5).ceil() as u64)
        } else {
            Duration::from_secs(120)
        };

        loop {
            // Try to decode buffered data first
            if !buf.is_empty() {
                match decode_packet(buf) {
                    Ok((packet, consumed)) => {
                        buf.advance(consumed);
                        match packet {
                            Packet::Publish(p) => {
                                self.handle_publish(stream, client_id, p, is_v5).await;
                            }
                            Packet::Subscribe(p) => {
                                self.handle_subscribe(stream, p, client_id, is_v5).await;
                            }
                            Packet::Unsubscribe(p) => {
                                self.handle_unsubscribe(stream, p, client_id, is_v5).await;
                            }
                            Packet::Pingreq => {
                                let resp = vec![0xD0, 0x00]; // PINGRESP
                                let _ = stream.write_all(&resp).await;
                            }
                            Packet::Disconnect(d) => {
                                info!("[{}] Disconnect (reason={})", client_id, d.reason_code);
                                break;
                            }
                            _ => {
                                debug!("[{}] Ignoring packet type {:?}", client_id, packet);
                            }
                        }
                        continue;
                    }
                    Err(packet::DecodeError::InsufficientData) => {}
                    Err(e) => {
                        warn!("[{}] Packet decode error: {}", client_id, e);
                        break;
                    }
                }
            }

            // Read more data with timeout
            match time::timeout(timeout, self.read_more(stream, buf)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => break,
                Err(_) => {
                    // Timeout — assume client disconnected
                    warn!("[{}] Keep alive timeout", client_id);
                    break;
                }
            }
        }

        self.disconnect_client(client_id).await;
    }

    async fn read_more(&self, stream: &mut TcpStream, buf: &mut BytesMut) -> std::io::Result<()> {
        let mut chunk = vec![0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "eof"));
        }
        buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    async fn send_connack_raw(
        &self,
        stream: &mut TcpStream,
        is_v5: bool,
        reason_code: u8,
        reason: &str,
    ) {
        let bytes = if is_v5 {
            let mut props = Properties::new();
            if !reason.is_empty() {
                props.reason_string = Some(reason.to_string());
            }
            connack_v5(reason_code, &props)
        } else if reason_code == RC_SUCCESS {
            connack_v3(false, 0x00)
        } else {
            connack_v3(false, reason_code)
        };
        let _ = stream.write_all(&bytes).await;
    }

    async fn handle_publish(
        &self,
        stream: &mut TcpStream,
        client_id: &str,
        p: WirePublish,
        is_v5: bool,
    ) {
        // Convert QoS
        let qos = match p.qos {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            _ => QoS::ExactlyOnce,
        };

        // Store the publish
        let store = StorePublish {
            topic: p.topic.clone(),
            payload: p.payload.clone(),
            qos: qos.clone(),
            retain: p.retain,
            user_properties: p.properties.user_properties.clone(),
            content_type: p.properties.content_type.clone(),
            response_topic: p.properties.response_topic.clone(),
            correlation_data: p.properties.correlation_data.clone(),
            message_expiry_interval: p.properties.message_expiry_interval,
            payload_format_indicator: p.properties.payload_format_indicator,
            topic_alias: p.properties.topic_alias,
        };

        // Deliver to matching subscribers
        let subscribers = self.subscription_tree.match_topic(&p.topic).await;
        for sub in &subscribers {
            self.session_manager.enqueue_pending(&sub.client_id, store.clone()).await;
            debug!("[{}] -> [{}] on '{}' ({} bytes)",
                client_id, sub.client_id, p.topic, p.payload.len());
        }

        // PUBACK if QoS 1 (PUBCOMP for QoS 2 not yet supported)
        if p.qos == 1 {
            if let Some(pid) = p.packet_id {
                let mut ack = vec![0x40, 0x02]; // PUBACK
                ack.put_u16(pid);
                if is_v5 {
                    ack.push(RC_SUCCESS);
                    ack.push(0x00); // empty properties
                } else {
                    // v3.1.1 PUBACK is just packet id
                }
                let _ = stream.write_all(&ack).await;
            }
        }

        // Also notify management channel via state (for tools like list_topics)
        {
            let state = self.state.read().await;
            if state.subscriptions.is_empty() {
                debug!("[{}] No subscribers for topic '{}'", client_id, p.topic);
            }
        }
    }

    async fn handle_subscribe(
        &self,
        stream: &mut TcpStream,
        p: SubscribePacket,
        client_id: &str,
        _is_v5: bool,
    ) {
        let mut reason_codes = Vec::with_capacity(p.topic_filters.len());

        for tf in &p.topic_filters {
            let qos = match tf.options.qos {
                0 => QoS::AtMostOnce,
                1 => QoS::AtLeastOnce,
                _ => QoS::ExactlyOnce,
            };

            let store_sub = StoreSubscription {
                client_id: client_id.to_string(),
                topic_filter: tf.topic_filter.clone(),
                qos: qos.clone(),
                no_local: tf.options.no_local,
                retain_as_published: tf.options.retain_as_published,
                retain_handling: tf.options.retain_handling,
            };

            self.subscription_tree.add(store_sub).await;
            self.session_manager.add_subscription(client_id, tf.topic_filter.clone(), qos).await;

            reason_codes.push(tf.options.qos);

            debug!("[{}] Subscribed to '{}' (qos={})", client_id, tf.topic_filter, tf.options.qos);
        }

        let suback = Packet::Suback(packet::SubackPacket {
            packet_id: p.packet_id,
            properties: Properties::new(),
            reason_codes,
        });
        let encoded = suback.encode();
        let _ = stream.write_all(&encoded).await;
    }

    async fn handle_unsubscribe(
        &self,
        stream: &mut TcpStream,
        p: UnsubscribePacket,
        client_id: &str,
        _is_v5: bool,
    ) {
        for filter in &p.topic_filters {
            self.subscription_tree.remove(client_id, filter).await;
            self.session_manager.remove_subscription(client_id, filter).await;
            debug!("[{}] Unsubscribed from '{}'", client_id, filter);
        }

        let reason_codes = vec![RC_SUCCESS; p.topic_filters.len()];
        let unsuback = Packet::Unsuback(packet::UnsubackPacket {
            packet_id: p.packet_id,
            properties: Properties::new(),
            reason_codes,
        });
        let encoded = unsuback.encode();
        let _ = stream.write_all(&encoded).await;
    }

    pub async fn disconnect_client(&self, client_id: &str) {
        let will = {
            let state = self.state.read().await;
            state.session_store.get(client_id)
                .and_then(|s| s.will_message.clone())
        };

        self.subscription_tree.remove_client_subscriptions(client_id).await;

        if let Some(will) = will {
            let store = StorePublish {
                topic: will.topic,
                payload: will.payload,
                qos: will.qos,
                retain: will.retain,
                user_properties: Vec::new(),
                content_type: None,
                response_topic: None,
                correlation_data: None,
                message_expiry_interval: None,
                payload_format_indicator: None,
                topic_alias: None,
            };
            let subscribers = self.subscription_tree.match_topic(&store.topic).await;
            for sub in &subscribers {
                info!("Delivering will message from {} to {}", client_id, sub.client_id);
                self.session_manager.enqueue_pending(&sub.client_id, store.clone()).await;
            }
        }

        let _ = self.session_manager.delete_session(client_id).await;
        {
            let mut state = self.state.write().await;
            state.clients.remove(client_id);
        }
    }
}

// Helper for BytesMut put_u16
trait PutU16Ext {
    fn put_u16(&mut self, val: u16);
}

impl PutU16Ext for Vec<u8> {
    fn put_u16(&mut self, val: u16) {
        self.extend_from_slice(&val.to_be_bytes());
    }
}
