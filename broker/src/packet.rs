use bytes::{Buf, BufMut, BytesMut};
use std::fmt;

// ── Control Packet Types ──────────────────────────────────────────────────

const CONNECT: u8 = 1;
const CONNACK: u8 = 2;
const PUBLISH: u8 = 3;
const PUBACK: u8 = 4;
const PUBREC: u8 = 5;
const PUBREL: u8 = 6;
const PUBCOMP: u8 = 7;
const SUBSCRIBE: u8 = 8;
const SUBACK: u8 = 9;
const UNSUBSCRIBE: u8 = 10;
const UNSUBACK: u8 = 11;
const PINGREQ: u8 = 12;
const PINGRESP: u8 = 13;
const DISCONNECT: u8 = 14;

// ── Reason Codes (v5) ─────────────────────────────────────────────────────

pub const RC_SUCCESS: u8 = 0x00;
pub const RC_NORMAL_DISCONNECTION: u8 = 0x00;
pub const RC_GRANTED_QOS_0: u8 = 0x00;
pub const RC_GRANTED_QOS_1: u8 = 0x01;
pub const RC_GRANTED_QOS_2: u8 = 0x02;
pub const RC_NO_MATCHING_SUBSCRIBERS: u8 = 0x10;
pub const RC_UNSPECIFIED_ERROR: u8 = 0x80;
pub const RC_MALFORMED_PACKET: u8 = 0x81;
pub const RC_PROTOCOL_ERROR: u8 = 0x82;
pub const RC_TOPIC_NAME_INVALID: u8 = 0x90;
pub const RC_SESSION_EXPIRY_INVALID: u8 = 0x9B;

// ── Property Identifiers (v5) ─────────────────────────────────────────────

pub const PROP_PAYLOAD_FORMAT_INDICATOR: u8 = 0x01;
pub const PROP_MESSAGE_EXPIRY_INTERVAL: u8 = 0x02;
pub const PROP_CONTENT_TYPE: u8 = 0x03;
pub const PROP_RESPONSE_TOPIC: u8 = 0x08;
pub const PROP_CORRELATION_DATA: u8 = 0x09;
pub const PROP_SUBSCRIPTION_IDENTIFIER: u8 = 0x0B;
pub const PROP_SESSION_EXPIRY_INTERVAL: u8 = 0x11;
pub const PROP_ASSIGNED_CLIENT_IDENTIFIER: u8 = 0x12;
pub const PROP_SERVER_KEEP_ALIVE: u8 = 0x13;
pub const PROP_AUTHENTICATION_METHOD: u8 = 0x15;
pub const PROP_AUTHENTICATION_DATA: u8 = 0x16;
pub const PROP_REQUEST_PROBLEM_INFORMATION: u8 = 0x18;
pub const PROP_WILL_DELAY_INTERVAL: u8 = 0x19;
pub const PROP_REQUEST_RESPONSE_INFORMATION: u8 = 0x1A;
pub const PROP_RESPONSE_INFORMATION: u8 = 0x1C;
pub const PROP_SERVER_REFERENCE: u8 = 0x1D;
pub const PROP_REASON_STRING: u8 = 0x1E;
pub const PROP_RECEIVE_MAXIMUM: u8 = 0x21;
pub const PROP_TOPIC_ALIAS_MAXIMUM: u8 = 0x22;
pub const PROP_TOPIC_ALIAS: u8 = 0x23;
pub const PROP_MAXIMUM_QOS: u8 = 0x24;
pub const PROP_RETAIN_AVAILABLE: u8 = 0x25;
pub const PROP_USER_PROPERTY: u8 = 0x26;
pub const PROP_MAXIMUM_PACKET_SIZE: u8 = 0x27;
pub const PROP_WILDCARD_SUBSCRIPTION_AVAILABLE: u8 = 0x28;
pub const PROP_SUBSCRIPTION_IDENTIFIER_AVAILABLE: u8 = 0x29;
pub const PROP_SHARED_SUBSCRIPTION_AVAILABLE: u8 = 0x2A;

// ── Remaining Length Encoding ─────────────────────────────────────────────

fn encode_remaining_length(mut len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4);
    loop {
        let mut byte = (len & 0x7F) as u8;
        len >>= 7;
        if len > 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if len == 0 {
            break;
        }
    }
    buf
}

fn decode_remaining_length(data: &[u8]) -> Option<(usize, usize)> {
    let mut value: usize = 0;
    let mut multiplier: usize = 1;
    for i in 0..data.len() {
        let byte = data[i];
        value += ((byte & 0x7F) as usize) * multiplier;
        multiplier *= 128;
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

// ── User Property ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct UserProperty {
    pub key: String,
    pub value: String,
}

// ── MQTT Properties ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Properties {
    pub payload_format_indicator: Option<u8>,
    pub message_expiry_interval: Option<u32>,
    pub content_type: Option<String>,
    pub response_topic: Option<String>,
    pub correlation_data: Option<Vec<u8>>,
    pub subscription_identifier: Option<u32>,
    pub session_expiry_interval: Option<u32>,
    pub assigned_client_identifier: Option<String>,
    pub server_keep_alive: Option<u16>,
    pub authentication_method: Option<String>,
    pub authentication_data: Option<Vec<u8>>,
    pub request_problem_information: Option<u8>,
    pub will_delay_interval: Option<u32>,
    pub request_response_information: Option<u8>,
    pub response_information: Option<String>,
    pub server_reference: Option<String>,
    pub reason_string: Option<String>,
    pub receive_maximum: Option<u16>,
    pub topic_alias_maximum: Option<u16>,
    pub topic_alias: Option<u16>,
    pub maximum_qos: Option<u8>,
    pub retain_available: Option<u8>,
    pub maximum_packet_size: Option<u32>,
    pub wildcard_subscription_available: Option<u8>,
    pub subscription_identifier_available: Option<u8>,
    pub shared_subscription_available: Option<u8>,
    pub user_properties: Vec<UserProperty>,
}

