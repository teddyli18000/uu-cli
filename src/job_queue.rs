//! Cross-process FIFO tickets for serializing jobs per remote device.
//!
//! A ticket owns an exclusive file lock while it waits. Crashed processes
//! release the OS lock automatically; the next queue operation detects and
//! removes those stale ticket files while holding the queue metadata lock.

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const QUEUE_DIRECTORY: &str = "job-queue";
const TICKET_PREFIX: &str = "ticket-";
const TICKET_SUFFIX: &str = ".lock";

/// A position in the pending queue for one remote device.
///
/// Hold a ticket only while waiting to acquire a worker lease. Call `release`
/// immediately after acquiring the lease and before dispatching the job.
#[derive(Debug)]
pub struct QueueTicket {
    queue_dir: PathBuf,
    number: u64,
    ticket_file: Option<File>,
}

impl QueueTicket {
    /// Join the FIFO queue scoped to the config directory and remote device.
    pub fn join(config_path: &Path, device: &str) -> Result<Self> {
        let queue_dir = queue_directory(config_path, device)?;
        fs::create_dir_all(&queue_dir)
            .with_context(|| format!("cannot create device job queue {}", queue_dir.display()))?;

        let _metadata_lock = lock_metadata(&queue_dir)?;
        clean_stale_tickets(&queue_dir)?;

        let number = next_number(&queue_dir)?;
        let ticket_path = ticket_path(&queue_dir, number);
        let ticket_file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&ticket_path)
            .with_context(|| format!("cannot create queue ticket {}", ticket_path.display()))?;
        ticket_file
            .try_lock_exclusive()
            .with_context(|| format!("cannot lock queue ticket {}", ticket_path.display()))?;

        Ok(Self {
            queue_dir,
            number,
            ticket_file: Some(ticket_file),
        })
    }

    /// Number of live tickets ahead of this one. Zero means this ticket is next.
    pub fn position(&self) -> Result<usize> {
        if self.ticket_file.is_none() {
            return Err(anyhow!("queue ticket has already been released"));
        }

        let _metadata_lock = lock_metadata(&self.queue_dir)?;
        let tickets = live_ticket_numbers(&self.queue_dir)?;
        Ok(tickets
            .into_iter()
            .filter(|number| *number < self.number)
            .count())
    }

    /// Release this waiting ticket. Call as soon as a worker lease is acquired.
    pub fn release(&mut self) -> Result<()> {
        if self.ticket_file.is_none() {
            return Ok(());
        }

        let _metadata_lock = lock_metadata(&self.queue_dir)?;
        let ticket_file = self.ticket_file.take().expect("checked above");
        drop(ticket_file);
        remove_ticket(&self.queue_dir, self.number)
    }
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        let Some(ticket_file) = self.ticket_file.take() else {
            return;
        };

        // Destructors must not block behind another process. If metadata is
        // busy, closing our ticket lock is sufficient; the next queue scan
        // will remove the now-stale file.
        let metadata_path = self.queue_dir.join("queue.lock");
        let Ok(metadata_file) = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(metadata_path)
        else {
            drop(ticket_file);
            return;
        };

        if metadata_file.try_lock_exclusive().is_ok() {
            drop(ticket_file);
            let _ = remove_ticket(&self.queue_dir, self.number);
        } else {
            drop(ticket_file);
        }
    }
}

fn queue_directory(config_path: &Path, device: &str) -> Result<PathBuf> {
    let parent = config_path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent directory"))?;
    let digest = Sha256::digest(device.as_bytes());
    let device_key: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(parent
        .join("runtime")
        .join(QUEUE_DIRECTORY)
        .join(device_key))
}

fn lock_metadata(queue_dir: &Path) -> Result<File> {
    let path = queue_dir.join("queue.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open queue metadata lock {}", path.display()))?;
    FileExt::lock_exclusive(&file)
        .with_context(|| format!("cannot lock queue metadata {}", path.display()))?;
    Ok(file)
}

fn next_number(queue_dir: &Path) -> Result<u64> {
    let counter_path = queue_dir.join("counter");
    let mut counter = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&counter_path)
        .with_context(|| format!("cannot open queue counter {}", counter_path.display()))?;

    let mut stored = String::new();
    counter.read_to_string(&mut stored)?;
    let stored_number = stored.trim().parse::<u64>().unwrap_or(0);
    let max_ticket = ticket_numbers(queue_dir)?.into_iter().max().unwrap_or(0);
    let number = stored_number
        .max(max_ticket)
        .checked_add(1)
        .ok_or_else(|| anyhow!("device job queue ticket counter exhausted"))?;

    counter.set_len(0)?;
    counter.seek(SeekFrom::Start(0))?;
    writeln!(counter, "{number}")?;
    counter.sync_all()?;
    Ok(number)
}

