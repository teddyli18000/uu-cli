use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRequest {
    pub id: String,
    pub device_identity: String,
    pub remote_root: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub worker_id: String,
    pub job: JobRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerState {
    Idle,
    Leased { job_id: String },
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    pub id: String,
    pub device_identity: String,
    pub state: WorkerState,
}

#[derive(Debug, Default)]
pub struct Scheduler {
    workers: HashMap<String, Worker>,
    queues: HashMap<String, VecDeque<JobRequest>>,
}

impl Scheduler {
    pub fn register_worker(&mut self, id: String, device_identity: String) {
        self.workers.entry(id.clone()).or_insert(Worker {
            id,
            device_identity,
            state: WorkerState::Idle,
        });
    }

    pub fn worker(&self, id: &str) -> Option<&Worker> {
        self.workers.get(id)
    }

    pub fn submit(&mut self, job: JobRequest) -> Option<Lease> {
        if let Some(worker_id) = self.idle_worker_for(&job.device_identity) {
            self.lease(worker_id, job).ok()
        } else {
            self.queues
                .entry(job.device_identity.clone())
                .or_default()
                .push_back(job);
            None
        }
    }

    /// Finish a job that is known to have completed. If work is queued for the
    /// same device, the worker is immediately leased to the oldest queued job.
    pub fn finish(
        &mut self,
        worker_id: &str,
        job_id: &str,
    ) -> Result<Option<Lease>, ScheduleError> {
        self.release_known_job(worker_id, job_id, WorkerState::Idle)?;
        self.dispatch_next(worker_id)
    }

    /// Transport loss or broker crash must never make an uncertain PTY reusable.
    /// The worker remains quarantined until another component can prove it safe.
    pub fn mark_unknown(&mut self, worker_id: &str, job_id: &str) -> Result<(), ScheduleError> {
        self.release_known_job(worker_id, job_id, WorkerState::Quarantined)
    }

    /// A recovery probe may explicitly return a quarantined worker to the pool.
    /// This method must only be called after the transport layer has proven that
    /// no prior job can still be running on that PTY.
    pub fn recover(&mut self, worker_id: &str) -> Result<Option<Lease>, ScheduleError> {
        let worker = self
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| ScheduleError::UnknownWorker(worker_id.into()))?;
        if worker.state != WorkerState::Quarantined {
            return Err(ScheduleError::NotQuarantined(worker_id.into()));
        }
        worker.state = WorkerState::Idle;
        self.dispatch_next(worker_id)
    }

    pub fn queued_for(&self, device_identity: &str) -> usize {
        self.queues.get(device_identity).map_or(0, VecDeque::len)
    }

    fn idle_worker_for(&self, device_identity: &str) -> Option<String> {
        self.workers
            .values()
            .find(|worker| {
                worker.device_identity == device_identity && worker.state == WorkerState::Idle
            })
            .map(|worker| worker.id.clone())
    }

    fn lease(&mut self, worker_id: String, job: JobRequest) -> Result<Lease, ScheduleError> {
        let worker = self
            .workers
            .get_mut(&worker_id)
            .ok_or_else(|| ScheduleError::UnknownWorker(worker_id.clone()))?;
        if worker.device_identity != job.device_identity {
            return Err(ScheduleError::WrongDevice {
                worker: worker.device_identity.clone(),
                job: job.device_identity.clone(),
            });
        }
        if worker.state != WorkerState::Idle {
            return Err(ScheduleError::WorkerBusy(worker_id));
        }
        worker.state = WorkerState::Leased {
            job_id: job.id.clone(),
        };
        Ok(Lease { worker_id, job })
    }

    fn release_known_job(
        &mut self,
        worker_id: &str,
        job_id: &str,
        next_state: WorkerState,
    ) -> Result<(), ScheduleError> {
        let worker = self
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| ScheduleError::UnknownWorker(worker_id.into()))?;
        match &worker.state {
            WorkerState::Leased { job_id: current } if current == job_id => {
                worker.state = next_state;
                Ok(())
            }
            WorkerState::Leased { job_id: current } => Err(ScheduleError::WrongJob {
                expected: current.clone(),
                got: job_id.into(),
            }),
            _ => Err(ScheduleError::WorkerNotLeased(worker_id.into())),
        }
    }

    fn dispatch_next(&mut self, worker_id: &str) -> Result<Option<Lease>, ScheduleError> {
        let device_identity = {
            let worker = self
                .workers
                .get(worker_id)
                .ok_or_else(|| ScheduleError::UnknownWorker(worker_id.into()))?;
            if worker.state != WorkerState::Idle {
                return Ok(None);
            }
            worker.device_identity.clone()
        };

        let next = self
            .queues
            .get_mut(&device_identity)
            .and_then(VecDeque::pop_front);
        match next {
            Some(job) => self.lease(worker_id.into(), job).map(Some),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    UnknownWorker(String),
    WorkerBusy(String),
    WorkerNotLeased(String),
    NotQuarantined(String),
    WrongJob { expected: String, got: String },
    WrongDevice { worker: String, job: String },
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownWorker(id) => write!(f, "unknown worker {id}"),
            Self::WorkerBusy(id) => write!(f, "worker {id} is busy"),
            Self::WorkerNotLeased(id) => write!(f, "worker {id} is not leased"),
            Self::NotQuarantined(id) => write!(f, "worker {id} is not quarantined"),
            Self::WrongJob { expected, got } => {
                write!(f, "worker belongs to job {expected}, not {got}")
            }
            Self::WrongDevice { worker, job } => {
                write!(f, "worker device {worker} does not match job device {job}")
            }
        }
    }
}

