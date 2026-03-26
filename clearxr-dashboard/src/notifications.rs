//! Toast notification queue for the layer dashboard overlay.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NotificationLevel {
    Info,
    Warning,
    Success,
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub level: NotificationLevel,
    pub created: Instant,
    pub duration: Duration,
}

impl Notification {
    pub fn info(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            level: NotificationLevel::Info,
            created: Instant::now(),
            duration: Duration::from_secs(4),
        }
    }

    pub fn warning(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            level: NotificationLevel::Warning,
            created: Instant::now(),
            duration: Duration::from_secs(6),
        }
    }

    pub fn success(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            level: NotificationLevel::Success,
            created: Instant::now(),
            duration: Duration::from_secs(3),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created.elapsed() >= self.duration
    }
}

pub struct NotificationQueue {
    active: Vec<Notification>,
    max_visible: usize,
}

impl NotificationQueue {
    pub fn new(max_visible: usize) -> Self {
        Self {
            active: Vec::new(),
            max_visible,
        }
    }

    pub fn push(&mut self, notification: Notification) {
        self.active.push(notification);
        while self.active.len() > self.max_visible {
            self.active.remove(0);
        }
    }

    pub fn tick(&mut self) {
        self.active.retain(|n| !n.is_expired());
    }

    pub fn visible(&self) -> &[Notification] {
        &self.active
    }
}