fn ticket_numbers(queue_dir: &Path) -> Result<Vec<u64>> {
    let mut numbers = Vec::new();
    for entry in fs::read_dir(queue_dir)
        .with_context(|| format!("cannot read device job queue {}", queue_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(number) = parse_ticket_name(&name) {
            numbers.push(number);
        }
    }
    Ok(numbers)
}

fn parse_ticket_name(name: &str) -> Option<u64> {
    name.strip_prefix(TICKET_PREFIX)?
        .strip_suffix(TICKET_SUFFIX)?
        .parse()
        .ok()
}

fn ticket_path(queue_dir: &Path, number: u64) -> PathBuf {
    queue_dir.join(format!("{TICKET_PREFIX}{number:020}{TICKET_SUFFIX}"))
}

fn live_ticket_numbers(queue_dir: &Path) -> Result<Vec<u64>> {
    let mut live = Vec::new();
    for number in ticket_numbers(queue_dir)? {
        let path = ticket_path(queue_dir, number);
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot open queue ticket {}", path.display()))
            }
        };

        match file.try_lock_exclusive() {
            Ok(()) => {
                // The metadata lock serializes all queue mutations. Closing the
                // stale lock before removal also works on Windows.
                drop(file);
                remove_ticket(queue_dir, number)?;
            }
            Err(error) if is_lock_contended(&error) => live.push(number),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot inspect queue ticket {}", path.display()))
            }
        }
    }
    Ok(live)
}

fn clean_stale_tickets(queue_dir: &Path) -> Result<()> {
    let _ = live_ticket_numbers(queue_dir)?;
    Ok(())
}

fn remove_ticket(queue_dir: &Path, number: u64) -> Result<()> {
    let path = ticket_path(queue_dir, number);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("cannot remove queue ticket {}", path.display()))
        }
    }
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        // ERROR_LOCK_VIOLATION: Windows reports lock contention as raw OS 33.
        error.raw_os_error() == Some(33)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn config_path(root: &Path) -> PathBuf {
        root.join("uu-cli").join("config.json")
    }

    #[test]
    fn tickets_are_fifo_and_release_moves_next_ticket_to_head() {
        let root = tempdir().unwrap();
        let config = config_path(root.path());
        let mut first = QueueTicket::join(&config, "device-a").unwrap();
        let second = QueueTicket::join(&config, "device-a").unwrap();
        let third = QueueTicket::join(&config, "device-a").unwrap();

        assert_eq!(first.position().unwrap(), 0);
        assert_eq!(second.position().unwrap(), 1);
        assert_eq!(third.position().unwrap(), 2);

        first.release().unwrap();
        assert_eq!(second.position().unwrap(), 0);
        assert_eq!(third.position().unwrap(), 1);
    }

    #[test]
    fn queues_for_distinct_devices_are_independent() {
        let root = tempdir().unwrap();
        let config = config_path(root.path());
        let first = QueueTicket::join(&config, "device-a").unwrap();
        let second = QueueTicket::join(&config, "device-b").unwrap();

        assert_eq!(first.position().unwrap(), 0);
        assert_eq!(second.position().unwrap(), 0);
    }

    #[test]
    fn dropping_ticket_removes_it_from_the_queue() {
        let root = tempdir().unwrap();
        let config = config_path(root.path());
        let first = QueueTicket::join(&config, "device-a").unwrap();
        let first_path = ticket_path(&first.queue_dir, first.number);
        let second = QueueTicket::join(&config, "device-a").unwrap();

        drop(first);

        assert!(!first_path.exists());
        assert_eq!(second.position().unwrap(), 0);
    }

    #[test]
    fn crashed_waiter_does_not_block_the_next_ticket() {
        let root = tempdir().unwrap();
        let config = config_path(root.path());
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "job_queue::tests::crashed_ticket_child_probe"])
            .env("UU_CLI_QUEUE_TEST_CONFIG", &config)
            .env("UU_CLI_QUEUE_TEST_DEVICE", "device-a")
            .status()
            .unwrap();
        assert!(
            status.success(),
            "child process failed to create its queue ticket"
        );

        let next = QueueTicket::join(&config, "device-a").unwrap();
        assert_eq!(next.position().unwrap(), 0);
    }

    #[test]
    fn crashed_ticket_child_probe() {
        let (Some(config), Some(device)) = (
            std::env::var_os("UU_CLI_QUEUE_TEST_CONFIG"),
            std::env::var_os("UU_CLI_QUEUE_TEST_DEVICE"),
        ) else {
            return;
        };
        let _ticket = QueueTicket::join(Path::new(&config), &device.to_string_lossy()).unwrap();
        // Simulate abrupt process termination so Drop cannot clean the ticket.
        std::process::exit(0);
    }
}
