//! `--load N --cores K`: a test run squeezed onto K cores beside N busy
//! loops, so that a race whose window is a few microseconds on an idle
//! machine opens wide enough to be hit (`docs/testing-and-ci.md`, "Running
//! the tests many times over").
//!
//! The SIGINT flake of step 13b failed about once in a thousand runs on the
//! desktop. Pinned to two cores beside busy loops it failed far more often,
//! which is how its session saw it at all.
//!
//! **How the run is pinned.** This process pins itself, all its threads, to
//! the first K of the cores it may use, and everything it starts after that
//! inherits the pin: cargo, nextest, every test process. On Linux it asks
//! `taskset`; on Windows, PowerShell, which sets the process's affinity
//! mask. Neither needs a dependency here. The N busy loops are threads of
//! this process, so they die with it, however it ends.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crate::Result;

/// The busy loops, running until this is dropped.
pub(crate) struct Load {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for Load {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// What `--load` and `--cores` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Squeeze {
    /// Busy loops beside the run.
    pub(crate) load: usize,
    /// The cores the run and the loops share. None: every core this
    /// process may use.
    pub(crate) cores: Option<usize>,
}

impl Squeeze {
    /// Reads `--load N` or `--cores K` at `argument`, taking its value from
    /// `next`. Returns whether it was one of them.
    pub(crate) fn take(
        &mut self,
        argument: &str,
        next: &mut dyn Iterator<Item = &String>,
    ) -> Result<bool> {
        let field = match argument {
            "--load" => "load",
            "--cores" => "cores",
            _ => return Ok(false),
        };
        let text = next
            .next()
            .ok_or_else(|| format!("`{argument}` needs a number"))?;
        let number: usize = text
            .parse()
            .map_err(|_| format!("`{argument} {text}` is not a number"))?;
        if field == "load" {
            if number > 64 {
                return Err(format!("`--load {number}`: 64 busy loops at most"));
            }
            self.load = number;
        } else {
            if number == 0 {
                return Err("`--cores 0`: at least one core".to_owned());
            }
            self.cores = Some(number);
        }
        Ok(true)
    }