impl Properties {
    pub fn new() -> Self {
        Self::default()
    }

    fn encoded_length(&self) -> usize {
        let mut len = 0usize;
        if self.payload_format_indicator.is_some() { len += 2; }
        if self.message_expiry_interval.is_some() { len += 5; }
        if let Some(v) = &self.content_type { len += 2 + v.len(); }
        if let Some(v) = &self.response_topic { len += 2 + v.len(); }
        if let Some(v) = &self.correlation_data { len += 2 + v.len(); }
        if let Some(_) = self.subscription_identifier { len += 1 + 4; }
        if self.session_expiry_interval.is_some() { len += 5; }
        if let Some(v) = &self.assigned_client_identifier { len += 2 + v.len(); }
        if self.server_keep_alive.is_some() { len += 3; }
        if let Some(v) = &self.authentication_method { len += 2 + v.len(); }
        if let Some(v) = &self.authentication_data { len += 2 + v.len(); }
        if self.request_problem_information.is_some() { len += 2; }
        if self.will_delay_interval.is_some() { len += 5; }
        if self.request_response_information.is_some() { len += 2; }
        if let Some(v) = &self.response_information { len += 2 + v.len(); }
        if let Some(v) = &self.server_reference { len += 2 + v.len(); }
        if let Some(v) = &self.reason_string { len += 2 + v.len(); }
        if self.receive_maximum.is_some() { len += 3; }
        if self.topic_alias_maximum.is_some() { len += 3; }
        if self.topic_alias.is_some() { len += 3; }
        if self.maximum_qos.is_some() { len += 2; }
        if self.retain_available.is_some() { len += 2; }
        if self.maximum_packet_size.is_some() { len += 5; }
        if self.wildcard_subscription_available.is_some() { len += 2; }
        if self.subscription_identifier_available.is_some() { len += 2; }
        if self.shared_subscription_available.is_some() { len += 2; }
        for up in &self.user_properties {
            len += 1 + 2 + up.key.len() + 2 + up.value.len();
        }
        len
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        let props_len = self.encoded_length();
        buf.extend(encode_remaining_length(props_len));

        if let Some(v) = self.payload_format_indicator {
            buf.push(PROP_PAYLOAD_FORMAT_INDICATOR); buf.push(v);
        }
        if let Some(v) = self.message_expiry_interval {
            buf.push(PROP_MESSAGE_EXPIRY_INTERVAL); buf.put_u32(v);
        }
        if let Some(v) = &self.content_type {
            buf.push(PROP_CONTENT_TYPE); encode_utf8(buf, v);
        }
        if let Some(v) = &self.response_topic {
            buf.push(PROP_RESPONSE_TOPIC); encode_utf8(buf, v);
        }
        if let Some(v) = &self.correlation_data {
            buf.push(PROP_CORRELATION_DATA); encode_binary(buf, v);
        }
        if let Some(v) = self.subscription_identifier {
            buf.push(PROP_SUBSCRIPTION_IDENTIFIER); buf.extend(encode_remaining_length(v as _));
        }
        if let Some(v) = self.session_expiry_interval {
            buf.push(PROP_SESSION_EXPIRY_INTERVAL); buf.put_u32(v);
        }
        if let Some(v) = &self.assigned_client_identifier {
            buf.push(PROP_ASSIGNED_CLIENT_IDENTIFIER); encode_utf8(buf, v);
        }
        if let Some(v) = self.server_keep_alive {
            buf.push(PROP_SERVER_KEEP_ALIVE); buf.put_u16(v);
        }
        if let Some(v) = &self.authentication_method {
            buf.push(PROP_AUTHENTICATION_METHOD); encode_utf8(buf, v);
        }
        if let Some(v) = &self.authentication_data {
            buf.push(PROP_AUTHENTICATION_DATA); encode_binary(buf, v);
        }
        if let Some(v) = self.request_problem_information {
            buf.push(PROP_REQUEST_PROBLEM_INFORMATION); buf.push(v);
        }
        if let Some(v) = self.will_delay_interval {
            buf.push(PROP_WILL_DELAY_INTERVAL); buf.put_u32(v);
        }
        if let Some(v) = self.request_response_information {
            buf.push(PROP_REQUEST_RESPONSE_INFORMATION); buf.push(v);
        }
        if let Some(v) = &self.response_information {
            buf.push(PROP_RESPONSE_INFORMATION); encode_utf8(buf, v);
        }
        if let Some(v) = &self.server_reference {
            buf.push(PROP_SERVER_REFERENCE); encode_utf8(buf, v);
        }
        if let Some(v) = &self.reason_string {
            buf.push(PROP_REASON_STRING); encode_utf8(buf, v);
        }
        if let Some(v) = self.receive_maximum {
            buf.push(PROP_RECEIVE_MAXIMUM); buf.put_u16(v);
        }
        if let Some(v) = self.topic_alias_maximum {
            buf.push(PROP_TOPIC_ALIAS_MAXIMUM); buf.put_u16(v);
        }
        if let Some(v) = self.topic_alias {
            buf.push(PROP_TOPIC_ALIAS); buf.put_u16(v);
        }
        if let Some(v) = self.maximum_qos {
            buf.push(PROP_MAXIMUM_QOS); buf.push(v);
        }
        if let Some(v) = self.retain_available {
            buf.push(PROP_RETAIN_AVAILABLE); buf.push(v);
        }
        if let Some(v) = self.maximum_packet_size {
            buf.push(PROP_MAXIMUM_PACKET_SIZE); buf.put_u32(v);
        }
        if let Some(v) = self.wildcard_subscription_available {
            buf.push(PROP_WILDCARD_SUBSCRIPTION_AVAILABLE); buf.push(v);
        }
        if let Some(v) = self.subscription_identifier_available {
            buf.push(PROP_SUBSCRIPTION_IDENTIFIER_AVAILABLE); buf.push(v);
        }
        if let Some(v) = self.shared_subscription_available {
            buf.push(PROP_SHARED_SUBSCRIPTION_AVAILABLE); buf.push(v);
        }
        for up in &self.user_properties {
            buf.push(PROP_USER_PROPERTY);
            encode_utf8(buf, &up.key);
            encode_utf8(buf, &up.value);
        }
    }

