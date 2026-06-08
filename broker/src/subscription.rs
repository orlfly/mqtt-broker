use crate::state::{Subscription, SharedBrokerState};

pub struct SubscriptionTree {
    state: SharedBrokerState,
}

impl SubscriptionTree {
    pub fn new(state: SharedBrokerState) -> Self {
        Self { state }
    }

    pub async fn add(&self, subscription: Subscription) {
        let mut subs = self.state.write().await;
        // Remove existing subscription for same client+topic before adding
        if let Some(subscribers) = subs.subscriptions.get_mut(&subscription.topic_filter) {
            subscribers.retain(|s| s.client_id != subscription.client_id);
        }
        subs.subscriptions
            .entry(subscription.topic_filter.clone())
            .or_default()
            .push(subscription);
    }

    pub async fn remove(&self, client_id: &str, topic_filter: &str) {
        let mut subs = self.state.write().await;
        if let Some(subscribers) = subs.subscriptions.get_mut(topic_filter) {
            subscribers.retain(|s| s.client_id != client_id);
            if subscribers.is_empty() {
                subs.subscriptions.remove(topic_filter);
            }
        }
    }

    pub async fn remove_client_subscriptions(&self, client_id: &str) {
        let mut subs = self.state.write().await;
        subs.subscriptions.retain(|_, subscribers| {
            subscribers.retain(|s| s.client_id != client_id);
            !subscribers.is_empty()
        });
    }

    fn topic_matches(topic: &str, filter: &str) -> bool {
        if topic == filter {
            return true;
        }

        let topic_parts: Vec<&str> = topic.split('/').collect();
        let filter_parts: Vec<&str> = filter.split('/').collect();

        let mut ti = 0;
        let mut fi = 0;

        while fi < filter_parts.len() {
            if filter_parts[fi] == "#" {
                return true;
            }

            if ti >= topic_parts.len() {
                return false;
            }

            if filter_parts[fi] == "+" || filter_parts[fi] == topic_parts[ti] {
                ti += 1;
                fi += 1;
            } else {
                return false;
            }
        }

        ti == topic_parts.len()
    }

    pub async fn match_topic(&self, topic: &str) -> Vec<Subscription> {
        let subs = self.state.read().await;
        let mut matched = Vec::new();

        for (filter, subscribers) in subs.subscriptions.iter() {
            if Self::topic_matches(topic, filter) {
                matched.extend(subscribers.iter().cloned());
            }
        }

        matched
    }

    pub fn matches_filter(topic: &str, filter: &str) -> bool {
        Self::topic_matches(topic, filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_matching_exact() {
        assert!(SubscriptionTree::matches_filter("sensor/temp", "sensor/temp"));
        assert!(!SubscriptionTree::matches_filter("sensor/temp", "sensor/humidity"));
    }

    #[test]
    fn test_topic_matching_single_level_wildcard() {
        assert!(SubscriptionTree::matches_filter("sensor/temp", "sensor/+"));
        assert!(SubscriptionTree::matches_filter("sensor/humidity", "sensor/+"));
        assert!(!SubscriptionTree::matches_filter("sensor/floor1/temp", "sensor/+"));
    }

    #[test]
    fn test_topic_matching_multi_level_wildcard() {
        assert!(SubscriptionTree::matches_filter("sensor/temp", "sensor/#"));
        assert!(SubscriptionTree::matches_filter("sensor/floor1/temp", "sensor/#"));
        assert!(SubscriptionTree::matches_filter("sensor", "sensor/#"));
    }
}