    /// Pins this process to its cores and starts the busy loops. Nothing
    /// happens for a squeeze that asks for nothing.
    pub(crate) fn start(self) -> Result<Option<Load>> {
        if self == Squeeze::default() {
            return Ok(None);
        }
        if let Some(cores) = self.cores {
            let pinned = pin(cores)?;
            println!(
                "pinned to cores {pinned}, with {} busy loop(s) beside the run",
                self.load
            );
        } else {
            println!("{} busy loop(s) beside the run, on every core", self.load);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..self.load)
            .map(|index| {
                let stop = Arc::clone(&stop);
                std::thread::Builder::new()
                    .name(format!("busy-{index}"))
                    .spawn(move || busy(&stop))
                    .map_err(|error| format!("could not start a busy loop: {error}"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Load { stop, threads }))
    }
}

/// Spins until told to stop: arithmetic the compiler cannot remove, not a
/// pause instruction, which would give its core's other thread room.
fn busy(stop: &AtomicBool) {
    let mut value: u64 = 0x9e37_79b9_7f4a_7c15;
    while !stop.load(Ordering::Relaxed) {
        for _ in 0..10_000 {
            value = std::hint::black_box(value.rotate_left(5).wrapping_mul(0x2545_f491_4f6c_dd1d));
        }
    }
}

/// The first `count` cores of `allowed`, a CPU list as Linux writes it
/// (`0-3,8,10-11`), as a list `taskset -c` reads.
fn first_cores(allowed: &str, count: usize) -> Result<String> {
    let mut cores = Vec::new();
    for part in allowed.trim().split(',').filter(|part| !part.is_empty()) {
        let (low, high) = part.split_once('-').unwrap_or((part, part));
        let (low, high): (usize, usize) = (
            low.parse()
                .map_err(|_| format!("a CPU list I cannot read: {allowed}"))?,
            high.parse()
                .map_err(|_| format!("a CPU list I cannot read: {allowed}"))?,
        );
        cores.extend(low..=high);
    }
    if cores.len() < count {
        return Err(format!(
            "`--cores {count}`: this process may use {} core(s) ({})",
            cores.len(),
            allowed.trim()
        ));
    }
    let chosen: Vec<String> = cores[..count].iter().map(ToString::to_string).collect();
    Ok(chosen.join(","))
}

/// The lowest `count` set bits of `mask`.
fn first_bits(mask: u64, count: usize) -> Result<u64> {
    let mut chosen = 0u64;
    let mut left = count;
    for bit in 0..64 {
        if left == 0 {
            break;
        }
        if mask & (1 << bit) != 0 {
            chosen |= 1 << bit;
            left -= 1;
        }
    }
    if left > 0 {
        return Err(format!(
            "`--cores {count}`: this process may use {} core(s)",
            mask.count_ones()
        ));
    }
    Ok(chosen)
}

/// Pins this process and all its threads to `count` of its cores, and says
/// which.
fn pin(count: usize) -> Result<String> {
    let pid = std::process::id().to_string();
    if cfg!(windows) {
        let read = powershell(&format!("[int64](Get-Process -Id {pid}).ProcessorAffinity"))?;
        let mask: u64 = read
            .trim()
            .parse()
            .map_err(|_| format!("PowerShell gave an affinity mask I cannot read: {read}"))?;
        let chosen = first_bits(mask, count)?;
        powershell(&format!(
            "(Get-Process -Id {pid}).ProcessorAffinity = [IntPtr]{chosen}"
        ))?;
        Ok(format!("mask {chosen:#x}"))
    } else {
        let status = std::fs::read_to_string("/proc/self/status")
            .map_err(|error| format!("could not read /proc/self/status: {error}"))?;
        let allowed = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .ok_or("/proc/self/status has no Cpus_allowed_list")?;
        let cores = first_cores(allowed, count)?;
        let output = Command::new("taskset")
            .args(["--all-tasks", "--pid", "--cpu-list", &cores, &pid])
            .output()
            .map_err(|error| format!("could not run taskset: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "taskset failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(cores)
    }
}

fn powershell(script: &str) -> Result<String> {
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|error| format!("could not run PowerShell: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "PowerShell failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{Squeeze, first_bits, first_cores};

    #[test]
    fn the_first_cores_of_a_list_are_chosen() {
        assert_eq!(first_cores("0-27", 2).unwrap(), "0,1");
        assert_eq!(first_cores("3,5-7\n", 3).unwrap(), "3,5,6");
        assert_eq!(first_cores("4", 1).unwrap(), "4");
        let error = first_cores("0-1", 4).unwrap_err();
        assert!(error.contains("may use 2 core(s)"), "{error}");
        assert!(first_cores("x-y", 1).is_err());
    }

    #[test]
    fn the_lowest_bits_of_a_mask_are_chosen() {
        assert_eq!(first_bits(0xff, 2).unwrap(), 0b11);
        assert_eq!(
            first_bits(0b1010_1100, 3).unwrap(),
            0b1010_1100 & !0b1000_0000
        );
        assert!(first_bits(0b1, 2).is_err());
    }

    #[test]
    fn load_and_cores_are_read() {
        let arguments: Vec<String> = ["8", "2", "x"].map(str::to_owned).to_vec();
        let mut squeeze = Squeeze::default();
        let mut rest = arguments.iter();
        assert!(squeeze.take("--load", &mut rest).unwrap());
        assert!(squeeze.take("--cores", &mut rest).unwrap());
        assert!(!squeeze.take("--count", &mut rest).unwrap());
        assert_eq!(
            squeeze,
            Squeeze {
                load: 8,
                cores: Some(2)
            }
        );
        let mut rest = arguments[2..].iter();
        assert!(squeeze.take("--load", &mut rest).is_err());
        let zero = ["0".to_owned()];
        assert!(squeeze.take("--cores", &mut zero.iter()).is_err());
        let many = ["65".to_owned()];
        assert!(squeeze.take("--load", &mut many.iter()).is_err());
    }

    #[test]
    fn a_squeeze_that_asks_for_nothing_does_nothing() {
        assert!(Squeeze::default().start().unwrap().is_none());
    }

    /// Busy loops start and stop, with no pin: nothing about the machine
    /// is changed.
    #[test]
    fn busy_loops_stop_when_dropped() {
        let load = Squeeze {
            load: 2,
            cores: None,
        }
        .start()
        .unwrap()
        .expect("two loops");
        assert_eq!(load.threads.len(), 2);
        drop(load);
    }
}
