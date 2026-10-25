// The few POSIX calls std does not offer, declared directly (no libc crate):
// pipes at fixed descriptors and the parent-death signal for Chromium and the
// ffmpeg of the overlay, signals taken by a thread instead of a handler, and
// the monotonic clock as a number (Chromium's frame times).
// Linux only, as the images the sidecar and the overlay run in.

use std::ffi::c_int;
use std::io;
use std::os::fd::RawFd;

const SIG_BLOCK: c_int = 0;
const SIG_SETMASK: c_int = 2;
pub const SIGHUP: c_int = 1;
pub const SIGINT: c_int = 2;
pub const SIGQUIT: c_int = 3;
const SIGKILL: c_int = 9;
pub const SIGTERM: c_int = 15;
const PR_SET_PDEATHSIG: c_int = 1;
const F_SETFD: c_int = 2;
const CLOCK_MONOTONIC: c_int = 1;

/// `struct timespec` of 64-bit Linux (x86_64, aarch64; glibc and musl).
#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

/// `sigset_t` of glibc and musl: 1024 bits.
#[repr(C)]
struct SigSet([u64; 16]);

unsafe extern "C" {
    fn dup2(old: c_int, new: c_int) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    fn prctl(option: c_int, ...) -> c_int;
    fn sigemptyset(set: *mut SigSet) -> c_int;
    fn sigaddset(set: *mut SigSet, signal: c_int) -> c_int;
    fn sigprocmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
    fn pthread_sigmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
    fn sigwait(set: *const SigSet, signal: *mut c_int) -> c_int;
    fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
}

/// `CLOCK_MONOTONIC` in milliseconds: the clock of `Instant` and of
/// Chromium's `TimeTicks` (`HeadlessExperimental.beginFrame` frame times).
pub fn monotonic_ms() -> f64 {
    let mut time = Timespec { sec: 0, nsec: 0 };
    // SAFETY: valid output pointer; CLOCK_MONOTONIC always exists on Linux.
    unsafe {
        clock_gettime(CLOCK_MONOTONIC, &mut time);
    }
    time.sec as f64 * 1000.0 + time.nsec as f64 / 1e6
}

const STOP_SIGNALS: &[c_int] = &[SIGTERM, SIGINT];

fn signal_set(signals: &[c_int]) -> SigSet {
    let mut set = SigSet([0; 16]);
    // SAFETY: `set` is a valid, writable sigset_t.
    unsafe {
        sigemptyset(&mut set);
        for &signal in signals {
            sigaddset(&mut set, signal);
        }
    }
    set
}

/// Blocks `signals` in this thread and every thread started after it;
/// `wait_signal` then takes them. Children of `Command` start unblocked.
pub fn block_signals(signals: &[c_int]) {
    let set = signal_set(signals);
    // SAFETY: valid set; a null old set is allowed.
    unsafe {
        pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Waits for one of `signals` (blocked by `block_signals`).
pub fn wait_signal(signals: &[c_int]) -> c_int {
    let set = signal_set(signals);
    let mut signal = 0;
    // SAFETY: valid set and output pointer.
    unsafe {
        sigwait(&set, &mut signal);
    }
    signal
}

/// Blocks SIGTERM and SIGINT for `wait_for_stop`. Call first thing in `main`.
pub fn block_stop_signals() {
    block_signals(STOP_SIGNALS);
}

/// Waits for SIGTERM or SIGINT (blocked by `block_stop_signals`).
pub fn wait_for_stop() -> i32 {
    wait_signal(STOP_SIGNALS)
}

/// Sends `signal` to process `pid`.
pub fn send_signal(pid: u32, signal: c_int) {
    // SAFETY: plain system call; a process already gone only fails it.
    unsafe {
        kill(pid as c_int, signal);
    }
}

/// For `CommandExt::pre_exec` in the forked child: `fds[i].0` becomes
/// descriptor `fds[i].1`, the signal mask is cleared (Chromium must not
/// inherit the blocked stop signals) and the child gets SIGKILL when the
/// sidecar dies. Only async-signal-safe calls.
pub fn child_setup(fds: &[(RawFd, RawFd)]) -> io::Result<()> {
    for &(from, to) in fds {
        // SAFETY: plain descriptor duplication in the child.
        if unsafe { dup2(from, to) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let empty = SigSet([0; 16]);
    // SAFETY: an all-zero sigset_t is the empty set on Linux; prctl takes an unsigned long.
    unsafe {
        sigprocmask(SIG_SETMASK, &empty, std::ptr::null_mut());
        if prctl(PR_SET_PDEATHSIG, SIGKILL as std::ffi::c_ulong) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// For `CommandExt::pre_exec` of the overlay's ffmpeg: descriptor `fd`
/// stays open across exec (its `pipe:N` input) and ffmpeg gets SIGTERM when
/// the overlay dies. Only async-signal-safe calls.
pub fn ffmpeg_child_setup(fd: RawFd) -> io::Result<()> {
    // SAFETY: clears FD_CLOEXEC of a descriptor the child owns; prctl takes an unsigned long.
    unsafe {
        if fcntl(fd, F_SETFD, 0 as c_int) < 0 {
            return Err(io::Error::last_os_error());
        }
        if prctl(PR_SET_PDEATHSIG, SIGTERM as std::ffi::c_ulong) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
