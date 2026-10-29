//! Write failures a test makes a container meet, to check that a failure
//! taking extents or checkpointing the map leaves the file and the open as
//! they were. Only tests build it.

use std::cell::Cell;
use std::io;

use fylo_format::ContainerExtentKind;

/// A write of the container that a test can make fail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContainerFault {
    /// Growing the file to hold extents taken past its end.
    Grow,
    /// Clearing a reused segment extent.
    ClearExtent,
    /// Writing the header of an extent of this kind.
    ExtentHeader(ContainerExtentKind),
    /// Writing a page of a map copy.
    Map,
    /// Writing the superblock copy that names a new map.
    Superblock,
    /// Flushing that superblock copy, which is written by then.
    Flush,
}

thread_local! {
    /// The armed failure, and how many of its writes pass first.
    static ARMED: Cell<Option<(ContainerFault, usize)>> = const { Cell::new(None) };
}

/// Make the next write `fault` names on this thread fail, once, as on a
/// full disk. Each test runs on its own thread, so tests running side by
/// side never meet another's failure.
pub(crate) fn arm(fault: ContainerFault) {
    arm_after(fault, 0);
}

/// Make a write `fault` names on this thread fail once `passes` of them
/// have passed.
pub(crate) fn arm_after(fault: ContainerFault, passes: usize) {
    ARMED.set(Some((fault, passes)));
}

/// Whether the failure armed on this thread is still waiting for its
/// write. A failure that was never met is disarmed.
pub(crate) fn pending() -> bool {
    ARMED.take().is_some()
}

/// Fail when this thread armed `fault` and its passes are spent.
pub(super) fn check(fault: ContainerFault) -> io::Result<()> {
    match ARMED.get() {
        Some((armed, passes)) if armed == fault && passes > 0 => {
            ARMED.set(Some((armed, passes - 1)));
            Ok(())
        }
        Some((armed, _)) if armed == fault => {
            ARMED.set(None);
            Err(io::Error::new(
                io::ErrorKind::StorageFull,
                format!("injected {fault:?} failure"),
            ))
        }
        _ => Ok(()),
    }
}

mod tests;
