// The few POSIX calls std does not offer, declared directly (no libc crate):
// pipes at fixed descriptors and the parent-death signal for Chromium, and
// SIGTERM/SIGINT taken by a thread instead of a handler. Linux only, as the
// images the sidecar runs in.

use std::ffi::c_int;
use std::io;
use std::os::fd::RawFd;

const SIG_BLOCK: c_int = 0;
const SIG_SETMASK: c_int = 2;
const SIGINT: c_int = 2;
const SIGKILL: c_int = 9;
const SIGTERM: c_int = 15;
const PR_SET_PDEATHSIG: c_int = 1;

/// `sigset_t` of glibc and musl: 1024 bits.
#[repr(C)]
struct SigSet([u64; 16]);

unsafe extern "C" {
    fn dup2(old: c_int, new: c_int) -> c_int;
    fn prctl(option: c_int, ...) -> c_int;
    fn sigemptyset(set: *mut SigSet) -> c_int;
    fn sigaddset(set: *mut SigSet, signal: c_int) -> c_int;
    fn sigprocmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
    fn pthread_sigmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int;
    fn sigwait(set: *const SigSet, signal: *mut c_int) -> c_int;
}

fn stop_signals() -> SigSet {
    let mut set = SigSet([0; 16]);
    // SAFETY: `set` is a valid, writable sigset_t.
    unsafe {
        sigemptyset(&mut set);
        sigaddset(&mut set, SIGTERM);
        sigaddset(&mut set, SIGINT);
    }
    set
}

/// Blocks SIGTERM and SIGINT in this thread and every thread started after
/// it; `wait_for_stop` then takes them. Call first thing in `main`.
pub fn block_stop_signals() {
    let set = stop_signals();
    // SAFETY: valid set; a null old set is allowed.
    unsafe {
        pthread_sigmask(SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Waits for SIGTERM or SIGINT (blocked by `block_stop_signals`).
pub fn wait_for_stop() -> i32 {
    let set = stop_signals();
    let mut signal = 0;
    // SAFETY: valid set and output pointer.
    unsafe {
        sigwait(&set, &mut signal);
    }
    signal
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
