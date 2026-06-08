//! Integration tests for `broker::management`.
//!
//! These exercise the in-process channel end-to-end: a fake
//! "broker state" is built, a handle is requested, requests
//! are sent, replies are awaited, and the payloads are
//! compared. The point is to catch regressions in the
//! oneshot plumbing (the kind of bug that the type system
//! can't see, e.g. dropping the reply sender before the
//! caller awaits it).

use broker::management::management_pair;
use broker::state::{
    create_shared_state, ClientInfo, MqttProtocol, QoS, Subscription,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Instant;
use tokio::time::Duration;

fn fake_client(id: &str) -> ClientInfo {
    ClientInfo {
        client_id: id.into(),
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
        protocol_version: MqttProtocol::V311,
        connected_at: Instant::now(),
        clean_session: true,
        keep_alive: 60,
        username: Some("alice".into()),
    }
}

fn fake_sub(client_id: &str, topic: &str) -> Subscription {
    Subscription {
        client_id: client_id.into(),
        topic_filter: topic.into(),
        qos: QoS::AtLeastOnce,
        no_local: false,
        retain_as_published: false,
        retain_handling: 0,
    }
}

#[tokio::test]
async fn list_clients_returns_all_connected() {
    let state = create_shared_state();
    {
        let mut s = state.write().await;
        s.clients.insert("a".into(), fake_client("a"));
        s.clients.insert("b".into(), fake_client("b"));
    }
    let (mgmt, task) = management_pair(state);
    tokio::spawn(task);

    let clients = mgmt.list_clients().await;
    let ids: Vec<&str> = clients.iter().map(|c| c.client_id.as_str()).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"a"));
    assert!(ids.contains(&"b"));
}

#[tokio::test]
async fn get_client_returns_some_for_known_id() {
    let state = create_shared_state();
    state.write().await.clients.insert("a".into(), fake_client("a"));
    let (mgmt, task) = management_pair(state);
    tokio::spawn(task);

    let c = mgmt.get_client("a").await.expect("client a exists");
    assert_eq!(c.client_id, "a");
    assert_eq!(c.username.as_deref(), Some("alice"));
}

#[tokio::test]
async fn get_client_returns_none_for_unknown_id() {
    let state = create_shared_state();
    let (mgmt, task) = management_pair(state);
    tokio::spawn(task);

    assert!(mgmt.get_client("ghost").await.is_none());
}

#[tokio::test]
async fn list_subscriptions_returns_topics_with_subscriber_counts() {
    let state = create_shared_state();
    {
        let mut s = state.write().await;
        s.subscriptions
            .entry("sensors/temp".into())
            .or_default()
            .push(fake_sub("a", "sensors/temp"));
        s.subscriptions
            .entry("sensors/temp".into())
            .or_default()
            .push(fake_sub("b", "sensors/temp"));
        s.subscriptions
            .entry("actuators/light".into())
            .or_default()
            .push(fake_sub("a", "actuators/light"));
    }
    let (mgmt, task) = management_pair(state);
    tokio::spawn(task);

    let topics = mgmt.list_subscriptions().await;
    assert_eq!(topics.len(), 2);
    let temp = topics
        .iter()
        .find(|t| t.topic == "sensors/temp")
        .expect("sensors/temp present");
    assert_eq!(temp.subscribers.len(), 2);
    let light = topics
        .iter()
        .find(|t| t.topic == "actuators/light")
        .expect("actuators/light present");
    assert_eq!(light.subscribers.len(), 1);
}

#[tokio::test]
async fn get_topic_subscribers_returns_empty_for_unknown_topic() {
    let state = create_shared_state();
    let (mgmt, task) = management_pair(state);
    tokio::spawn(task);

    let subs = mgmt.get_topic_subscribers("never/subscribed").await;
    assert!(subs.is_empty());
}

#[tokio::test]
async fn get_topic_subscribers_returns_only_matching_topic() {
    let state = create_shared_state();
    {
        let mut s = state.write().await;
        s.subscriptions
            .entry("foo".into())
            .or_default()
            .push(fake_sub("a", "foo"));
        s.subscriptions
            .entry("bar".into())
            .or_default()
            .push(fake_sub("b", "bar"));
    }
    let (mgmt, task) = management_pair(state);
    tokio::spawn(task);

    let subs = mgmt.get_topic_subscribers("foo").await;
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].client_id, "a");
}

/// Multiple concurrent requests must all be answered. The
/// mpsc/oneshot plumbing is single-producer, but the
/// management task is the only consumer, so a few in-flight
/// requests at once is the realistic case (the LLM
/// parallelises tool calls).
#[tokio::test]
async fn many_concurrent_list_clients_calls_all_succeed() {
    let state = create_shared_state();
    state.write().await.clients.insert("a".into(), fake_client("a"));
    let (mgmt, task) = management_pair(state);
    tokio::spawn(task);

    let mut handles = Vec::new();
    for _ in 0..20 {
        let m = mgmt.clone();
        handles.push(tokio::spawn(async move { m.list_clients().await }));
    }
    for h in handles {
        let clients = h.await.expect("task didn't panic");
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].client_id, "a");
    }
}
