//! Ownership of host file operations, independent of the SDK's RPC bookkeeping.
//!
//! Dropping an async future does NOT cancel Tokio's queued blocking closure.
//! A job therefore owns a reservation, a local cancellation flag and an abort
//! handle; its blocking closure rechecks connection/session/RPC cancellation.
//! Once the final check admits a synchronous operation, it is in progress and
//! may finish despite cancellation. We do not claim to undo an OS file write.
use agent_client_protocol::{Error, RequestCancellation};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::sync::{watch, Semaphore};

pub(super) const MAX_FILE_JOBS: usize = 32;
const MAX_QUEUED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct State {
    jobs: usize,
    bytes: usize,
    // Each replacement is a NEW epoch. Old cancelled work must never revive
    // when the next prompt starts on the same SID.
    sessions: HashMap<String, watch::Sender<bool>>,
}

pub(super) struct FileTasks {
    state: Mutex<State>,
    closed: watch::Sender<bool>,
    // Serialize file IO on this connection, including aliased paths. Waiting
    // happens in spawned SDK tasks, never in the message-dispatch callback.
    lane: Arc<Semaphore>,
}

impl FileTasks {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            closed: watch::channel(false).0,
            lane: Arc::new(Semaphore::new(1)),
        })
    }

    pub fn reserve(self: &Arc<Self>, sid: &str, bytes: usize) -> Result<FileJob, Error> {
        let mut state = self.state.lock().unwrap();
        if *self.closed.borrow() {
            return Err(Error::request_cancelled());
        }
        if state.sessions.get(sid).is_some_and(|epoch| *epoch.borrow()) {
            return Err(Error::request_cancelled());
        }
        let total = state.bytes.checked_add(bytes).ok_or_else(busy)?;
        if state.jobs >= MAX_FILE_JOBS || total > MAX_QUEUED_BYTES {
            return Err(busy());
        }
        let session = state
            .sessions
            .entry(sid.into())
            .or_insert_with(|| watch::channel(false).0)
            .clone();
        state.jobs += 1;
        state.bytes = total;
        Ok(FileJob {
            scope: self.clone(),
            session,
            bytes,
        })
    }

    pub fn cancel_session(&self, sid: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .sessions
            .entry(sid.into())
            .or_insert_with(|| watch::channel(false).0)
            .send_replace(true);
    }

    /// Called only after the host accepted a new turn (not on a failed begin).
    pub fn begin_session(&self, sid: &str) {
        let mut state = self.state.lock().unwrap();
        if state.sessions.get(sid).is_some_and(|epoch| *epoch.borrow()) {
            state.sessions.insert(sid.into(), watch::channel(false).0);
        }
    }

    pub fn close(&self) {
        // Registration, admission to synchronous IO, and close are ordered by
        // this lock. Never hold it across an actual OS file operation.
        let _state = self.state.lock().unwrap();
        self.closed.send_replace(true);
        self.lane.close();
    }
}

fn busy() -> Error {
    Error::internal_error().data("宿主文件任务队列已满，请稍后重试")
}

pub(super) struct FileJob {
    scope: Arc<FileTasks>,
    session: watch::Sender<bool>,
    bytes: usize,
}

impl Drop for FileJob {
    fn drop(&mut self) {
        let mut state = self.scope.state.lock().unwrap();
        state.jobs -= 1;
        state.bytes -= self.bytes;
    }
}

struct AbortBlocking {
    cancelled: Arc<AtomicBool>,
    handle: tokio::task::AbortHandle,
}
impl Drop for AbortBlocking {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // Abort can remove work that hasn't started; the flag/check is still
        // required because neither dropping JoinHandle nor abort alone suffices.
        self.handle.abort();
    }
}

impl FileJob {
    fn check(&self, peer: &RequestCancellation, dropped: &AtomicBool) -> Result<(), Error> {
        let _state = self.scope.state.lock().unwrap();
        if *self.scope.closed.borrow()
            || *self.session.borrow()
            || peer.is_cancelled()
            || dropped.load(Ordering::Acquire)
        {
            Err(Error::request_cancelled())
        } else {
            Ok(())
        }
    }