    fn decode(data: &[u8]) -> Result<(Self, usize), &'static str> {
        let (props_len, consumed) = decode_remaining_length(data)
            .ok_or("invalid properties length")?;
        if data.len() < consumed + props_len {
            return Err("properties data truncated");
        }
        let mut pos = consumed;
        let end = consumed + props_len;
        let mut props = Properties::new();

        while pos < end {
            let id = data[pos];
            pos += 1;
            match id {
                PROP_PAYLOAD_FORMAT_INDICATOR => {
                    if pos + 1 > end { return Err("truncated payload format indicator"); }
                    props.payload_format_indicator = Some(data[pos]);
                    pos += 1;
                }
                PROP_MESSAGE_EXPIRY_INTERVAL => {
                    if pos + 4 > end { return Err("truncated message expiry"); }
                    props.message_expiry_interval = Some(u32::from_be_bytes(
                        data[pos..pos+4].try_into().unwrap()
                    ));
                    pos += 4;
                }
                PROP_CONTENT_TYPE => {
                    let (s, n) = decode_utf8(&data[pos..end])
                        .ok_or("invalid content type")?;
                    props.content_type = Some(s.to_string());
                    pos += n;
                }
                PROP_RESPONSE_TOPIC => {
                    let (s, n) = decode_utf8(&data[pos..end])
                        .ok_or("invalid response topic")?;
                    props.response_topic = Some(s.to_string());
                    pos += n;
                }
                PROP_CORRELATION_DATA => {
                    let (b, n) = decode_binary(&data[pos..end])
                        .ok_or("invalid correlation data")?;
                    props.correlation_data = Some(b.to_vec());
                    pos += n;
                }
                PROP_SUBSCRIPTION_IDENTIFIER => {
                    let (v, n) = decode_remaining_length(&data[pos..end])
                        .ok_or("invalid subscription identifier")?;
                    props.subscription_identifier = Some(v as u32);
                    pos += n;
                }
                PROP_SESSION_EXPIRY_INTERVAL => {
                    if pos + 4 > end { return Err("truncated session expiry"); }
                    props.session_expiry_interval = Some(u32::from_be_bytes(
                        data[pos..pos+4].try_into().unwrap()
                    ));
                    pos += 4;
                }
                PROP_ASSIGNED_CLIENT_IDENTIFIER => {
                    let (s, n) = decode_utf8(&data[pos..end])
                        .ok_or("invalid assigned client id")?;
                    props.assigned_client_identifier = Some(s.to_string());
                    pos += n;
                }
                PROP_SERVER_KEEP_ALIVE => {
                    if pos + 2 > end { return Err("truncated server keep alive"); }
                    props.server_keep_alive = Some(u16::from_be_bytes(
                        data[pos..pos+2].try_into().unwrap()
                    ));
                    pos += 2;
                }
                PROP_AUTHENTICATION_METHOD => {
                    let (s, n) = decode_utf8(&data[pos..end])
                        .ok_or("invalid auth method")?;
                    props.authentication_method = Some(s.to_string());
                    pos += n;
                }
                PROP_AUTHENTICATION_DATA => {
                    let (b, n) = decode_binary(&data[pos..end])
                        .ok_or("invalid auth data")?;
                    props.authentication_data = Some(b.to_vec());
                    pos += n;
                }
                PROP_REQUEST_PROBLEM_INFORMATION => {
                    if pos + 1 > end { return Err("truncated request problem info"); }
                    props.request_problem_information = Some(data[pos]);
                    pos += 1;
                }
                PROP_WILL_DELAY_INTERVAL => {
                    if pos + 4 > end { return Err("truncated will delay"); }
                    props.will_delay_interval = Some(u32::from_be_bytes(
                        data[pos..pos+4].try_into().unwrap()
                    ));
                    pos += 4;
                }
                PROP_REQUEST_RESPONSE_INFORMATION => {
                    if pos + 1 > end { return Err("truncated request response info"); }
                    props.request_response_information = Some(data[pos]);
                    pos += 1;
                }
                PROP_RESPONSE_INFORMATION => {
                    let (s, n) = decode_utf8(&data[pos..end])
                        .ok_or("invalid response info")?;
                    props.response_information = Some(s.to_string());
                    pos += n;
                }
                PROP_SERVER_REFERENCE => {
                    let (s, n) = decode_utf8(&data[pos..end])
                        .ok_or("invalid server reference")?;
                    props.server_reference = Some(s.to_string());
                    pos += n;
                }
                PROP_REASON_STRING => {
                    let (s, n) = decode_utf8(&data[pos..end])
                        .ok_or("invalid reason string")?;
                    props.reason_string = Some(s.to_string());
                    pos += n;
                }
                PROP_RECEIVE_MAXIMUM => {
                    if pos + 2 > end { return Err("truncated receive maximum"); }
                    props.receive_maximum = Some(u16::from_be_bytes(
                        data[pos..pos+2].try_into().unwrap()
                    ));
                    pos += 2;
                }
                PROP_TOPIC_ALIAS_MAXIMUM => {
                    if pos + 2 > end { return Err("truncated topic alias max"); }
                    props.topic_alias_maximum = Some(u16::from_be_bytes(
                        data[pos..pos+2].try_into().unwrap()
                    ));
                    pos += 2;
                }
                PROP_TOPIC_ALIAS => {
                    if pos + 2 > end { return Err("truncated topic alias"); }
                    props.topic_alias = Some(u16::from_be_bytes(
                        data[pos..pos+2].try_into().unwrap()
                    ));
                    pos += 2;
                }
                PROP_MAXIMUM_QOS => {
                    if pos + 1 > end { return Err("truncated max qos"); }
                    props.maximum_qos = Some(data[pos]);
                    pos += 1;
                }
                PROP_RETAIN_AVAILABLE => {
                    if pos + 1 > end { return Err("truncated retain available"); }
                    props.retain_available = Some(data[pos]);
                    pos += 1;
                }
                PROP_MAXIMUM_PACKET_SIZE => {
                    if pos + 4 > end { return Err("truncated max packet size"); }
                    props.maximum_packet_size = Some(u32::from_be_bytes(
                        data[pos..pos+4].try_into().unwrap()
                    ));
                    pos += 4;
                }
                PROP_WILDCARD_SUBSCRIPTION_AVAILABLE => {
                    if pos + 1 > end { return Err("truncated wildcard available"); }
                    props.wildcard_subscription_available = Some(data[pos]);
                    pos += 1;
                }
                PROP_SUBSCRIPTION_IDENTIFIER_AVAILABLE => {
                    if pos + 1 > end { return Err("truncated sub id available"); }
                    props.subscription_identifier_available = Some(data[pos]);
                    pos += 1;
                }
                PROP_SHARED_SUBSCRIPTION_AVAILABLE => {
                    if pos + 1 > end { return Err("truncated shared sub available"); }
                    props.shared_subscription_available = Some(data[pos]);
                    pos += 1;
                }
                PROP_USER_PROPERTY => {
                    let (key, n1) = decode_utf8(&data[pos..end])
                        .ok_or("invalid user property key")?;
                    pos += n1;
                    let (value, n2) = decode_utf8(&data[pos..end])
                        .ok_or("invalid user property value")?;
                    pos += n2;
                    props.user_properties.push(UserProperty {
                        key: key.to_string(),
                        value: value.to_string(),
                    });
                }
                _ => {
                    return Err("unknown property id");
                }
            }
        }

