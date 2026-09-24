//! Live heap bytes, and what that number is and is not (issue libviprs#1136).
//!
//! A `#[global_allocator]` that keeps a running total of the bytes in flight
//! and a high-water mark over them. A phase arms it, and what it reports is
//! the peak the process reached while that phase ran, counted from what was
//! already live when the phase started.
//!
//! # Which quantity this is
//!
//! There are two memory figures about the PMTiles writer in the engine
//! repository and they do not agree, because they are not the same
//! measurement. `tests/pmtiles_bounded_memory.rs` says **72 bytes a distinct
//! payload**, measured on live heap and scoped to the three tables the
//! writer's own documentation names. `src/pmtiles/writer.rs`'s table says
//! 1,083.8 MB of peak **RSS** at ten million payloads, and the slope across
//! its five rows is about 105.8 bytes a payload. Live heap is what the program
//! is holding; RSS is what the kernel has given the process and has not taken
//! back, so it carries the allocator's unreturned pages, fragmentation, the
//! binary, the stacks and every buffer that was ever touched. Nothing
//! reconciles them and nothing should: one is a subset of a process measured
//! one way, the other is the whole process measured another.
//!
//! **This module counts live heap**, so it is on the 72's basis and not the
//! 105.8's. It is *whole-process* live heap rather than the writer's three
//! tables, because a global allocator cannot see whose bytes it is handing
//! out, so the source raster and everything else the engine holds is in it
//! too. That is the right number for the question the split asks, which is
//! whether finalize raises the peak a generation reaches, and it is the wrong
//! number to divide by a payload count and compare with either figure above.
//!
//! The document already has a `heapPeakBytes` field on every cell and nothing
//! has ever filled it. `peak_rss_mb` beside it stays what it always was: the
//! child's `ru_maxrss` through `wait4`, which is the RSS quantity.
//!
//! # Why it is armed rather than always on
//!
//! Counting costs two atomic read-modify-writes per allocation, and
//! `read_concurrent@8` is a published curve about eight threads contending.
//! Eight threads hammering one counter is contention this harness invented,
//! measured, and published as the archive's. So counting is behind a flag:
//! with it off an allocation pays one relaxed load of a line nobody writes,
//! which no thread has to wait for.
//!
//! The cost with it on is inside the noise floor. Nine interleaved pairs of
//! the combined `generate` pass, armed against unarmed, moved by +13.1%,
//! -3.6%, -3.0% and -0.3% on the four `(backend, cell)` combinations I
//! measured, which is a sign that flips and a spread the wall noise floor
//! already covers. It is not free, so the reconciliation test arms it across
//! the combined row as well as the split, and the two phases are then
//! instrumented the same way as the number they are reconciled against.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// The change in live heap since counting was first switched on.
///
/// Signed, and it has to be. With counting behind a flag an allocation made
/// before a window opened and freed inside it is a subtraction with no
/// matching addition, so the running total goes below where it started. That
/// is not an error: every object alive when a window opened was allocated
/// before it, so freeing one really did lower live heap, and the signed total
/// is exactly `live(now) - live(when counting started)`. Unsigned, the same
/// subtraction wraps to something near `u64::MAX` and the peak becomes
/// nonsense.
static IN_FLIGHT: AtomicI64 = AtomicI64::new(0);

/// The high-water mark over [`IN_FLIGHT`] since the last [`arm`].
static PEAK: AtomicI64 = AtomicI64::new(0);

/// Whether an allocation is counted at all.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Whether [`Counting`] has ever charged an allocation, which is the same
/// question as whether it is this process's global allocator.
///
/// The allocator sets it from inside the counting path, so it is a fact about
/// which allocator ran. The first version of this inferred the same thing from
/// the counter moving by the size of a probe allocation, and that is an
/// inference two threads share: any other thread freeing something bigger than
/// the probe at the wrong moment made an installed allocator look absent. It
/// failed about a third of the time with two tests in one binary.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// One measuring window at a time, because the counters above are one per
/// process and `libtest` runs a binary's tests on parallel threads. Two
/// windows open at once measure each other.
static WINDOW: Mutex<()> = Mutex::new(());

thread_local! {
    /// Whether this thread is already inside a window.
    ///
    /// A phase that arms inside a measurement that armed around it must not
    /// queue behind the lock its own caller is holding, so the inner window
    /// takes no lock and the outer one keeps it.
    static NESTED: Cell<bool> = const { Cell::new(false) };
}

/// How much [`arm`] allocates to make the allocator say it is there.
///
/// Small, because the answer comes from the allocator setting [`INSTALLED`]
/// and not from the size of the block. Freed again before `arm` returns.
const PROBE_BYTES: usize = 64;

/// The counting allocator.
///
/// Install it in a binary that wants the numbers:
///
/// ```ignore
/// #[global_allocator]
/// static HEAP: libviprs_bench::storage::heap::Counting =
///     libviprs_bench::storage::heap::Counting;
/// ```
///
/// A binary that leaves that line out still compiles and still runs every
/// scenario; what it does not do is publish a heap peak, because [`arm`]
/// probes for the line rather than assuming it.
pub struct Counting;