    pub async fn run<T, F>(self, peer: RequestCancellation, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, String> + Send + 'static,
    {
        let dropped = Arc::new(AtomicBool::new(false));
        self.check(&peer, &dropped)?;
        let mut closed = self.scope.closed.subscribe();
        let mut session = self.session.subscribe();
        // Subscribe first, then check: close/cancel between these operations is
        // observed either by the check or by changed(), never missed.
        self.check(&peer, &dropped)?;
        let lane = tokio::select! {
            biased;
            _ = closed.changed() => return Err(Error::request_cancelled()),
            _ = session.changed() => return Err(Error::request_cancelled()),
            _ = peer.cancelled() => return Err(Error::request_cancelled()),
            permit = self.scope.lane.clone().acquire_owned() => permit.map_err(|_| Error::request_cancelled())?,
        };
        let worker_peer = peer.clone();
        let worker_dropped = dropped.clone();
        let mut work = tokio::task::spawn_blocking(move || {
            // Keep reservation and serial lane until the closure actually exits,
            // even if its async waiter disappears while IO is in progress.
            let _lane = lane;
            let _job = self;
            _job.check(&worker_peer, &worker_dropped)?;
            operation().map_err(|error| Error::internal_error().data(error))
        });
        let _guard = AbortBlocking {
            cancelled: dropped,
            handle: work.abort_handle(),
        };
        tokio::select! {
            biased;
            result = &mut work => result.map_err(|e| {
                if e.is_cancelled() { Error::request_cancelled() }
                else { Error::internal_error().data(e.to_string()) }
            })?,
            _ = closed.changed() => Err(Error::request_cancelled()),
            _ = session.changed() => Err(Error::request_cancelled()),
            _ = peer.cancelled() => Err(Error::request_cancelled()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cancelled(result: Result<FileJob, Error>) -> bool {
        matches!(result, Err(e) if i32::from(e.code) == -32800)
    }
    #[test]
    fn close_rejects_registration_and_is_idempotent() {
        let scope = FileTasks::new();
        let job = scope.reserve("s", 8).unwrap();
        scope.close();
        scope.close();
        assert!(cancelled(scope.reserve("s", 0)));
        drop(job);
        assert_eq!(scope.state.lock().unwrap().jobs, 0);
    }
    #[test]
    fn accepted_new_turn_gets_new_epoch_without_reviving_old_job() {
        let scope = FileTasks::new();
        let old = scope.reserve("s", 8).unwrap();
        scope.cancel_session("s");
        assert!(*old.session.borrow());
        assert!(cancelled(scope.reserve("s", 0)));
        scope.begin_session("s");
        let new = scope.reserve("s", 9).unwrap();
        assert!(*old.session.borrow());
        assert!(!*new.session.borrow());
        scope.close();
    }
    #[test]
    fn count_budget_includes_queued_jobs_and_is_released() {
        let scope = FileTasks::new();
        let mut jobs = (0..MAX_FILE_JOBS)
            .map(|_| scope.reserve("s", 1).unwrap())
            .collect::<Vec<_>>();
        assert!(scope.reserve("s", 1).is_err());
        jobs.pop();
        let another = scope.reserve("s", 1).unwrap();
        drop(jobs);
        drop(another);
        let state = scope.state.lock().unwrap();
        assert_eq!((state.jobs, state.bytes), (0, 0));
    }
    #[test]
    fn byte_budget_and_overflow_are_checked_before_admission() {
        let scope = FileTasks::new();
        let job = scope.reserve("s", MAX_QUEUED_BYTES).unwrap();
        assert!(scope.reserve("s", 1).is_err());
        assert!(scope.reserve("s", usize::MAX).is_err());
        drop(job);
        assert!(scope.reserve("s", 1).is_ok());
    }
    #[test]
    fn cancelling_one_session_does_not_cancel_another() {
        let scope = FileTasks::new();
        let other = scope.reserve("B", 0).unwrap();
        scope.cancel_session("A");
        assert!(cancelled(scope.reserve("A", 0)));
        assert!(!*other.session.borrow());
        assert!(scope.reserve("B", 0).is_ok());
    }
}
