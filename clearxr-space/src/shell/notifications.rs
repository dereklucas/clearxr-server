//! Toast notification queue for the VR shell.
//!
//! Notifications appear briefly near the user's head, then auto-dismiss.

use std::time::{Duration, Instant};

/// Priority level affects display duration and position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NotificationLevel {
    /// Informational (4 second display).
    Info,
    /// Warning (6 second display).
    Warning,
    /// Success confirmation (3 second display).
    Success,
}

/// A queued notification.
#[derive(Clone, Debug)]
pub struct Notification {
    /// Notification headline text.
    pub title: String,
    /// Optional body text (may be empty).
    pub body: String,
    /// Severity / display style.
    pub level: NotificationLevel,
    /// When this notification was created.
    pub created: Instant,
    /// How long to display before auto-dismissing.
    pub duration: Duration,
}

impl Notification {
    /// Create an info-level notification (4 second display).
    pub fn info(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            level: NotificationLevel::Info,
            created: Instant::now(),
            duration: Duration::from_secs(4),
        }
    }

    /// Create a warning-level notification (6 second display).
    pub fn warning(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            level: NotificationLevel::Warning,
            created: Instant::now(),
            duration: Duration::from_secs(6),
        }
    }

    /// Create a success-level notification (3 second display).
    pub fn success(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            level: NotificationLevel::Success,
            created: Instant::now(),
            duration: Duration::from_secs(3),
        }
    }

    /// Returns 0.0..1.0 progress through the notification's lifetime.
    #[allow(dead_code)] // Used in tests; will be used for fade-out animation
    pub fn progress(&self) -> f32 {
        let elapsed = self.created.elapsed().as_secs_f32();
        let total = self.duration.as_secs_f32();
        (elapsed / total).min(1.0)
    }

    /// Whether the notification has expired.
    pub fn is_expired(&self) -> bool {
        self.created.elapsed() >= self.duration
    }
}

/// Manages a queue of active notifications.
pub struct NotificationQueue {
    active: Vec<Notification>,
    max_visible: usize,
}

impl NotificationQueue {
    /// Create a new queue that displays at most `max_visible` notifications at once.
    pub fn new(max_visible: usize) -> Self {
        Self {
            active: Vec::new(),
            max_visible,
        }
    }

    /// Add a notification to the queue.
    pub fn push(&mut self, notification: Notification) {
        self.active.push(notification);
        // Trim if over max (remove oldest)
        while self.active.len() > self.max_visible {
            self.active.remove(0);
        }
    }

    /// Remove expired notifications. Called each frame.
    pub fn tick(&mut self) {
        self.active.retain(|n| !n.is_expired());
    }

    /// Currently visible notifications.
    pub fn visible(&self) -> &[Notification] {
        &self.active
    }

    /// Number of active notifications.
    pub fn count(&self) -> usize {
        self.active.len()
    }

    /// Clear all notifications.
    #[allow(dead_code)] // Used in tests; public API
    pub fn clear(&mut self) {
        self.active.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn notification_info_defaults() {
        let n = Notification::info("Test", "Body");
        assert_eq!(n.level, NotificationLevel::Info);
        assert_eq!(n.duration, Duration::from_secs(4));
        assert!(!n.is_expired());
    }

    #[test]
    fn notification_progress() {
        let n = Notification::info("Test", "Body");
        assert!(n.progress() < 0.1); // Just created
    }

    #[test]
    fn notification_expires() {
        let mut n = Notification::info("Test", "Body");
        n.duration = Duration::from_millis(10);
        thread::sleep(Duration::from_millis(15));
        assert!(n.is_expired());
        assert!((n.progress() - 1.0).abs() < 0.01);
    }

    #[test]
    fn queue_push_and_tick() {
        let mut q = NotificationQueue::new(3);
        q.push(Notification::info("A", ""));
        q.push(Notification::info("B", ""));
        assert_eq!(q.count(), 2);

        // Not expired yet
        q.tick();
        assert_eq!(q.count(), 2);
    }

    #[test]
    fn queue_max_visible() {
        let mut q = NotificationQueue::new(2);
        q.push(Notification::info("A", ""));
        q.push(Notification::info("B", ""));
        q.push(Notification::info("C", ""));
        assert_eq!(q.count(), 2);
        // Oldest (A) should have been removed
        assert_eq!(q.visible()[0].title, "B");
    }

    #[test]
    fn queue_tick_removes_expired() {
        let mut q = NotificationQueue::new(5);
        let mut n = Notification::info("Expire", "");
        n.duration = Duration::from_millis(10);
        q.push(n);
        q.push(Notification::info("Stay", ""));

        thread::sleep(Duration::from_millis(15));
        q.tick();

        assert_eq!(q.count(), 1);
        assert_eq!(q.visible()[0].title, "Stay");
    }

    #[test]
    fn queue_clear() {
        let mut q = NotificationQueue::new(5);
        q.push(Notification::info("A", ""));
        q.push(Notification::warning("B", ""));
        q.clear();
        assert_eq!(q.count(), 0);
    }

    #[test]
    fn notification_warning_longer_than_info() {
        let info = Notification::info("A", "");
        let warn = Notification::warning("B", "");
        assert!(warn.duration > info.duration);
    }

    #[test]
    fn notification_success_shortest() {
        let success = Notification::success("A", "");
        let info = Notification::info("B", "");
        assert!(success.duration < info.duration);
    }

    #[test]
    fn queue_empty_tick() {
        let mut q = NotificationQueue::new(3);
        q.tick(); // Should not crash on empty queue
        assert_eq!(q.count(), 0);
    }

    #[test]
    fn queue_max_visible_one() {
        let mut q = NotificationQueue::new(1);
        q.push(Notification::info("A", ""));
        q.push(Notification::info("B", ""));
        assert_eq!(q.count(), 1);
        assert_eq!(q.visible()[0].title, "B");
    }

    #[test]
    fn notification_fifo_order() {
        let mut q = NotificationQueue::new(10);
        q.push(Notification::info("First", ""));
        q.push(Notification::info("Second", ""));
        q.push(Notification::info("Third", ""));
        let vis = q.visible();
        assert_eq!(vis[0].title, "First");
        assert_eq!(vis[1].title, "Second");
        assert_eq!(vis[2].title, "Third");
    }

    #[test]
    fn notification_body_preserved() {
        let n = Notification::info("Title", "Some body text");
        assert_eq!(n.title, "Title");
        assert_eq!(n.body, "Some body text");
    }

    #[test]
    fn notification_levels_distinct() {
        let info = Notification::info("", "");
        let warn = Notification::warning("", "");
        let success = Notification::success("", "");
        assert_ne!(info.level, warn.level);
        assert_ne!(info.level, success.level);
        assert_ne!(warn.level, success.level);
    }

    #[test]
    fn notification_not_expired_immediately() {
        let n = Notification::info("Test", "Body");
        assert!(!n.is_expired());
        let w = Notification::warning("Test", "Body");
        assert!(!w.is_expired());
        let s = Notification::success("Test", "Body");
        assert!(!s.is_expired());
    }

    #[test]
    fn queue_visible_returns_all_active() {
        let mut q = NotificationQueue::new(5);
        q.push(Notification::info("A", "body1"));
        q.push(Notification::warning("B", "body2"));
        q.push(Notification::success("C", "body3"));
        let vis = q.visible();
        assert_eq!(vis.len(), 3);
        assert_eq!(vis[0].body, "body1");
        assert_eq!(vis[1].body, "body2");
        assert_eq!(vis[2].body, "body3");
    }
}