impl Counting {
    fn charge(size: usize) {
        if !ARMED.load(Ordering::Relaxed) {
            return;
        }
        if !INSTALLED.load(Ordering::Relaxed) {
            INSTALLED.store(true, Ordering::Relaxed);
        }
        let now = IN_FLIGHT.fetch_add(size as i64, Ordering::Relaxed) + size as i64;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }

    fn discharge(size: usize) {
        if !ARMED.load(Ordering::Relaxed) {
            return;
        }
        IN_FLIGHT.fetch_sub(size as i64, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to `System`, which upholds the `GlobalAlloc`
// contract, and hands back exactly the pointer `System` returned. The counters
// are relaxed atomics read only for reporting, so they cannot affect the
// pointers handed back or the layouts passed on.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            Self::charge(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            Self::charge(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        Self::discharge(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(ptr, layout, new_size) };
        if !out.is_null() {
            // The net difference, which is the grow-in-place reading. The same
            // allocator in `libviprs/tests/pmtiles_bounded_memory.rs` charges
            // the new block before discharging the old one, so its peak is the
            // sum of the two, and it says why: it is an upper-bound test and
            // the conservative direction is the safe one there. This is a
            // published measurement rather than a bound, and a peak that
            // counts a `Vec` twice every time it doubles is a peak nothing in
            // the process ever held.
            if new_size >= layout.size() {
                Self::charge(new_size - layout.size());
            } else {
                Self::discharge(layout.size() - new_size);
            }
        }
        out
    }
}

/// A window in which allocations are counted.
///
/// Holds the process's one measuring lock, so a second window on another
/// thread waits rather than measuring through this one. Dropping it puts the
/// flag back where it was rather than clearing it, so a phase that arms inside
/// a measurement does not turn the measurement off on its way out.
///
/// A nested window re-baselines the high-water mark, which is what the phase
/// inside wants and means an outer window's own peak is only its own when
/// nothing inside it armed. Nothing reads an outer peak: a caller arms around
/// a whole measurement to put every part of it under the same allocator, not
/// to get a number out of it.
pub struct Armed {
    baseline: i64,
    was_armed: bool,
    was_nested: bool,
    installed: bool,
    /// `None` on a nested window, whose caller is holding the lock already.
    _lock: Option<MutexGuard<'static, ()>>,
}

impl Armed {
    /// The peak live heap since this window opened, over what was already live
    /// when it did.
    ///
    /// `None` when [`Counting`] is not this process's global allocator. Not
    /// `Some(0)`: zero is the best possible number on a lower-is-better
    /// column, and a binary that forgot the `#[global_allocator]` line would
    /// otherwise publish a win.
    pub fn peak_bytes(&self) -> Option<u64> {
        self.installed
            .then(|| (PEAK.load(Ordering::SeqCst) - self.baseline).max(0) as u64)
    }

    /// What is live right now, over the same baseline.
    ///
    /// The phase's retained heap rather than its high-water mark: what
    /// ingestion is still holding when the last tile is in is a different
    /// question from what it peaked at, and the writer's three
    /// distinct-payload tables are the former.
    ///
    /// Clamped at zero, because a phase that freed more than it allocated
    /// holds nothing rather than a negative number of bytes.
    pub fn live_bytes(&self) -> Option<u64> {
        self.installed
            .then(|| (IN_FLIGHT.load(Ordering::SeqCst) - self.baseline).max(0) as u64)
    }

    /// Whether the counting allocator is really in the allocation path.
    pub fn installed(&self) -> bool {
        self.installed
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        ARMED.store(self.was_armed, Ordering::SeqCst);
        NESTED.with(|flag| flag.set(self.was_nested));
    }
}

/// Start counting, and prove the counter is in the path before believing
/// anything it says.
///
/// The probe is the positive control. Without it an uninstalled allocator and
/// a phase that allocated nothing report the same thing, and one of those is a
/// measurement.
pub fn arm() -> Armed {
    let already_nested = NESTED.with(|flag| flag.get());
    let lock = if already_nested {
        None
    } else {
        // A poisoned lock means a measuring test panicked, which says nothing
        // about the counters: they are plain atomics and the panic cannot have
        // left them half written.
        Some(WINDOW.lock().unwrap_or_else(|e| e.into_inner()))
    };
    let was_nested = NESTED.with(|flag| flag.replace(true));
    let was_armed = ARMED.swap(true, Ordering::SeqCst);

    let probe = std::hint::black_box(vec![0u8; PROBE_BYTES]);
    let installed = INSTALLED.load(Ordering::SeqCst);
    drop(probe);

    // The baseline is taken after the probe, so what the probe held is not in
    // the window it was there to validate.
    let baseline = IN_FLIGHT.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    Armed {
        baseline,
        was_armed,
        was_nested,
        installed,
        _lock: lock,
    }
}