impl std::error::Error for ScheduleError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, device: &str, root: &str) -> JobRequest {
        JobRequest {
            id: id.into(),
            device_identity: device.into(),
            remote_root: root.into(),
            command: "pwd".into(),
        }
    }

    #[test]
    fn same_device_different_workspaces_share_workers_but_keep_roots() {
        let mut scheduler = Scheduler::default();
        scheduler.register_worker("w1".into(), "car-a".into());

        let first = scheduler
            .submit(job("j1", "car-a", "/home/a/project-a"))
            .unwrap();
        assert_eq!(first.job.remote_root, "/home/a/project-a");

        assert!(scheduler
            .submit(job("j2", "car-a", "/home/a/project-b"))
            .is_none());
        let second = scheduler.finish("w1", "j1").unwrap().unwrap();
        assert_eq!(second.worker_id, "w1");
        assert_eq!(second.job.remote_root, "/home/a/project-b");
    }

    #[test]
    fn never_schedules_across_devices() {
        let mut scheduler = Scheduler::default();
        scheduler.register_worker("a1".into(), "car-a".into());
        scheduler.register_worker("b1".into(), "car-b".into());

        let lease = scheduler.submit(job("j", "car-b", "/b")).unwrap();
        assert_eq!(lease.worker_id, "b1");
        assert_eq!(scheduler.worker("a1").unwrap().state, WorkerState::Idle);
    }

    #[test]
    fn queue_is_fifo_per_device() {
        let mut scheduler = Scheduler::default();
        scheduler.register_worker("w".into(), "car".into());
        let first = scheduler.submit(job("1", "car", "/one")).unwrap();
        assert_eq!(first.job.id, "1");
        assert!(scheduler.submit(job("2", "car", "/two")).is_none());
        assert!(scheduler.submit(job("3", "car", "/three")).is_none());

        let second = scheduler.finish("w", "1").unwrap().unwrap();
        assert_eq!(second.job.id, "2");
        let third = scheduler.finish("w", "2").unwrap().unwrap();
        assert_eq!(third.job.id, "3");
    }

    #[test]
    fn unknown_job_quarantines_worker_and_does_not_replay_queue() {
        let mut scheduler = Scheduler::default();
        scheduler.register_worker("w".into(), "car".into());
        scheduler.submit(job("1", "car", "/one")).unwrap();
        assert!(scheduler.submit(job("2", "car", "/two")).is_none());

        scheduler.mark_unknown("w", "1").unwrap();
        assert_eq!(
            scheduler.worker("w").unwrap().state,
            WorkerState::Quarantined
        );
        assert_eq!(scheduler.queued_for("car"), 1);

        let recovered = scheduler.recover("w").unwrap().unwrap();
        assert_eq!(recovered.job.id, "2");
    }

    #[test]
    fn wrong_job_cannot_release_worker() {
        let mut scheduler = Scheduler::default();
        scheduler.register_worker("w".into(), "car".into());
        scheduler.submit(job("correct", "car", "/root")).unwrap();

        assert!(matches!(
            scheduler.finish("w", "wrong"),
            Err(ScheduleError::WrongJob { .. })
        ));
        assert_eq!(
            scheduler.worker("w").unwrap().state,
            WorkerState::Leased {
                job_id: "correct".into()
            }
        );
    }
}