        Ok((props, end))
    }
}

// ── UTF-8 String Encoding / Decoding ──────────────────────────────────────

fn encode_utf8(buf: &mut Vec<u8>, s: &str) {
    let len = s.len();
    assert!(len <= u16::MAX as usize, "MQTT string too long");
    buf.put_u16(len as u16);
    buf.extend_from_slice(s.as_bytes());
}

fn decode_utf8(data: &[u8]) -> Option<(&str, usize)> {
    if data.len() < 2 {
        return None;
    }
    let len = u16::from_be_bytes(data[0..2].try_into().unwrap()) as usize;
    if data.len() < 2 + len {
        return None;
    }
    let s = std::str::from_utf8(&data[2..2 + len]).ok()?;
    Some((s, 2 + len))
}

fn encode_binary(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len();
    assert!(len <= u16::MAX as usize);
    buf.put_u16(len as u16);
    buf.extend_from_slice(data);
}

fn decode_binary(data: &[u8]) -> Option<(&[u8], usize)> {
    if data.len() < 2 {
        return None;
    }
    let len = u16::from_be_bytes(data[0..2].try_into().unwrap()) as usize;
    if data.len() < 2 + len {
        return None;
    }
    Some((&data[2..2 + len], 2 + len))
}

// ── Connect Packet ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Will {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    pub retain: bool,
    pub properties: Properties,
}

#[derive(Debug, Clone)]
pub struct ConnectPacket {
    pub protocol_version: u8,
    pub clean_start: bool,
    pub keep_alive: u16,
    pub properties: Properties,
    pub client_id: String,
    pub will: Option<Will>,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
}

// ── Connack Packet ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ConnackPacket {
    pub session_present: bool,
    pub reason_code: u8,
    pub properties: Properties,
}

// ── Publish Packet ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PublishPacket {
    pub dup: bool,
    pub qos: u8,
    pub retain: bool,
    pub topic: String,
    pub packet_id: Option<u16>,
    pub properties: Properties,
    pub payload: Vec<u8>,
}

// ── Subscribe Packet ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SubscribeOptions {
    pub qos: u8,
    pub no_local: bool,
    pub retain_as_published: bool,
    pub retain_handling: u8,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self { qos: 0, no_local: false, retain_as_published: false, retain_handling: 0 }
    }
}

#[derive(Debug, Clone)]
pub struct SubscribeTopic {
    pub topic_filter: String,
    pub options: SubscribeOptions,
}

#[derive(Debug, Clone)]
pub struct SubscribePacket {
    pub packet_id: u16,
    pub properties: Properties,
    pub topic_filters: Vec<SubscribeTopic>,
}

// ── Suback Packet ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SubackPacket {
    pub packet_id: u16,
    pub properties: Properties,
    pub reason_codes: Vec<u8>,
}

// ── Unsubscribe Packet ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UnsubscribePacket {
    pub packet_id: u16,
    pub properties: Properties,
    pub topic_filters: Vec<String>,
}

// ── Unsuback Packet ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UnsubackPacket {
    pub packet_id: u16,
    pub properties: Properties,
    pub reason_codes: Vec<u8>,
}

// ── Disconnect Packet ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DisconnectPacket {
    pub reason_code: u8,
    pub properties: Properties,
}

// ── Enum of All Packet Types ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Packet {
    Connect(ConnectPacket),
    Connack(ConnackPacket),
    Publish(PublishPacket),
    Subscribe(SubscribePacket),
    Suback(SubackPacket),
    Unsubscribe(UnsubscribePacket),
    Unsuback(UnsubackPacket),
    Pingreq,
    Pingresp,
    Disconnect(DisconnectPacket),
}

impl Packet {
    fn type_name(&self) -> &'static str {
        match self {
            Packet::Connect(_) => "CONNECT",
            Packet::Connack(_) => "CONNACK",
            Packet::Publish(_) => "PUBLISH",
            Packet::Subscribe(_) => "SUBSCRIBE",
            Packet::Suback(_) => "SUBACK",
            Packet::Unsubscribe(_) => "UNSUBSCRIBE",
            Packet::Unsuback(_) => "UNSUBACK",
            Packet::Pingreq => "PINGREQ",
            Packet::Pingresp => "PINGRESP",
            Packet::Disconnect(_) => "DISCONNECT",
        }
    }
}

