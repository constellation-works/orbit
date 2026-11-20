//! Standing drain phase 1 (ORB-25965): how long work waited with no drain.
//!
//! The measure is the time inside a look-back window when at least one task
//! was waiting (`backlog`, which a drain admits, or `proposed`, which a drain
//! pilots and may approve) while none of this workspace's auto drains held an
//! admission window. It is rebuilt on every read from two durable records:
//! each task's status history and each drain run's start, deadline,
//! admissions stop, cancel and end. Nothing is sampled or stored.
//!
//! Both statuses count, so the figure is a ceiling on admissible work: a
//! backlog task whose dependencies are unmet, or a proposed task the pilot
//! would refuse, still counts as waiting.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};

use crate::OrbitRuntime;
use crate::application::job::DrainAdmissionWindow;

type Span = (DateTime<Utc>, DateTime<Utc>);

/// One UTC day of [`IdleWithWork`].
#[derive(Debug, Clone, PartialEq)]
pub struct IdleWithWorkDay {
    pub day: NaiveDate,
    /// Hours with at least one task waiting and no admission window.
    pub idle_hours: f64,
    /// The same time summed per waiting task.
    pub idle_task_hours: f64,
    /// Distinct tasks that waited during those hours.
    pub idle_tasks: usize,
}

/// Idle-with-work time over the last `days` UTC days, today included.
#[derive(Debug, Clone, PartialEq)]
pub struct IdleWithWork {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub idle_hours: f64,
    pub idle_task_hours: f64,
    pub idle_tasks: usize,
    /// Hours an admission window was open.
    pub window_hours: f64,
    /// Oldest first.
    pub days: Vec<IdleWithWorkDay>,
    /// Tasks whose history could not be read; they are left out.
    pub unreadable_histories: usize,
}

impl OrbitRuntime {
    /// Measure idle-with-work time over the last `days` UTC days (at least
    /// one), up to now. Read-only.
    pub fn drain_idle_with_work(&self, days: u32) -> Result<IdleWithWork, OrbitError> {
        let now = Utc::now();
        let days = i64::from(days.max(1));
        let since = (now.date_naive() - Duration::days(days - 1))
            .and_hms_opt(0, 0, 0)
            .map_or(now, |midnight| midnight.and_utc());
        let windows = merge(
            self.drain_admission_windows_since(since, now)?
                .into_iter()
                .map(
                    |DrainAdmissionWindow {
                         opened_at,
                         closed_at,
                     }| (opened_at, closed_at),
                )
                .filter_map(|span| clip(span, (since, now)))
                .collect(),
        );
        let mut unreadable_histories = 0;
        // Each task's waiting time that fell outside every admission window.
        let mut idle_by_task: Vec<(String, Vec<Span>)> = Vec::new();
        for task in self.list_task_metadata()? {
            if !is_waiting(task.status) && task.updated_at < since {
                continue;
            }
            let id = task.id.to_string();
            let history = match self.get_task_history(&id) {
                Ok(history) => history,
                Err(error) => {
                    tracing::debug!(task_id = %id, %error, "idle-with-work: history unreadable");
                    unreadable_histories += 1;
                    continue;
                }
            };
            let idle: Vec<Span> = waiting_spans(&task, &history, now)
                .into_iter()
                .filter_map(|span| clip(span, (since, now)))
                .flat_map(|span| subtract(span, &windows))
                .collect();
            if !idle.is_empty() {
                idle_by_task.push((id, idle));
            }
        }
        let mut days_out = Vec::new();
        let mut day = since.date_naive();
        while day <= now.date_naive() {
            let start = day.and_hms_opt(0, 0, 0).map_or(since, |at| at.and_utc());
            let bounds = (start, (start + Duration::days(1)).min(now));
            days_out.push(summarize(day, bounds, &idle_by_task));
            day = day.succ_opt().unwrap_or(day + Duration::days(1));
        }
        let whole = summarize(since.date_naive(), (since, now), &idle_by_task);
        Ok(IdleWithWork {
            since,
            until: now,
            idle_hours: whole.idle_hours,
            idle_task_hours: whole.idle_task_hours,
            idle_tasks: whole.idle_tasks,
            window_hours: hours(windows.iter().map(|(a, b)| *b - *a).sum()),
            days: days_out,
            unreadable_histories,
        })
    }
}

fn is_waiting(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Backlog | TaskStatus::Proposed)
}

/// When `task` was `backlog` or `proposed`, from its recorded status
/// changes. A task with none has held its current status since creation; a
/// history whose first change is not a creation starts from that change's
/// `from_status` at creation time.
fn waiting_spans(task: &Task, history: &[TaskHistoryEntry], now: DateTime<Utc>) -> Vec<Span> {
    let mut transitions = history
        .iter()
        .filter_map(|entry| entry.to_status.map(|to| (entry, to)))
        .peekable();
    let mut current: Option<(TaskStatus, DateTime<Utc>)> = match transitions.peek() {
        None => Some((task.status, task.created_at)),
        Some((entry, _)) => entry
            .from_status
            .map(|from| (from, task.created_at.min(entry.at))),
    };
    let mut spans = Vec::new();
    for (entry, to) in transitions {
        if let Some((status, from)) = current
            && is_waiting(status)
            && entry.at > from
        {
            spans.push((from, entry.at));
        }
        current = Some((to, entry.at));
    }
    if let Some((status, from)) = current
        && is_waiting(status)
        && now > from
    {
        spans.push((from, now));
    }
    spans
}

fn summarize(
    day: NaiveDate,
    bounds: Span,
    idle_by_task: &[(String, Vec<Span>)],
) -> IdleWithWorkDay {
    let mut all = Vec::new();
    let mut task_time = Duration::zero();
    let mut tasks = 0;
    for (_, spans) in idle_by_task {
        let clipped: Vec<Span> = spans
            .iter()
            .filter_map(|span| clip(*span, bounds))
            .collect();
        if clipped.is_empty() {
            continue;
        }
        tasks += 1;
        task_time += clipped.iter().map(|(a, b)| *b - *a).sum();
        all.extend(clipped);
    }
    let union = merge(all);
    IdleWithWorkDay {
        day,
        idle_hours: hours(union.iter().map(|(a, b)| *b - *a).sum()),
        idle_task_hours: hours(task_time),
        idle_tasks: tasks,
    }
}

fn hours(duration: Duration) -> f64 {
    duration.num_seconds() as f64 / 3600.0
}

fn clip(span: Span, bounds: Span) -> Option<Span> {
    let start = span.0.max(bounds.0);
    let end = span.1.min(bounds.1);
    (end > start).then_some((start, end))
}

/// Sorted, non-overlapping union of `spans`.
fn merge(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort();
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// `span` minus the sorted, non-overlapping `holes`.
fn subtract(span: Span, holes: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut cursor = span.0;
    for &(start, end) in holes {
        if end <= cursor {
            continue;
        }
        if start >= span.1 {
            break;
        }
        if start > cursor {
            out.push((cursor, start));
        }
        cursor = cursor.max(end);
        if cursor >= span.1 {
            return out;
        }
    }
    if span.1 > cursor {
        out.push((cursor, span.1));
    }
    out
}

/// Days the doctor row and the perf figure cover.
pub const IDLE_WITH_WORK_DAYS: u32 = 7;
