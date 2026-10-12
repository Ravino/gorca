//! Supervisor primitives (ADR-0001 section 6.5): every spawned task has an owner
//! ([`TaskSet`]) and a cancellation path ([`CancellationToken`]).

use std::collections::HashMap;
use std::future::Future;
use std::time::{Duration, Instant};

use tokio::task::{Id, JoinError, JoinSet};
use tokio_util::sync::CancellationToken;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TaskExit {
    Completed,
    Cancelled,
    Panicked,
}

impl TaskExit {
    fn from_join<T>(r: &Result<T, JoinError>) -> Self {
        match r {
            Ok(_) => TaskExit::Completed,
            Err(e) if e.is_panic() => TaskExit::Panicked,
            Err(_) => TaskExit::Cancelled,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TaskReport {
    pub name: &'static str,
    pub exit: TaskExit,
}

/// Owner of a group of tasks. Dropping it aborts the tasks (`JoinSet` semantics).
#[derive(Debug)]
pub struct TaskSet {
    set: JoinSet<()>,
    names: HashMap<Id, &'static str>,
    cancel: CancellationToken,
}

impl TaskSet {
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            set: JoinSet::new(),
            names: HashMap::new(),
            cancel,
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Spawn a task that ends when it completes or when the token is cancelled.
    pub fn spawn<F>(&mut self, name: &'static str, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let token = self.cancel.clone();
        let handle = self.set.spawn(async move {
            tokio::select! {
                biased;
                () = token.cancelled() => {}
                () = fut => {}
            }
        });
        self.names.insert(handle.id(), name);
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Next finished task, or `None` when the set is empty.
    pub async fn join_next(&mut self) -> Option<TaskReport> {
        let (id, res) = match self.set.join_next_with_id().await? {
            Ok((id, ())) => (id, Ok(())),
            Err(e) => (e.id(), Err(e)),
        };
        let name = self.names.remove(&id).unwrap_or("<unknown>");
        Some(TaskReport {
            name,
            exit: TaskExit::from_join(&res),
        })
    }

    /// Cancel, wait up to `grace` for tasks to finish, then abort the stragglers.
    pub async fn shutdown(&mut self, grace: Duration) -> Vec<TaskReport> {
        self.cancel.cancel();
        let mut reports = Vec::new();
        let drain = async {
            while let Some(r) = self.join_next().await {
                reports.push(r);
            }
        };
        if tokio::time::timeout(grace, drain).await.is_err() {
            self.set.abort_all();
            while let Some(r) = self.join_next().await {
                reports.push(r);
            }
        }
        reports
    }
}

/// Crash-loop policy: more than `max_restarts` in `window` quarantines the scope (6.5, 10).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub window: Duration,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RestartDecision {
    Restart,
    Quarantine,
}

#[derive(Debug)]
pub struct RestartTracker {
    policy: RestartPolicy,
    recent: Vec<Instant>,
}

impl RestartTracker {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            recent: Vec::new(),
        }
    }

    /// Record a failure at `now` and decide.
    pub fn record(&mut self, now: Instant) -> RestartDecision {
        let window = self.policy.window;
        self.recent
            .retain(|t| now.saturating_duration_since(*t) < window);
        self.recent.push(now);
        let count = u32::try_from(self.recent.len()).unwrap_or(u32::MAX);
        if count > self.policy.max_restarts {
            RestartDecision::Quarantine
        } else {
            RestartDecision::Restart
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_resets_in_a_minute_quarantine_on_the_fourth() {
        let mut t = RestartTracker::new(RestartPolicy {
            max_restarts: 3,
            window: Duration::from_secs(60),
        });
        let t0 = Instant::now();
        for i in 0..3 {
            assert_eq!(
                t.record(t0 + Duration::from_secs(i)),
                RestartDecision::Restart
            );
        }
        assert_eq!(
            t.record(t0 + Duration::from_secs(4)),
            RestartDecision::Quarantine
        );
        // Old failures age out of the window.
        assert_eq!(
            t.record(t0 + Duration::from_secs(200)),
            RestartDecision::Restart
        );
    }

    #[tokio::test]
    async fn completed_and_panicked_tasks_are_reported_with_names() {
        let mut set = TaskSet::new(CancellationToken::new());
        set.spawn("ok", async {});
        set.spawn("bad", async { panic!("boom") });
        let mut got = [
            set.join_next().await.unwrap(),
            set.join_next().await.unwrap(),
        ];
        got.sort_by_key(|r| r.name);
        assert_eq!(
            got[0],
            TaskReport {
                name: "bad",
                exit: TaskExit::Panicked
            }
        );
        assert_eq!(
            got[1],
            TaskReport {
                name: "ok",
                exit: TaskExit::Completed
            }
        );
        assert!(set.join_next().await.is_none());
    }

    #[tokio::test]
    async fn shutdown_cancels_hung_tasks() {
        let mut set = TaskSet::new(CancellationToken::new());
        set.spawn("hung", std::future::pending());
        let reports = set.shutdown(Duration::from_secs(1)).await;
        assert_eq!(
            reports,
            vec![TaskReport {
                name: "hung",
                exit: TaskExit::Completed
            }]
        );
        assert!(set.is_empty());
    }
}