impl fmt::Display for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.type_name())
    }
}

// ── Encode ────────────────────────────────────────────────────────────────

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Packet::Connack(p) => encode_connack(p),
            Packet::Suback(p) => encode_suback(p),
            Packet::Unsuback(p) => encode_unsuback(p),
            Packet::Pingresp => encode_simple(PINGRESP, 0x00),
            Packet::Publish(p) => encode_publish(p),
            _ => panic!("Cannot encode {:?}", self),
        }
    }
}

fn encode_simple(packet_type: u8, flags: u8) -> Vec<u8> {
    let rl = encode_remaining_length(0);
    let mut buf = Vec::with_capacity(1 + rl.len());
    buf.push((packet_type << 4) | flags);
    buf.extend(rl);
    buf
}

fn encode_connack(p: &ConnackPacket) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(if p.session_present { 1 } else { 0 });
    payload.push(p.reason_code);
    p.properties.encode(&mut payload);

    let rl = encode_remaining_length(payload.len());
    let mut buf = Vec::with_capacity(1 + rl.len() + payload.len());
    buf.push(CONNACK << 4);
    buf.extend(rl);
    buf.extend(payload);
    buf
}

fn encode_suback(p: &SubackPacket) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.put_u16(p.packet_id);
    p.properties.encode(&mut payload);
    payload.extend(&p.reason_codes);

    let rl = encode_remaining_length(payload.len());
    let mut buf = Vec::with_capacity(1 + rl.len() + payload.len());
    buf.push(SUBACK << 4);
    buf.extend(rl);
    buf.extend(payload);
    buf
}

fn encode_unsuback(p: &UnsubackPacket) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.put_u16(p.packet_id);
    p.properties.encode(&mut payload);
    payload.extend(&p.reason_codes);

    let rl = encode_remaining_length(payload.len());
    let mut buf = Vec::with_capacity(1 + rl.len() + payload.len());
    buf.push(UNSUBACK << 4);
    buf.extend(rl);
    buf.extend(payload);
    buf
}

fn encode_publish(p: &PublishPacket) -> Vec<u8> {
    let mut flags = 0u8;
    if p.dup { flags |= 0x08; }
    flags |= (p.qos & 0x03) << 1;
    if p.retain { flags |= 0x01; }

    let mut payload = Vec::new();
    encode_utf8(&mut payload, &p.topic);
    if p.qos > 0 {
        payload.put_u16(p.packet_id.unwrap_or(0));
    }
    p.properties.encode(&mut payload);
    payload.extend(&p.payload);

    let rl = encode_remaining_length(payload.len());
    let mut buf = Vec::with_capacity(1 + rl.len() + payload.len());
    buf.push((PUBLISH << 4) | flags);
    buf.extend(rl);
    buf.extend(payload);
    buf
}

// ── Decode ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DecodeError {
    InsufficientData,
    Malformed(&'static str),
    UnsupportedPacket(u8),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::InsufficientData => write!(f, "insufficient data"),
            DecodeError::Malformed(msg) => write!(f, "malformed packet: {}", msg),
            DecodeError::UnsupportedPacket(t) => write!(f, "unsupported packet type: {}", t),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode a single MQTT packet from `data`. Returns `(Packet, total_bytes_consumed)`
/// or an error. If `InsufficientData` is returned, the caller should read more
/// bytes and call again (the decoder is stateless).
pub fn decode_packet(data: &[u8]) -> Result<(Packet, usize), DecodeError> {
    if data.is_empty() {
        return Err(DecodeError::InsufficientData);
    }

    let first_byte = data[0];
    let packet_type = first_byte >> 4;
    let flags = first_byte & 0x0F;

    let (remaining_len, rl_size) = decode_remaining_length(&data[1..])
        .ok_or(DecodeError::InsufficientData)?;

    let header_size = 1 + rl_size;
    let total_size = header_size + remaining_len;

    if data.len() < total_size {
        return Err(DecodeError::InsufficientData);
    }

    let body = &data[header_size..total_size];

    match packet_type {
        CONNECT => {
            let p = decode_connect(body)?;
            Ok((Packet::Connect(p), total_size))
        }
        PUBLISH => {
            let p = decode_publish(body, flags)?;
            Ok((Packet::Publish(p), total_size))
        }
        SUBSCRIBE => {
            let p = decode_subscribe(body)?;
            Ok((Packet::Subscribe(p), total_size))
        }
        UNSUBSCRIBE => {
            let p = decode_unsubscribe(body)?;
            Ok((Packet::Unsubscribe(p), total_size))
        }
        PINGREQ => {
            Ok((Packet::Pingreq, total_size))
        }
        DISCONNECT => {
            let p = decode_disconnect(body)?;
            Ok((Packet::Disconnect(p), total_size))
        }
        PUBACK | PUBREC | PUBREL | PUBCOMP => {
            // Acknowledge QoS flows silently (not yet implemented)
            Ok((Packet::Pingresp, total_size))
        }
        CONNACK => {
            let session_present = body.first().copied().unwrap_or(0) != 0;
            let reason_code = body.get(1).copied().unwrap_or(0);
            Ok((Packet::Connack(ConnackPacket {
                session_present,
                reason_code,
                properties: Properties::new(),
            }), total_size))
        }
        SUBACK => {
            if body.len() < 2 {
                return Err(DecodeError::Malformed("truncated suback"));
            }
            let packet_id = u16::from_be_bytes(body[0..2].try_into().unwrap());
            let mut pos = 2;
            let (properties, n) = Properties::decode(&body[pos..])
                .map_err(DecodeError::Malformed)?;
            pos += n;
            let reason_codes = body[pos..].to_vec();
            Ok((Packet::Suback(SubackPacket { packet_id, properties, reason_codes }), total_size))
        }
        UNSUBACK | PINGRESP => {
            Ok((Packet::Pingresp, total_size))
        }
        _ => Err(DecodeError::UnsupportedPacket(packet_type)),
    }
}

fn decode_connect(body: &[u8]) -> Result<ConnectPacket, DecodeError> {
    let (protocol_name, mut pos) = decode_utf8(body)
        .ok_or(DecodeError::Malformed("missing protocol name"))?;
    if protocol_name != "MQTT" {
        return Err(DecodeError::Malformed("invalid protocol name"));
    }
    if body.len() < pos + 1 {
        return Err(DecodeError::Malformed("missing protocol version"));
    }
    let protocol_version = body[pos];
    pos += 1;

    if protocol_version != 4 && protocol_version != 5 {
        return Err(DecodeError::Malformed("unsupported protocol version"));
    }

    if body.len() < pos + 1 {
        return Err(DecodeError::Malformed("missing connect flags"));
    }
    let connect_flags = body[pos];
    pos += 1;

    if body.len() < pos + 2 {
        return Err(DecodeError::Malformed("missing keep alive"));
    }
    let keep_alive = u16::from_be_bytes(body[pos..pos+2].try_into().unwrap());
    pos += 2;

    let clean_start = (connect_flags & 0x02) != 0;
    let will_flag = (connect_flags & 0x04) != 0;
    let will_qos = (connect_flags >> 3) & 0x03;
    let will_retain = (connect_flags & 0x20) != 0;
    let password_flag = (connect_flags & 0x40) != 0;
    let username_flag = (connect_flags & 0x80) != 0;

    let mut properties = Properties::new();
    if protocol_version == 5 {
        let (props, n) = Properties::decode(&body[pos..])
            .map_err(DecodeError::Malformed)?;
        properties = props;
        pos += n;
    }

    let client_id = if body.len() <= pos {
        return Err(DecodeError::Malformed("missing client id"));
    } else {
        let (cid, n) = decode_utf8(&body[pos..])
            .ok_or(DecodeError::Malformed("invalid client id"))?;
        pos += n;
        cid.to_string()
    };

    let will = if will_flag {
        let will_props = if protocol_version == 5 {
            let (wp, n) = Properties::decode(&body[pos..])
                .map_err(DecodeError::Malformed)?;
            pos += n;
            wp
        } else {
            Properties::new()
        };
        let (topic, n) = decode_utf8(&body[pos..])
            .ok_or(DecodeError::Malformed("invalid will topic"))?;
        pos += n;
        let (payload, n) = decode_binary(&body[pos..])
            .ok_or(DecodeError::Malformed("invalid will payload"))?;
        pos += n;
        Some(Will {
            topic: topic.to_string(),
            payload: payload.to_vec(),
            qos: will_qos,
            retain: will_retain,
            properties: will_props,
        })
    } else {
        None
    };

    let username = if username_flag {
        let (u, n) = decode_utf8(&body[pos..])
            .ok_or(DecodeError::Malformed("invalid username"))?;
        pos += n;
        Some(u.to_string())
    } else {
        None
    };

    let password = if password_flag {
        let (p, _n) = decode_binary(&body[pos..])
            .ok_or(DecodeError::Malformed("invalid password"))?;
        Some(p.to_vec())
    } else {
        None
    };

    Ok(ConnectPacket {
        protocol_version,
        clean_start,
        keep_alive,
        properties,
        client_id,
        will,
        username,
        password,
    })
}

fn decode_publish(body: &[u8], flags: u8) -> Result<PublishPacket, DecodeError> {
    let dup = (flags & 0x08) != 0;
    let qos = (flags >> 1) & 0x03;
    let retain = (flags & 0x01) != 0;

    let (topic, mut pos) = decode_utf8(body)
        .ok_or(DecodeError::Malformed("invalid publish topic"))?;

    let packet_id = if qos > 0 {
        if body.len() < pos + 2 {
            return Err(DecodeError::Malformed("missing packet id"));
        }
        let id = u16::from_be_bytes(body[pos..pos+2].try_into().unwrap());
        pos += 2;
        Some(id)
    } else {
        None
    };

    let (properties, n) = Properties::decode(&body[pos..])
        .map_err(DecodeError::Malformed)?;
    pos += n;

    let payload = body[pos..].to_vec();

    Ok(PublishPacket {
        dup,
        qos,
        retain,
        topic: topic.to_string(),
        packet_id,
        properties,
        payload,
    })
}

fn decode_subscribe(body: &[u8]) -> Result<SubscribePacket, DecodeError> {
    if body.len() < 2 {
        return Err(DecodeError::Malformed("missing packet id"));
    }
    let packet_id = u16::from_be_bytes(body[0..2].try_into().unwrap());
    let mut pos = 2;

    let (properties, n) = Properties::decode(&body[pos..])
        .map_err(DecodeError::Malformed)?;
    pos += n;

    let mut topic_filters = Vec::new();
    while pos < body.len() {
        let (filter, n) = decode_utf8(&body[pos..])
            .ok_or(DecodeError::Malformed("invalid subscribe topic"))?;
        pos += n;
        if body.len() <= pos {
            return Err(DecodeError::Malformed("missing subscribe options"));
        }
        let opts_byte = body[pos];
        pos += 1;
        topic_filters.push(SubscribeTopic {
            topic_filter: filter.to_string(),
            options: SubscribeOptions {
                qos: opts_byte & 0x03,
                no_local: (opts_byte & 0x04) != 0,
                retain_as_published: (opts_byte & 0x08) != 0,
                retain_handling: (opts_byte >> 4) & 0x03,
            },
        });
    }

    Ok(SubscribePacket { packet_id, properties, topic_filters })
}

fn decode_unsubscribe(body: &[u8]) -> Result<UnsubscribePacket, DecodeError> {
    if body.len() < 2 {
        return Err(DecodeError::Malformed("missing packet id"));
    }
    let packet_id = u16::from_be_bytes(body[0..2].try_into().unwrap());
    let mut pos = 2;

    let (properties, n) = Properties::decode(&body[pos..])
        .map_err(DecodeError::Malformed)?;
    pos += n;

    let mut topic_filters = Vec::new();
    while pos < body.len() {
        let (filter, n) = decode_utf8(&body[pos..])
            .ok_or(DecodeError::Malformed("invalid unsubscribe topic"))?;
        pos += n;
        topic_filters.push(filter.to_string());
    }

    Ok(UnsubscribePacket { packet_id, properties, topic_filters })
}

fn decode_disconnect(body: &[u8]) -> Result<DisconnectPacket, DecodeError> {
    if body.is_empty() {
        return Ok(DisconnectPacket {
            reason_code: RC_NORMAL_DISCONNECTION,
            properties: Properties::new(),
        });
    }
    let reason_code = body[0];
    let properties = if body.len() > 1 {
        let (p, _) = Properties::decode(&body[1..])
            .map_err(DecodeError::Malformed)?;
        p
    } else {
        Properties::new()
    };
    Ok(DisconnectPacket { reason_code, properties })
}

// ── Stream Decoder (stateless) ────────────────────────────────────────────

pub struct MqttDecoder;

impl MqttDecoder {
    pub fn new() -> Self {
        Self
    }

    /// Attempt to decode a single packet from `src`. Returns:
    /// - `Ok(Some(packet))` if a complete packet was decoded
    /// - `Ok(None)` if more data is needed
    /// - `Err` on protocol error
    pub fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Packet>, DecodeError> {
        match decode_packet(src) {
            Ok((packet, consumed)) => {
                src.advance(consumed);
                Ok(Some(packet))
            }
            Err(DecodeError::InsufficientData) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ── Connect Return Code Helpers ───────────────────────────────────────────

pub fn connack_v3(session_present: bool, return_code: u8) -> Vec<u8> {
    let mut buf = vec![CONNACK << 4, 0x02];
    buf.push(if session_present { 1 } else { 0 });
    buf.push(return_code);
    buf
}

pub fn connack_v5(reason_code: u8, properties: &Properties) -> Vec<u8> {
    let connack = Packet::Connack(ConnackPacket {
        session_present: false,
        reason_code,
        properties: properties.clone(),
    });
    connack.encode()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remaining_length_roundtrip() {
        for val in [0usize, 1, 127, 128, 16383, 16384, 2097151, 268435455] {
            let encoded = encode_remaining_length(val);
            let (decoded, n) = decode_remaining_length(&encoded).unwrap();
            assert_eq!(decoded, val, "roundtrip failed for {}", val);
            assert_eq!(n, encoded.len());
        }
    }

    #[test]
    fn test_utf8_roundtrip() {
        let s = "你好世界";
        let mut buf = Vec::new();
        encode_utf8(&mut buf, s);
        let (decoded, n) = decode_utf8(&buf).unwrap();
        assert_eq!(decoded, s);
        assert_eq!(n, 2 + s.len());
    }

    #[test]
    fn test_connect_v5_basic() {
        // Minimal MQTT v5 CONNECT: protocol "MQTT", version 5, clean_start, keep_alive 60,
        // client_id "test-client"
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "MQTT");
        raw.push(5); // v5
        raw.push(0x02); // connect flags: clean start
        raw.put_u16(60); // keep alive
        // v5 properties: empty
        raw.push(0x00); // properties length = 0
        encode_utf8(&mut raw, "test-client");

        let packet = decode_connect(&raw).unwrap();
        assert_eq!(packet.protocol_version, 5);
        assert!(packet.clean_start);
        assert_eq!(packet.keep_alive, 60);
        assert_eq!(packet.client_id, "test-client");
        assert!(packet.will.is_none());
        assert!(packet.username.is_none());
    }

    #[test]
    fn test_connect_v3_1_1() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "MQTT");
        raw.push(4); // v3.1.1
        raw.push(0x02); // clean session
        raw.put_u16(30);
        encode_utf8(&mut raw, "legacy-client");

        let packet = decode_connect(&raw).unwrap();
        assert_eq!(packet.protocol_version, 4);
        assert_eq!(packet.client_id, "legacy-client");
    }

    #[test]
    fn test_connect_with_user_properties() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "MQTT");
        raw.push(5);
        raw.push(0x02);
        raw.put_u16(60);
        // Properties: 1 user property (name="马龙", emoji="🛠️")
        let mut props = Vec::new();
        props.push(PROP_USER_PROPERTY);
        encode_utf8(&mut props, "name");
        encode_utf8(&mut props, "马龙");
        props.push(PROP_USER_PROPERTY);
        encode_utf8(&mut props, "emoji");
        encode_utf8(&mut props, "🛠️");

        let props_encoded = encode_remaining_length(props.len());
        raw.extend(&props_encoded);
        raw.extend(&props);
        encode_utf8(&mut raw, "skill-agent");

        let packet = decode_connect(&raw).unwrap();
        assert_eq!(packet.client_id, "skill-agent");
        assert_eq!(packet.properties.user_properties.len(), 2);
        assert_eq!(packet.properties.user_properties[0].key, "name");
        assert_eq!(packet.properties.user_properties[0].value, "马龙");
    }

    #[test]
    fn test_connect_with_username_password() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "MQTT");
        raw.push(5);
        raw.push(0xC0); // username + password flags
        raw.put_u16(60);
        raw.push(0x00); // empty properties
        encode_utf8(&mut raw, "test-client");
        encode_utf8(&mut raw, "myuser");
        encode_binary(&mut raw, b"mypass");

        let packet = decode_connect(&raw).unwrap();
        assert_eq!(packet.username.as_deref(), Some("myuser"));
        assert_eq!(packet.password.as_deref(), Some(&b"mypass"[..]));
    }

    #[test]
    fn test_publish_qos0() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "test/topic");
        raw.push(0x00); // empty v5 properties
        let payload = b"hello mqtt v5";
        raw.extend_from_slice(payload);

        let packet = decode_publish(&raw, 0x00).unwrap();
        assert_eq!(packet.topic, "test/topic");
        assert_eq!(packet.qos, 0);
        assert!(!packet.dup);
        assert!(!packet.retain);
        assert!(packet.packet_id.is_none());
        assert_eq!(packet.payload, payload);
    }

    #[test]
    fn test_publish_with_user_properties() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "agent-001/inbound");
        // Properties with 1 user property
        let mut props = Vec::new();
        props.push(PROP_USER_PROPERTY);
        encode_utf8(&mut props, "reply_to");
        encode_utf8(&mut props, "openclaw-malong/inbound");

        let props_encoded = encode_remaining_length(props.len());
        raw.extend(&props_encoded);
        raw.extend(&props);
        raw.extend_from_slice(b"{\"text\":\"hello\"}");

        let packet = decode_publish(&raw, 0x00).unwrap();
        assert_eq!(packet.topic, "agent-001/inbound");
        assert_eq!(packet.properties.user_properties.len(), 1);
        assert_eq!(packet.properties.user_properties[0].key, "reply_to");
        assert_eq!(packet.properties.user_properties[0].value, "openclaw-malong/inbound");
    }

    #[test]
    fn test_subscribe_single_topic() {
        let mut raw = Vec::new();
        raw.put_u16(1); // packet id
        raw.push(0x00); // empty properties
        encode_utf8(&mut raw, "test/topic");
        raw.push(0x01); // QoS 1

        let packet = decode_subscribe(&raw).unwrap();
        assert_eq!(packet.packet_id, 1);
        assert_eq!(packet.topic_filters.len(), 1);
        assert_eq!(packet.topic_filters[0].topic_filter, "test/topic");
        assert_eq!(packet.topic_filters[0].options.qos, 1);
    }

    #[test]
    fn test_subscribe_with_no_local() {
        let mut raw = Vec::new();
        raw.put_u16(42);
        raw.push(0x00);
        encode_utf8(&mut raw, "sensor/#");
        raw.push(0x06); // QoS 0 + NoLocal

        let packet = decode_subscribe(&raw).unwrap();
        assert!(packet.topic_filters[0].options.no_local);
    }

    #[test]
    fn test_unsubscribe() {
        let mut raw = Vec::new();
        raw.put_u16(7);
        raw.push(0x00);
        encode_utf8(&mut raw, "test/topic");
        encode_utf8(&mut raw, "other/#");

        let packet = decode_unsubscribe(&raw).unwrap();
        assert_eq!(packet.packet_id, 7);
        assert_eq!(packet.topic_filters.len(), 2);
    }

    #[test]
    fn test_disconnect_normal() {
        let packet = decode_disconnect(&[RC_NORMAL_DISCONNECTION]).unwrap();
        assert_eq!(packet.reason_code, RC_NORMAL_DISCONNECTION);
    }

    #[test]
    fn test_connack_success_v5() {
        let encoded = connack_v5(RC_SUCCESS, &Properties::new());
        let (decoded, _) = decode_packet(&encoded).unwrap();
        match decoded {
            Packet::Connack(p) => {
                assert_eq!(p.reason_code, RC_SUCCESS);
                assert!(!p.session_present);
            }
            _ => panic!("Expected Connack"),
        }
    }

    #[test]
    fn test_connack_v3() {
        let encoded = connack_v3(false, 0x00);
        assert_eq!(encoded, vec![CONNACK << 4, 0x02, 0x00, 0x00]);
    }

    #[test]
    fn test_encode_suback() {
        let suback = Packet::Suback(SubackPacket {
            packet_id: 5,
            properties: Properties::new(),
            reason_codes: vec![RC_GRANTED_QOS_0, RC_GRANTED_QOS_1],
        });
        let encoded = suback.encode();
        let (decoded, _) = decode_packet(&encoded).unwrap();
        match decoded {
            Packet::Suback(p) => {
                assert_eq!(p.packet_id, 5);
                assert_eq!(p.reason_codes, vec![0x00, 0x01]);
            }
            _ => panic!("Expected Suback"),
        }
    }

    #[test]
    fn test_full_packet_roundtrip() {
        // Build a CONNECT -> encode -> decode
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "MQTT");
        raw.push(5);
        raw.push(0x02);
        raw.put_u16(60);
        raw.push(0x00); // empty properties
        encode_utf8(&mut raw, "roundtrip-test");

        // Wrap in fixed header
        let rl = encode_remaining_length(raw.len());
        let mut frame = Vec::with_capacity(1 + rl.len() + raw.len());
        frame.push(CONNECT << 4);
        frame.extend(rl);
        frame.extend(raw);

        let (packet, _) = decode_packet(&frame).unwrap();
        match packet {
            Packet::Connect(p) => {
                assert_eq!(p.client_id, "roundtrip-test");
                assert_eq!(p.protocol_version, 5);
            }
            _ => panic!("Expected Connect"),
        }
    }

    #[test]
    fn test_invalid_protocol_name() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "MQIsdp"); // v3.1 name, not supported
        let result = decode_connect(&raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_client_id_clean_start() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "MQTT");
        raw.push(5);
        raw.push(0x02); // clean start
        raw.put_u16(60);
        raw.push(0x00); // empty properties
        encode_utf8(&mut raw, ""); // empty client id
        let packet = decode_connect(&raw).unwrap();
        assert_eq!(packet.client_id, "");
        assert!(packet.clean_start);
    }

    #[test]
    fn test_multiple_user_properties() {
        let mut raw = Vec::new();
        encode_utf8(&mut raw, "test/topic");

        let mut props = Vec::new();
        for pair in &[("a", "1"), ("b", "2"), ("c", "3")] {
            props.push(PROP_USER_PROPERTY);
            encode_utf8(&mut props, pair.0);
            encode_utf8(&mut props, pair.1);
        }
        let props_encoded = encode_remaining_length(props.len());
        raw.extend(&props_encoded);
        raw.extend(&props);
        raw.extend_from_slice(b"payload");

        let packet = decode_publish(&raw, 0x00).unwrap();
        assert_eq!(packet.properties.user_properties.len(), 3);
        assert_eq!(packet.properties.user_properties[2].key, "c");
    }
}
